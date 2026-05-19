# Build & test the Wormhole Global Accountant Solana program.
#
# The program is loaded by mollusk-svm in integration tests, so the SBF
# binary must exist before `cargo test` runs.

SBF_OUT_DIR := $(CURDIR)/target/deploy

# The mock-vaa feature gates the placeholder VAA verification path used by
# tests. Production cuts must NOT enable this — the real Verify VAA Shim CPI
# integration is tracked in a later slice. The non-mock branch in close_digest
# issues a compile_error! so a `cargo build` without `mock-vaa` will fail loudly.
.PHONY: build
build:
	cargo build-sbf --features bpf-entrypoint,mock-vaa

.PHONY: test
test: build
	SBF_OUT_DIR=$(SBF_OUT_DIR) cargo test --features mock-vaa

.PHONY: clippy
clippy:
	cargo clippy --all-targets --features mock-vaa -- -D warnings

.PHONY: clean
clean:
	cargo clean
