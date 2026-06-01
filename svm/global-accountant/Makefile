# Build & test the Wormhole Global Accountant Solana program.
#
# The program is loaded by mollusk-svm in integration tests, so the SBF
# binary must exist before `cargo test` runs.

SBF_OUT_DIR := $(CURDIR)/target/deploy

# Two build targets. Keep the split unambiguous:
#
#   * build-dev   — test/dev build with the mock-vaa close path and
#                   `test-only-open-digest` exposed. This is what `make test`
#                   depends on; the resulting `.so` is for mollusk and the
#                   surfpool spike only.
#   * build-prod  — production build with both features OFF. The default
#                   `close_digest` branch CPIs into the Verify VAA Shim
#                   (`EFaNWErqAtVWufdNb7yofSHHfWFos843DFpu4JBw24at`) and the
#                   `open_digest` direct entrypoint is not in the dispatch
#                   table. This is the shape that will ship to mainnet.
.PHONY: build-dev
build-dev:
	cargo build-sbf --features bpf-entrypoint,mock-vaa,test-only-open-digest,mock-noreplay

.PHONY: build-prod
build-prod:
	cargo build-sbf --features bpf-entrypoint

# `make build` defaults to the dev build. Explicit targets above are preferred
# in CI; this alias is a convenience for local iteration.
.PHONY: build
build: build-dev

.PHONY: test
test: build-dev
	SBF_OUT_DIR=$(SBF_OUT_DIR) cargo test --features mock-vaa,test-only-open-digest,mock-noreplay

# Surfpool E2E spike. Spawns a real `surfpool start` subprocess, deploys the
# dev `.so` via the `surfnet_writeProgram` cheatcode, and drives the
# DigestAccount lifecycle over JSON-RPC. Ignored by default (`#[ignore]`)
# because it requires the `surfpool` binary and is slower than mollusk.
#
# Requires `surfpool` on PATH or at ~/.local/bin/surfpool (install via
# `curl -sL https://run.surfpool.run/ | bash`).
.PHONY: test-e2e-spike
test-e2e-spike: build-dev
	SBF_OUT_DIR=$(SBF_OUT_DIR) cargo test --features mock-vaa,test-only-open-digest,mock-noreplay \
		--test surfpool_e2e_spike -- --ignored --nocapture

# Surfpool E2E test against a Solana mainnet fork. Same subprocess story as
# test-e2e-spike, but surfpool boots with `--rpc-url` pointing at mainnet so
# the Verify VAA Shim, the Core Bridge, and the active `GuardianSet` PDA
# lazy-fetch from upstream. The test deploys the production-shape `.so` (no
# `mock-vaa`) so the real Shim CPI is exercised end-to-end against a real
# historical VAA. The DigestAccount PDA the test closes is hand-crafted via
# the `surfnet_setAccount` cheatcode (the prod-shape build does not expose
# `open_digest`).
#
# Default datasource: the public RPC from
# `~/WormholeLabs/CoreTeam/w7-registry/chains/mainnet/solana.yaml`. surfpool
# v1.2.1 cannot inject the `Origin` header that the labsapis proxy requires,
# so the proxy is not the default — override with `GA_E2E_DATASOURCE_RPC=<url>`
# for an endpoint that does not enforce CORS.
#
# Build features: the test crate is configured with the workspace default of
# `mock-vaa,test-only-open-digest,mock-noreplay` enabled — these affect only the program
# binary, not the host-side test code, so the test compiles fine either way.
# The deployed `.so` comes from `build-prod`, i.e. *neither* feature on, so
# the real `close_digest` CPI is what executes inside the simnet.
.PHONY: test-e2e-mainnet-fork
test-e2e-mainnet-fork: build-prod
	SBF_OUT_DIR=$(SBF_OUT_DIR) cargo test --features mock-vaa,test-only-open-digest,mock-noreplay \
		--test surfpool_e2e_mainnet_fork -- --ignored --nocapture

# VAA-replay corpus. Replays a curated set of historical mainnet VAAs
# (~7 fixtures in `tests/fixtures/`) against the production-shape `.so`. Each
# test owns its own surfpool subprocess for isolation. Three corpus entries
# (GS1, GS4, GS5) target the historical guardian-set ceiling concern and are
# expected to fail at the Shim CPI — they assert the failure rather than
# success, so a Shim policy change will flip them and surface a regression.
#
# Same build/datasource story as test-e2e-mainnet-fork. The host-side test
# binary needs the mock-vaa feature combo to satisfy the workspace dev-dep
# self-reference; the deployed `.so` is the prod build with no features.
.PHONY: test-e2e-vaa-corpus
test-e2e-vaa-corpus: build-prod
	SBF_OUT_DIR=$(SBF_OUT_DIR) cargo test --features mock-vaa,test-only-open-digest,mock-noreplay \
		--test surfpool_e2e_vaa_corpus -- --ignored --nocapture

# noreplay co-deployment smoke test. Boots surfpool offline, deploys both
# `global_accountant.so` AND the upstream `solana_noreplay.so` at its canonical
# devnet program ID (`repMHgR5BEpGLeZvM5iGoNNDPw4eu2BS6sXJzaC8K4t`), then
# drives `CreateBitmap` + `MarkUsed` directly against noreplay to confirm the
# wire format and account layout match `solana-noreplay`'s on-chain
# expectations. The production `submit_observations` path replaces the
# Keypair-signed authority used here with a PDA owned by global-accountant
# invoked via `invoke_signed`.
#
# NOT included in the unified `test-e2e` aggregator below: this target
# exercises a one-off deployment invariant rather than a regression-prone code
# path inside our own program. Adding it to every CI run would burn ~10 seconds
# on a property that does not depend on our diff. Re-enable here if it becomes
# a true regression guard.
NOREPLAY_SO := $(HOME)/WormholeLabs/CoreTeam/solana-noreplay/target/deploy/solana_noreplay.so

# Surfpool e2e for `submit_observations` driven against the real
# `solana-noreplay` program. Deploys both `global_accountant.so` (production
# shape — no mock-noreplay) and `solana_noreplay.so`, drives 13 distinct
# guardian observations end-to-end, and asserts the bitmap bit gets set in the
# real noreplay PDA. A 14th submission must fail with `AlreadyAccounted` via
# the direct-read pre-check.
#
# Build target is `build-prod` (no mock features) so the real CPI is exercised.
# The host-side test binary still needs the mock features enabled in its
# workspace dev-dep self-reference to satisfy crate-feature unification — they
# affect only the on-chain program binary, not the test harness.
.PHONY: test-e2e-submit-obs
test-e2e-submit-obs: build-prod
	@test -f "$(NOREPLAY_SO)" || { \
		echo "ERROR: $(NOREPLAY_SO) not found."; \
		echo "Rebuild via: cd ~/WormholeLabs/CoreTeam/solana-noreplay && just build"; \
		exit 1; \
	}
	SBF_OUT_DIR=$(SBF_OUT_DIR) cargo test --features mock-vaa,test-only-open-digest,mock-noreplay \
		--test surfpool_e2e_submit_observations_real_noreplay -- --ignored --nocapture

# Surfpool e2e for `submit_vaas` driven against the real Verify VAA Shim and
# real `solana-noreplay` programs. Deploys both `global_accountant.so`
# (production shape) and `solana_noreplay.so`, pre-posts the signatures of a
# historical Token Bridge transfer VAA via the Shim's `PostSignatures`, and
# calls `submit_vaas` to apply the transfer.
#
# Build target is `build-prod` so the real Shim CPI is exercised. The
# host-side test binary still needs the mock features enabled in its
# workspace dev-dep self-reference; they affect only the on-chain `.so`.
.PHONY: test-e2e-submit-vaas
test-e2e-submit-vaas: build-prod
	@test -f "$(NOREPLAY_SO)" || { \
		echo "ERROR: $(NOREPLAY_SO) not found."; \
		echo "Rebuild via: cd ~/WormholeLabs/CoreTeam/solana-noreplay && just build"; \
		exit 1; \
	}
	SBF_OUT_DIR=$(SBF_OUT_DIR) cargo test --features mock-vaa,test-only-open-digest,mock-noreplay \
		--test surfpool_e2e_submit_vaas -- --ignored --nocapture

.PHONY: test-e2e-noreplay-smoke
test-e2e-noreplay-smoke: build-dev
	@test -f "$(NOREPLAY_SO)" || { \
		echo "ERROR: $(NOREPLAY_SO) not found."; \
		echo "Rebuild via: cd ~/WormholeLabs/CoreTeam/solana-noreplay && just build"; \
		echo "(runs: cargo build-sbf --manifest-path program/Cargo.toml --no-default-features)"; \
		exit 1; \
	}
	@# Stale `.so` builds (older than the noreplay source tree itself) have
	@# historically produced spurious "missing required signature" failures
	@# inside the program's signer check — likely due to a pinocchio
	@# AccountView wire-format skew between the .so and surfpool's loader.
	@# If you see that error after upgrading either dep, rebuild the .so from
	@# `~/WormholeLabs/CoreTeam/solana-noreplay` first.
	SBF_OUT_DIR=$(SBF_OUT_DIR) cargo test --features mock-vaa,test-only-open-digest,mock-noreplay \
		--test surfpool_e2e_noreplay_smoke -- --ignored --nocapture

# Unified surfpool E2E target. Runs the spike + mainnet-fork test + VAA-replay
# corpus + the real-noreplay submit_observations / submit_vaas e2e tests. CI
# uses this target so every layer is exercised in one job with a single warm
# cargo cache and a single warm surfpool mainnet-account cache.
#
# Sequencing rationale: the three test binaries cannot share a single
# `target/deploy/global_accountant.so` because they need different builds.
# The spike loads the dev `.so` (`build-dev`, with `mock-vaa` + the direct
# `open_digest` entrypoint); the mainnet-fork test and the VAA-replay corpus
# load the production-shape `.so` (`build-prod`, real Verify VAA Shim CPI,
# no `open_digest` direct entrypoint). `cargo build-sbf` writes both
# variants to the same path, so the dev runs must complete before the prod
# `.so` overwrites it.
#
# Each test boots its own surfpool subprocess for PDA isolation and per-test
# failure scoping (see the corpus header for details). Test-level
# parallelism is therefore the right knob: too much concurrency starves the
# public mainnet RPC and multiplies port-allocation races. `--test-threads=1`
# was previously required for port allocation under load; we honour that
# here to keep CI deterministic. Bump locally with `RUST_TEST_THREADS=N` if
# your network can take more.
.PHONY: test-e2e
test-e2e: test-e2e-spike test-e2e-mainnet-fork test-e2e-vaa-corpus test-e2e-submit-obs test-e2e-submit-vaas
	@echo "All surfpool E2E layers green."

# Compute-unit spike for the `Uint256` type. Builds the minimal Pinocchio
# program in `programs/uint256-cu-spike/` and runs the `#[ignore]`-gated tests
# that log `compute_units_consumed`. Not part of the default `cargo test`
# set — re-run when the underlying 256-bit math crate changes (e.g. swap from
# `ruint` to a vendored Orca implementation) to confirm the CU baseline does
# not regress.
.PHONY: test-cu-spike
test-cu-spike:
	cargo build-sbf --manifest-path programs/uint256-cu-spike/Cargo.toml \
		--features bpf-entrypoint
	SBF_OUT_DIR=$(SBF_OUT_DIR) cargo test -p uint256-cu-spike --test cu_spike \
		-- --ignored --nocapture

.PHONY: clippy
clippy:
	cargo clippy --all-targets --features mock-vaa,test-only-open-digest,mock-noreplay -- -D warnings

# Production-shape clippy. Mirrors `build-prod` — no test features enabled,
# so this catches lint regressions that the default `clippy` target would
# miss because it always exercises the dev feature combo. CI should run both
# targets; a regression that only trips here means the production build is
# affected.
.PHONY: clippy-prod
clippy-prod:
	cargo clippy -p global-accountant -- -D warnings
	cargo clippy -p global-accountant-definitions -- -D warnings

.PHONY: clean
clean:
	cargo clean
