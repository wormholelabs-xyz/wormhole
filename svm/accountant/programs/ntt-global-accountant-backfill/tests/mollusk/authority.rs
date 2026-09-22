//! Authority isolation between the two backfill artifacts. Each `.so` pins its own operator
//! key at compile time, so one migration's key cannot drive the other's program.

use accountant_operational_core::accounts::balance;
use global_accountant_definitions::{GlobalAccountantError, Uint256};
use mollusk_svm::program::keyed_account_for_system_program;
use mollusk_svm::Mollusk;
use solana_instruction::{AccountMeta, Instruction as SolanaInstruction};
use solana_pubkey::Pubkey;

use crate::common::*;

const ETHEREUM: u16 = 2;
const TOKEN_ADDRESS: [u8; 32] = [0x11u8; 32];

/// The WTT backfill `.so` at its own `declare_id!` address.
fn wtt_program_id() -> Pubkey {
    Pubkey::new_from_array(global_accountant_backfill::ID.to_bytes())
}

fn wtt_mollusk() -> Mollusk {
    mollusk_with_fixtures(&wtt_program_id(), "global_accountant_backfill")
}

/// A one-entry `BackfillBalance` signed by `signer`, discriminator 1 in both programs.
fn probe(
    mollusk: &Mollusk,
    program: Pubkey,
    signer: Pubkey,
) -> mollusk_svm::result::InstructionResult {
    let entry = wire::balance_entry(ETHEREUM, ETHEREUM, TOKEN_ADDRESS, Uint256::from_u128(1).0);
    let pda = balance::derive_pda(&program, ETHEREUM, ETHEREUM, &TOKEN_ADDRESS).0;
    let (system_program, system_account) = keyed_account_for_system_program();

    let ix = SolanaInstruction::new_with_bytes(
        program,
        &wire::encode_balance_batch(1, &[entry]),
        vec![
            AccountMeta::new(signer, true),
            AccountMeta::new_readonly(system_program, false),
            AccountMeta::new(pda, false),
        ],
    );
    let accounts = vec![
        (signer, system_owned_account(10_000_000_000)),
        (system_program, system_account),
        (pda, uninitialised_pda_account()),
    ];
    mollusk.process_instruction(&ix, &accounts)
}

#[test]
fn each_artifact_accepts_only_its_own_operator_key() {
    let ntt = mollusk();
    let wtt = wtt_mollusk();
    assert_ne!(program_id(), wtt_program_id(), "program ids");
    assert_ne!(
        ntt_test_authority_pubkey(),
        test_authority_pubkey(),
        "test keys"
    );

    // (label, mollusk, program id, signer, expected error)
    let cases: [(&str, &Mollusk, Pubkey, Pubkey, Option<u64>); 4] = [
        (
            "NTT key on the NTT program",
            &ntt,
            program_id(),
            ntt_test_authority_pubkey(),
            None,
        ),
        (
            "WTT key on the WTT program",
            &wtt,
            wtt_program_id(),
            test_authority_pubkey(),
            None,
        ),
        (
            "WTT key on the NTT program",
            &ntt,
            program_id(),
            test_authority_pubkey(),
            Some(GlobalAccountantError::UnauthorizedCaller as u64),
        ),
        (
            "NTT key on the WTT program",
            &wtt,
            wtt_program_id(),
            ntt_test_authority_pubkey(),
            Some(GlobalAccountantError::UnauthorizedCaller as u64),
        ),
    ];

    for (label, mollusk, program, signer, expected) in cases {
        let result = probe(mollusk, program, signer);
        match expected {
            None => assert_success(&result, label),
            Some(code) => assert_error(&result, code, label),
        }
    }
}
