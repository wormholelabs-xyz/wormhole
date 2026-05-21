//! Compute-unit baseline for `Uint256::checked_add` / `checked_sub`.
//!
//! Gated `#[ignore]` so the default `cargo test` set stays fast. Run via:
//!
//! ```sh
//! # from svm/global-accountant/
//! cargo build-sbf --manifest-path programs/uint256-cu-spike/Cargo.toml \
//!     --features bpf-entrypoint
//! SBF_OUT_DIR=$(pwd)/target/deploy \
//!     cargo test -p uint256-cu-spike -- --ignored --nocapture
//! ```
//!
//! The test logs `compute_units_consumed` so the value can be lifted into the
//! migration plan §13. A regression on either crate (`ruint`, or a future
//! switch to Orca's u256) flips the number and gets caught in code review.

use mollusk_svm::Mollusk;
use solana_instruction::Instruction;
use solana_pubkey::Pubkey;

fn program_id() -> Pubkey {
    Pubkey::new_from_array([8u8; 32])
}

fn mollusk() -> Mollusk {
    Mollusk::new(&program_id(), "uint256_cu_spike")
}

fn build_ix_data(a: [u8; 32], b: [u8; 32], op: u8) -> Vec<u8> {
    let mut data = Vec::with_capacity(32 + 32 + 1);
    data.extend_from_slice(&a);
    data.extend_from_slice(&b);
    data.push(op);
    data
}

/// Add path: 500 + 200 (both fit in the low u128 half). Mirrors the
/// `native_lock` / `wrapped_mint` CosmWasm test cases.
#[test]
#[ignore = "requires `cargo build-sbf` for uint256-cu-spike first"]
fn cu_spike_checked_add_no_overflow() {
    let mollusk = mollusk();

    // 500 in big-endian bytes is `0x01f4` in the low two bytes.
    let mut a = [0u8; 32];
    a[30] = 0x01;
    a[31] = 0xf4;
    let mut b = [0u8; 32];
    b[31] = 200;
    let data = build_ix_data(a, b, /* op = add */ 0);

    let ix = Instruction::new_with_bytes(program_id(), &data, vec![]);
    let result = mollusk.process_instruction(&ix, &[]);

    eprintln!(
        "uint256-cu-spike: checked_add(500, 200) consumed {} CU",
        result.compute_units_consumed
    );
    // No assertion on the CU number — we report it for the migration plan.
    // Assert only that the program ran to completion.
    assert!(
        matches!(result.program_result, mollusk_svm::result::ProgramResult::Success),
        "spike program failed: {:?}",
        result.program_result
    );
}

/// Subtract path: 500 - 200. Mirrors `native_unlock` / `wrapped_burn`.
#[test]
#[ignore = "requires `cargo build-sbf` for uint256-cu-spike first"]
fn cu_spike_checked_sub_no_underflow() {
    let mollusk = mollusk();

    let mut a = [0u8; 32];
    a[30] = 0x01;
    a[31] = 0xf4;
    let mut b = [0u8; 32];
    b[31] = 200;
    let data = build_ix_data(a, b, /* op = sub */ 1);

    let ix = Instruction::new_with_bytes(program_id(), &data, vec![]);
    let result = mollusk.process_instruction(&ix, &[]);

    eprintln!(
        "uint256-cu-spike: checked_sub(500, 200) consumed {} CU",
        result.compute_units_consumed
    );
    assert!(
        matches!(result.program_result, mollusk_svm::result::ProgramResult::Success),
        "spike program failed: {:?}",
        result.program_result
    );
}

/// Stress path: full 256-bit operands to confirm CU does not balloon on a
/// non-trivial operand width.
#[test]
#[ignore = "requires `cargo build-sbf` for uint256-cu-spike first"]
fn cu_spike_checked_add_full_width_operands() {
    let mollusk = mollusk();

    let mut a = [0u8; 32];
    for (i, byte) in a.iter_mut().enumerate() {
        *byte = 0x80 - (i as u8 / 4);
    }
    let mut b = [0u8; 32];
    for (i, byte) in b.iter_mut().enumerate() {
        *byte = 0x10 + (i as u8 / 4);
    }
    let data = build_ix_data(a, b, /* op = add */ 0);

    let ix = Instruction::new_with_bytes(program_id(), &data, vec![]);
    let result = mollusk.process_instruction(&ix, &[]);

    eprintln!(
        "uint256-cu-spike: checked_add(full-width) consumed {} CU",
        result.compute_units_consumed
    );
    assert!(
        matches!(result.program_result, mollusk_svm::result::ProgramResult::Success),
        "spike program failed: {:?}",
        result.program_result
    );
}
