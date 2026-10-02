//! Generates the wire layouts, constants and test fixtures of `node/pkg/accountant` from the
//! program types and code paths.
//!
//! `just go-codegen` writes the files. `just test` checks them. Both need `gofmt` on PATH.

mod global_accountant;
mod go;

/// Repository root is four levels above this crate.
const GO_PACKAGE_DIR: &str = concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/../../../../node/pkg/accountant"
);
const LAYOUT_FILE: &str = "solana_layout_gen.go";
const FIXTURES_FILE: &str = "solana_layout_fixtures_gen_test.go";

/// `(file name, contents)` of every generated file.
fn files() -> [(&'static str, String); 2] {
    [
        (LAYOUT_FILE, global_accountant::layout_file()),
        (FIXTURES_FILE, global_accountant::fixtures_file()),
    ]
}

fn main() {
    let dir = std::path::PathBuf::from(GO_PACKAGE_DIR);
    for (name, contents) in files() {
        let path = dir.join(name);
        std::fs::write(&path, contents)
            .unwrap_or_else(|err| panic!("write {}: {err}", path.display()));
    }
}

#[cfg(test)]
mod tests {
    use std::path::{Path, PathBuf};

    use super::{files, GO_PACKAGE_DIR};

    /// Describes the first differing line. Hex lines are long, so each side is truncated.
    fn first_difference(path: &Path, checked_in: &str, generated: &str) -> String {
        const SHOWN: usize = 120;
        let old: Vec<&str> = checked_in.split('\n').collect();
        let new: Vec<&str> = generated.split('\n').collect();
        let show = |side: Option<&&str>| match side {
            Some(text) => text.chars().take(SHOWN).collect::<String>(),
            None => "<end of file>".to_owned(),
        };
        for i in 0..old.len().max(new.len()) {
            if old.get(i) != new.get(i) {
                return format!(
                    "{} differs from the program at line {}.\n  checked in: {}\n  generated:  {}\nRun `just go-codegen`.",
                    path.display(),
                    i + 1,
                    show(old.get(i)),
                    show(new.get(i)),
                );
            }
        }
        unreachable!("the files differ, so one line differs");
    }

    #[test]
    fn go_files_match_program() {
        let dir = PathBuf::from(GO_PACKAGE_DIR);
        for (name, generated) in files() {
            let path = dir.join(name);
            let checked_in = std::fs::read_to_string(&path).unwrap_or_else(|err| {
                panic!("read {}: {err}. Run `just go-codegen`.", path.display())
            });
            // Exact bytes: a line-ending change is drift too.
            if checked_in != generated {
                panic!("{}", first_difference(&path, &checked_in, &generated));
            }
        }
    }
}
