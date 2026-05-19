# Build & test the Wormhole Global Accountant Solana program.
#
# The program is loaded by mollusk-svm in integration tests, so the SBF
# binary must exist before `cargo test` runs.

SBF_OUT_DIR := $(CURDIR)/target/deploy

# Two build targets. Keep the split unambiguous:
#
#   * build-dev   — test/dev build with the mock-vaa close path and
#                   `test-only-open-digest` exposed. This is what `make test`
#                   depends on; the resulting `.so` is for mollusk only.
#   * build-prod  — production build with both features OFF. Today this
#                   target is *expected to fail* at the `compile_error!` in
#                   `close_digest.rs` because the real Verify VAA Shim CPI is
#                   not yet wired in. Successful completion will only be
#                   possible after that integration lands; until then this
#                   target acts as a tripwire — if it ever compiles silently,
#                   something has stripped the gate.
.PHONY: build-dev
build-dev:
	cargo build-sbf --features bpf-entrypoint,mock-vaa,test-only-open-digest

.PHONY: build-prod
build-prod:
	cargo build-sbf --features bpf-entrypoint

# `make build` defaults to the dev build. Explicit targets above are preferred
# in CI; this alias is a convenience for local iteration.
.PHONY: build
build: build-dev

.PHONY: test
test: build-dev
	SBF_OUT_DIR=$(SBF_OUT_DIR) cargo test --features mock-vaa,test-only-open-digest

.PHONY: clippy
clippy:
	cargo clippy --all-targets --features mock-vaa,test-only-open-digest -- -D warnings

.PHONY: clean
clean:
	cargo clean
