//! Unit tests for the submitter's error classifier.
//!
//! Integration tests against a real validator land in Phase 9
//! (`tests/e2e_against_surfpool.rs`) — they're heavier and require surfpool
//! deployment. These tests cover the pure parsing/classification logic.

use ga_backfill::submitter::{
    extract_custom_program_error, ALREADY_ACCOUNTED_CUSTOM, UNAUTHORIZED_CALLER_CUSTOM,
};

#[test]
fn extracts_already_accounted_hex_code() {
    // ALREADY_ACCOUNTED = 7 → 0x7
    let msg = "RpcError(\"Transaction simulation failed: Error processing Instruction 0: custom program error: 0x7\")";
    assert_eq!(extract_custom_program_error(msg), Some(7));
    assert_eq!(extract_custom_program_error(msg), Some(ALREADY_ACCOUNTED_CUSTOM));
}

#[test]
fn extracts_unauthorized_caller_hex_code() {
    // UNAUTHORIZED_CALLER = 3 → 0x3
    let msg = "Transaction simulation failed: Error processing Instruction 0: custom program error: 0x3";
    assert_eq!(extract_custom_program_error(msg), Some(3));
    assert_eq!(extract_custom_program_error(msg), Some(UNAUTHORIZED_CALLER_CUSTOM));
}

#[test]
fn extracts_arbitrary_hex_codes() {
    assert_eq!(
        extract_custom_program_error("custom program error: 0x1"),
        Some(1)
    );
    assert_eq!(
        extract_custom_program_error("custom program error: 0x1a"),
        Some(26)
    );
    assert_eq!(
        extract_custom_program_error("custom program error: 0xff"),
        Some(255)
    );
}

#[test]
fn returns_none_for_non_program_errors() {
    assert_eq!(
        extract_custom_program_error("transport error: connection refused"),
        None
    );
    assert_eq!(extract_custom_program_error(""), None);
    assert_eq!(extract_custom_program_error("blockhash not found"), None);
}

#[test]
fn stops_at_non_hex_character() {
    // Hex parsing stops at the first non-hex char.
    let msg = "custom program error: 0x7; logs: [...]";
    assert_eq!(extract_custom_program_error(msg), Some(7));
}

#[test]
fn handles_multi_digit_codes_in_context() {
    // Real Solana RPC error wrapping with surrounding context.
    let msg = r#"RpcResponseError { code: -32002, message: "Transaction simulation failed: Error processing Instruction 0: custom program error: 0x1d", data: ... }"#;
    assert_eq!(extract_custom_program_error(msg), Some(0x1d));
}
