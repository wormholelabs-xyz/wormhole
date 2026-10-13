//! `svm-accountant.toml`: the environment both tools run in. Per-run
//! decisions (the accountant, the parity mode) stay on the command line, and
//! program addresses come from the build.

use std::fs::File;
use std::io::Read;
use std::path::{Path, PathBuf};

use anyhow::{bail, ensure, Context, Result};
use serde::Deserialize;
use sha2::{Digest, Sha256};

use crate::accountant::Accountant;

/// Default config path, relative to the working directory.
pub const DEFAULT_CONFIG_PATH: &str = "svm-accountant.toml";

/// Largest config file read.
pub const MAX_CONFIG_BYTES: u64 = 64 * 1024;

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ToolsConfig {
    /// JSON-RPC endpoint.
    pub rpc_url: String,
    /// `Origin` header for every RPC request; the internal proxy requires one.
    pub origin: Option<String>,
    #[serde(default)]
    pub backfill: BackfillSettings,
    pub wtt: Option<AccountantConfig>,
    pub ntt: Option<AccountantConfig>,
}

/// Submission settings of `svm-accountant-backfill run`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields, default)]
pub struct BackfillSettings {
    /// In-flight transactions, and concurrent account reads in the resume check.
    pub concurrency: usize,
    pub max_retries: u32,
    pub initial_retry_delay_ms: u64,
    pub max_retry_delay_ms: u64,
}

impl Default for BackfillSettings {
    fn default() -> Self {
        Self {
            concurrency: 16,
            max_retries: 5,
            initial_retry_delay_ms: 500,
            max_retry_delay_ms: 30_000,
        }
    }
}

/// Files of one accountant. Relative paths resolve against the config file directory.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AccountantConfig {
    /// Snapshot JSONL from the wormchain snapshot tool.
    pub snapshot: PathBuf,
    /// Parity ledger. `check` appends to it when set; `report` requires it.
    pub ledger: Option<PathBuf>,
    /// Backfill progress file. Defaults to `progress.json` next to the snapshot.
    pub progress_file: Option<PathBuf>,
    /// Backfill payer keypair. `run` requires it.
    pub payer: Option<PathBuf>,
}

/// A parsed config and the identity of the file it came from.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LoadedConfig {
    pub path: PathBuf,
    /// SHA-256 of the file bytes. The parity ledger records it.
    pub sha256: [u8; 32],
    pub config: ToolsConfig,
}

impl LoadedConfig {
    /// Read, parse and validate the file at `path`.
    pub fn load(path: &Path) -> Result<Self> {
        let file = File::open(path).with_context(|| format!("open config {}", path.display()))?;
        let mut bytes = Vec::new();
        file.take(MAX_CONFIG_BYTES + 1)
            .read_to_end(&mut bytes)
            .with_context(|| format!("read config {}", path.display()))?;
        ensure!(
            bytes.len() as u64 <= MAX_CONFIG_BYTES,
            "config {} exceeds MAX_CONFIG_BYTES ({MAX_CONFIG_BYTES})",
            path.display()
        );
        let text = std::str::from_utf8(&bytes)
            .with_context(|| format!("config {} is not UTF-8", path.display()))?;
        let mut config: ToolsConfig =
            toml::from_str(text).with_context(|| format!("parse config {}", path.display()))?;
        config.validate()?;
        let base = path.parent().unwrap_or(Path::new(""));
        for section in [&mut config.wtt, &mut config.ntt].into_iter().flatten() {
            section.resolve_against(base);
        }
        Ok(Self {
            path: path.to_path_buf(),
            sha256: Sha256::digest(&bytes).into(),
            config,
        })
    }

    /// The table of `accountant`. Fails when the file has none.
    pub fn accountant(&self, accountant: Accountant) -> Result<&AccountantConfig> {
        let section = match accountant {
            Accountant::Wtt => &self.config.wtt,
            Accountant::Ntt => &self.config.ntt,
        };
        match section {
            Some(section) => Ok(section),
            None => bail!("config {} has no [{accountant}] table", self.path.display()),
        }
    }
}

impl ToolsConfig {
    fn validate(&self) -> Result<()> {
        ensure!(
            self.rpc_url.starts_with("http://") || self.rpc_url.starts_with("https://"),
            "rpc_url `{}` is not an http(s) URL",
            self.rpc_url
        );
        if let Some(origin) = &self.origin {
            ensure!(
                origin.starts_with("http://") || origin.starts_with("https://"),
                "origin `{origin}` is not an http(s) origin"
            );
        }
        let backfill = &self.backfill;
        ensure!(
            (1..=256).contains(&backfill.concurrency),
            "backfill.concurrency {} is outside 1..=256",
            backfill.concurrency
        );
        ensure!(
            backfill.max_retries >= 1,
            "backfill.max_retries must be at least 1"
        );
        ensure!(
            backfill.initial_retry_delay_ms <= backfill.max_retry_delay_ms,
            "backfill.initial_retry_delay_ms {} exceeds backfill.max_retry_delay_ms {}",
            backfill.initial_retry_delay_ms,
            backfill.max_retry_delay_ms
        );
        ensure!(
            self.wtt.is_some() || self.ntt.is_some(),
            "config needs a [wtt] or an [ntt] table"
        );
        Ok(())
    }
}

impl AccountantConfig {
    fn resolve_against(&mut self, base: &Path) {
        let resolve = |path: &mut PathBuf| {
            if path.is_relative() {
                *path = base.join(&*path);
            }
        };
        resolve(&mut self.snapshot);
        for path in [&mut self.ledger, &mut self.progress_file, &mut self.payer]
            .into_iter()
            .flatten()
        {
            resolve(path);
        }
    }

    /// Progress file path: the configured one, or `progress.json` next to the snapshot.
    pub fn progress_file_path(&self) -> PathBuf {
        self.progress_file
            .clone()
            .unwrap_or_else(|| self.snapshot.with_file_name("progress.json"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const VALID: &str = r#"
rpc_url = "https://rpc.labsapis.com/mainnet/solana"
origin = "https://portalbridge.com"

[backfill]
concurrency = 8

[wtt]
snapshot = "snapshots/wtt.jsonl"
ledger = "/var/ledgers/wtt.jsonl"
payer = "keys/wtt.json"
"#;

    fn write(text: &str) -> (tempfile::TempDir, PathBuf) {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("svm-accountant.toml");
        std::fs::write(&path, text).unwrap();
        (dir, path)
    }

    #[test]
    fn loads_a_valid_file_and_resolves_relative_paths() {
        let (dir, path) = write(VALID);
        let loaded = LoadedConfig::load(&path).unwrap();
        let config = &loaded.config;
        assert_eq!(config.origin.as_deref(), Some("https://portalbridge.com"));
        assert_eq!(config.backfill.concurrency, 8);
        assert_eq!(config.backfill.max_retries, 5, "unset fields keep defaults");

        let wtt = loaded.accountant(Accountant::Wtt).unwrap();
        assert_eq!(wtt.snapshot, dir.path().join("snapshots/wtt.jsonl"));
        assert_eq!(
            wtt.ledger.as_deref(),
            Some(Path::new("/var/ledgers/wtt.jsonl"))
        );
        assert_eq!(
            wtt.payer.as_deref(),
            Some(dir.path().join("keys/wtt.json").as_path())
        );
        assert_eq!(
            wtt.progress_file_path(),
            dir.path().join("snapshots/progress.json")
        );
        assert_eq!(
            loaded.sha256,
            <[u8; 32]>::from(Sha256::digest(VALID.as_bytes()))
        );
    }

    #[test]
    fn rejects_invalid_files() {
        let cases: [(&str, String, &str); 7] = [
            (
                "unknown top-level key",
                format!("{VALID}\nrpc_ur = \"x\"\n"),
                "unknown field",
            ),
            (
                "unknown key in a table",
                VALID.replace("payer =", "snapshto = \"a\"\npayer ="),
                "unknown field",
            ),
            (
                "rpc_url not http",
                VALID.replace("https://rpc.labsapis.com", "ftp://rpc"),
                "not an http(s) URL",
            ),
            (
                "zero concurrency",
                VALID.replace("concurrency = 8", "concurrency = 0"),
                "outside 1..=256",
            ),
            (
                "no accountant table",
                "rpc_url = \"http://127.0.0.1:8899\"\n".to_string(),
                "needs a [wtt] or an [ntt] table",
            ),
            (
                "missing snapshot",
                VALID.replace("snapshot = \"snapshots/wtt.jsonl\"\n", ""),
                "missing field `snapshot`",
            ),
            (
                "over the size limit",
                format!("{VALID}\n#{}\n", "x".repeat(MAX_CONFIG_BYTES as usize)),
                "exceeds MAX_CONFIG_BYTES",
            ),
        ];
        for (name, text, want) in cases {
            let (_dir, path) = write(&text);
            let error = format!("{:#}", LoadedConfig::load(&path).unwrap_err());
            assert!(error.contains(want), "{name}: {error}");
        }
    }

    #[test]
    fn the_example_file_loads() {
        let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("../svm-accountant.example.toml");
        let loaded = LoadedConfig::load(&path).unwrap();
        assert_eq!(loaded.config.backfill, BackfillSettings::default());
        assert!(loaded.accountant(Accountant::Wtt).unwrap().payer.is_some());
        assert!(loaded.accountant(Accountant::Ntt).unwrap().ledger.is_some());
    }

    #[test]
    fn a_missing_accountant_table_fails_on_use() {
        let (_dir, path) = write(VALID);
        let loaded = LoadedConfig::load(&path).unwrap();
        let error = loaded.accountant(Accountant::Ntt).unwrap_err().to_string();
        assert!(error.contains("no [ntt] table"), "{error}");
    }
}
