//! Surfpool E2E for `submit_vaas` against the real Verify VAA Shim and `solana-noreplay`.
//! Deploys the `.so`, posts the VAA signatures through the Shim, and submits a real
//! Token Bridge transfer VAA. Asserts the NoReplay bit and both balance debits.
//!
//! # Run
//!
//! ```sh
//! just test-e2e-submit-vaas
//! ```
//!
//! `#[ignore]`: spawns surfpool. Needs `solana_noreplay.so` and a current `global_accountant.so`.
//!
//! Fixture `mainnet_solana_token_bridge_transfer_seq1395207.vaa`: Solana to Ethereum transfer
//! of an Ethereum-native ERC-20. Both sides debit, so both balance PDAs are pre-seeded.

#![allow(clippy::too_many_arguments)]

use std::time::{Duration, Instant};

use global_accountant_definitions::{
    BalanceAccountLayout, ChainRegistrationLayout, Instruction as IxDiscriminator, Uint256,
    ACCOUNT_SEED_PREFIX, CHAIN_REGISTRATION_SEED_PREFIX, NOREPLAY_AUTHORITY_SEED_PREFIX,
    VERIFY_VAA_SHIM_PROGRAM_ID,
};
use solana_commitment_config::CommitmentConfig;
use solana_instruction::{AccountMeta, Instruction};
use solana_keypair::Keypair;
use solana_pubkey::Pubkey;
use solana_signer::Signer;
use solana_system_interface::program as system_program;
use solana_transaction::Transaction;

mod common;
use common::{
    assert_canonical_log_in_tx, await_confirmed, deploy_program, derive_noreplay_bitmap_pda,
    hex_encode, load_vaa_fixture, noreplay_so_path, rpc_call, so_path, start_surfpool, ParsedVaa,
    SurfpoolOptions, NOREPLAY_PROGRAM_ID,
};

/// Core Bridge program ID on Solana mainnet.
const CORE_BRIDGE_PROGRAM_ID: Pubkey = Pubkey::new_from_array([
    0x0e, 0x0a, 0x58, 0x9a, 0x41, 0xa5, 0x5f, 0xbd, 0x66, 0xc5, 0x2a, 0x47, 0x5f, 0x2d, 0x92, 0xa6,
    0xd3, 0xdc, 0x9b, 0x47, 0x47, 0x11, 0x4c, 0xb9, 0xaf, 0x82, 0x5a, 0x98, 0xb5, 0x45, 0xd3, 0xce,
]);

/// `post_signatures` discriminator (`sha256("global:post_signatures")[..8]`).
const POST_SIGNATURES_SELECTOR: [u8; 8] = [0x8a, 0x02, 0x35, 0xa6, 0x2d, 0x4d, 0x89, 0x33];

const COMPUTE_BUDGET_PROGRAM_ID: Pubkey = Pubkey::new_from_array([
    0x03, 0x06, 0x46, 0x6f, 0xe5, 0x21, 0x17, 0x32, 0xff, 0xec, 0xad, 0xba, 0x72, 0xc3, 0x9b, 0xe7,
    0xbc, 0x8c, 0xe5, 0xbb, 0xc5, 0xf7, 0x12, 0x6b, 0x2c, 0x43, 0x9b, 0x3a, 0x40, 0x00, 0x00, 0x00,
]);

/// CU ceiling for `submit_vaas` (VerifyHash ~200k CU plus CPIs and lazy init).
const SUBMIT_VAAS_CU_LIMIT: u32 = 400_000;

/// Lamports for a cheatcode-seeded `BalanceAccountLayout` PDA; above the rent minimum.
const BALANCE_PDA_RENT_LAMPORTS: u64 = 1_169_280;

/// Surfpool mainnet fork datasource; `GA_E2E_DATASOURCE_RPC` overrides.
fn datasource_rpc_url() -> String {
    std::env::var("GA_E2E_DATASOURCE_RPC")
        .unwrap_or_else(|_| "https://api.mainnet-beta.solana.com".to_string())
}

fn derive_guardian_set_pda(index: u32) -> (Pubkey, u8) {
    let idx_be = index.to_be_bytes();
    Pubkey::find_program_address(&[b"GuardianSet", &idx_be], &CORE_BRIDGE_PROGRAM_ID)
}

fn derive_account_pda(
    program_id: &Pubkey,
    chain: u16,
    token_chain: u16,
    token_address: &[u8; 32],
) -> (Pubkey, u8) {
    let chain_be = chain.to_be_bytes();
    let token_chain_be = token_chain.to_be_bytes();
    Pubkey::find_program_address(
        &[
            ACCOUNT_SEED_PREFIX,
            &chain_be,
            &token_chain_be,
            token_address,
        ],
        program_id,
    )
}

fn derive_noreplay_authority_pda(program_id: &Pubkey) -> (Pubkey, u8) {
    Pubkey::find_program_address(&[NOREPLAY_AUTHORITY_SEED_PREFIX], program_id)
}

fn derive_chain_registration_pda(program_id: &Pubkey, chain: u16) -> (Pubkey, u8) {
    let chain_be = chain.to_be_bytes();
    Pubkey::find_program_address(&[CHAIN_REGISTRATION_SEED_PREFIX, &chain_be], program_id)
}

fn build_namespace(chain: u16, emitter: &[u8; 32]) -> [u8; 34] {
    let mut ns = [0u8; 34];
    ns[..2].copy_from_slice(&chain.to_be_bytes());
    ns[2..].copy_from_slice(emitter);
    ns
}

fn set_compute_unit_limit_ix(units: u32) -> Instruction {
    let mut data = Vec::with_capacity(5);
    data.push(0x02);
    data.extend_from_slice(&units.to_le_bytes());
    Instruction {
        program_id: COMPUTE_BUDGET_PROGRAM_ID,
        accounts: vec![],
        data,
    }
}

fn submit_vaas_ix_data(guardian_set_bump: u8, body: &[u8]) -> Vec<u8> {
    // [disc: u8][guardian_set_bump: u8][body_len: u16 LE][body]
    let mut data = Vec::with_capacity(1 + 1 + 2 + body.len());
    data.push(IxDiscriminator::SubmitVaas as u8);
    data.push(guardian_set_bump);
    data.extend_from_slice(&(body.len() as u16).to_le_bytes());
    data.extend_from_slice(body);
    data
}

fn post_signatures_ix_data(
    guardian_set_index: u32,
    total_signatures: u8,
    guardian_signatures: &[u8],
) -> Vec<u8> {
    assert!(
        guardian_signatures.len() % ParsedVaa::GUARDIAN_SIGNATURE_LENGTH == 0,
        "sig block length must be a multiple of 66"
    );
    let count = guardian_signatures.len() / ParsedVaa::GUARDIAN_SIGNATURE_LENGTH;
    let mut data = Vec::with_capacity(8 + 9 + guardian_signatures.len());
    data.extend_from_slice(&POST_SIGNATURES_SELECTOR);
    data.extend_from_slice(&guardian_set_index.to_le_bytes());
    data.push(total_signatures);
    data.extend_from_slice(&(count as u32).to_le_bytes());
    data.extend_from_slice(guardian_signatures);
    data
}

fn post_signatures(
    rpc: &solana_client::rpc_client::RpcClient,
    payer: &Keypair,
    guardian_signatures_kp: &Keypair,
    vaa: &ParsedVaa,
) {
    let shim_program_id = Pubkey::new_from_array(VERIFY_VAA_SHIM_PROGRAM_ID);
    let ix_data = post_signatures_ix_data(
        vaa.guardian_set_index,
        vaa.num_signatures,
        vaa.signatures_slice(),
    );
    let ix = Instruction {
        program_id: shim_program_id,
        accounts: vec![
            AccountMeta::new(payer.pubkey(), true),
            AccountMeta::new(guardian_signatures_kp.pubkey(), true),
            AccountMeta::new_readonly(system_program::ID, false),
        ],
        data: ix_data,
    };
    let blockhash = rpc.get_latest_blockhash().expect("blockhash post_sigs");
    let tx = Transaction::new_signed_with_payer(
        &[ix],
        Some(&payer.pubkey()),
        &[payer, guardian_signatures_kp],
        blockhash,
    );
    let sig = rpc
        .send_and_confirm_transaction(&tx)
        .expect("PostSignatures send_and_confirm");
    eprintln!("[submit-vaas-e2e] PostSignatures tx={sig}");
}

/// Seed a balance PDA through `surfnet_setAccount`.
fn seed_account_pda(
    rpc_url: &str,
    program_id: &Pubkey,
    pda: &Pubkey,
    chain: u16,
    token_chain: u16,
    token_address: &[u8; 32],
    balance: Uint256,
) {
    let mut layout: BalanceAccountLayout = bytemuck::Zeroable::zeroed();
    layout.tag = BalanceAccountLayout::TAG;
    layout.chain = chain;
    layout.token_chain = token_chain;
    layout.token_address = *token_address;
    layout.balance = balance;
    let bytes = bytemuck::bytes_of(&layout);
    assert_eq!(bytes.len(), BalanceAccountLayout::LEN);

    let resp = rpc_call(
        rpc_url,
        "surfnet_setAccount",
        serde_json::json!([
            pda.to_string(),
            {
                "lamports": BALANCE_PDA_RENT_LAMPORTS,
                "owner": program_id.to_string(),
                "executable": false,
                "rent_epoch": 0u64,
                "data": hex_encode(bytes),
            }
        ]),
    );
    assert!(
        resp.get("error").is_none(),
        "surfnet_setAccount failed for {pda}: {resp}"
    );
}

/// A real Token Bridge transfer VAA sets the NoReplay bit and debits both ledgers.
#[test]
#[ignore = "spawns surfpool subprocess; run via `just test-e2e-submit-vaas` or `cargo test -- --ignored`"]
fn surfpool_submit_vaas_token_bridge_transfer() {
    let ga_so = so_path("global_accountant");
    let ga_bytes = std::fs::read(&ga_so).unwrap_or_else(|e| {
        panic!(
            "could not read {}: {e}. Run `just build-prod` first.",
            ga_so.display()
        )
    });
    let noreplay_so = noreplay_so_path();
    let noreplay_bytes = std::fs::read(&noreplay_so).unwrap_or_else(|e| {
        panic!(
            "could not read {}: {e}. \
             Rebuild via `cd ~/WormholeLabs/CoreTeam/solana-noreplay && just build` \
             or override with GA_NOREPLAY_SO=<path>.",
            noreplay_so.display()
        )
    });
    eprintln!(
        "[submit-vaas-e2e] loaded global_accountant.so={} bytes, solana_noreplay.so={} bytes",
        ga_bytes.len(),
        noreplay_bytes.len()
    );

    let vaa = load_vaa_fixture("mainnet_solana_token_bridge_transfer_seq1395207.vaa");
    eprintln!(
        "[submit-vaas-e2e] VAA gsi={} chain={} sequence={} digest={}",
        vaa.guardian_set_index,
        vaa.emitter_chain,
        vaa.sequence,
        hex_encode(&vaa.digest),
    );
    assert_eq!(
        vaa.guardian_set_index, 6,
        "fixture must come from the active guardian set"
    );

    // Size the pre-seeded balances from the payload.
    let body = &vaa.bytes[vaa.body_offset..];
    assert_eq!(
        body[51], 0x01,
        "fixture must carry a Transfer action (0x01)"
    );
    let mut amount_bytes = [0u8; 32];
    amount_bytes.copy_from_slice(&body[52..84]);
    let amount = Uint256::from_be_bytes(amount_bytes);
    let mut token_address = [0u8; 32];
    token_address.copy_from_slice(&body[84..116]);
    let token_chain = u16::from_be_bytes([body[116], body[117]]);
    let recipient_chain = u16::from_be_bytes([body[150], body[151]]);
    eprintln!(
        "[submit-vaas-e2e] transfer: amount={:?} token_chain={} recipient_chain={} token_addr={}",
        amount,
        token_chain,
        recipient_chain,
        hex_encode(&token_address),
    );

    let guard = start_surfpool(SurfpoolOptions::mainnet_fork(
        "ga-surfpool-submit-vaas",
        datasource_rpc_url(),
    ));
    let rpc_url = guard.rpc_url();
    let rpc = guard.rpc_client();

    // Deploy at the `declare_id!` address; the program rejects any other.
    let ga_program_id = Pubkey::new_from_array(global_accountant::ID.to_bytes());
    let payer = Keypair::new();
    let guardian_signatures_kp = Keypair::new();
    eprintln!(
        "[submit-vaas-e2e] program_id={ga_program_id} payer={} sigs={}",
        payer.pubkey(),
        guardian_signatures_kp.pubkey(),
    );

    let airdrop_sig = rpc
        .request_airdrop(&payer.pubkey(), 20_000_000_000)
        .expect("airdrop payer");
    await_confirmed("airdrop", Duration::from_secs(10), || {
        rpc.confirm_transaction(&airdrop_sig)
    });

    deploy_program(&rpc_url, &ga_program_id, &ga_bytes);
    deploy_program(&rpc_url, &NOREPLAY_PROGRAM_ID, &noreplay_bytes);

    let shim_program_id = Pubkey::new_from_array(VERIFY_VAA_SHIM_PROGRAM_ID);
    let _shim = rpc.get_account(&shim_program_id).expect("Shim lazy-fetch");
    let _core = rpc
        .get_account(&CORE_BRIDGE_PROGRAM_ID)
        .expect("Core Bridge lazy-fetch");
    let (gs_pda, gs_bump) = derive_guardian_set_pda(vaa.guardian_set_index);
    let gs_account = rpc
        .get_account(&gs_pda)
        .expect("GuardianSet PDA lazy-fetch");
    assert_eq!(
        gs_account.owner, CORE_BRIDGE_PROGRAM_ID,
        "GuardianSet owner == Core Bridge"
    );
    eprintln!(
        "[submit-vaas-e2e] gs_pda={gs_pda} bump={gs_bump} data_len={}",
        gs_account.data.len()
    );

    let post_start = Instant::now();
    post_signatures(&rpc, &payer, &guardian_signatures_kp, &vaa);
    eprintln!(
        "[submit-vaas-e2e] PostSignatures elapsed: {:?}",
        post_start.elapsed()
    );

    let (noreplay_authority, _na_bump) = derive_noreplay_authority_pda(&ga_program_id);
    let namespace = build_namespace(vaa.emitter_chain, &vaa.emitter_address);
    let (bitmap_pda, bitmap_bump) =
        derive_noreplay_bitmap_pda(&noreplay_authority, &namespace, vaa.sequence);
    let (source_account_pda, _src_bump) = derive_account_pda(
        &ga_program_id,
        vaa.emitter_chain,
        token_chain,
        &token_address,
    );
    let (dest_account_pda, _dst_bump) =
        derive_account_pda(&ga_program_id, recipient_chain, token_chain, &token_address);
    eprintln!(
        "[submit-vaas-e2e] noreplay_authority={noreplay_authority} \
         bitmap_pda={bitmap_pda} bitmap_bump={bitmap_bump} src_pda={source_account_pda} \
         dst_pda={dest_account_pda}"
    );

    // Both sides debit.
    let seed_amount = amount
        .checked_add(Uint256::from_u128(1_000_000_000))
        .expect("seed amount fits in u256");
    seed_account_pda(
        &rpc_url,
        &ga_program_id,
        &source_account_pda,
        vaa.emitter_chain,
        token_chain,
        &token_address,
        seed_amount,
    );
    seed_account_pda(
        &rpc_url,
        &ga_program_id,
        &dest_account_pda,
        recipient_chain,
        token_chain,
        &token_address,
        seed_amount,
    );

    // Registration PDA written by cheatcode; the `.so` creates it only through `register_chain`.
    let (chain_registration_pda, _cr_bump) =
        derive_chain_registration_pda(&ga_program_id, vaa.emitter_chain);
    {
        let mut registration: ChainRegistrationLayout = bytemuck::Zeroable::zeroed();
        registration.tag = ChainRegistrationLayout::TAG;
        registration.chain = vaa.emitter_chain;
        registration.emitter_address = vaa.emitter_address;
        let resp = rpc_call(
            &rpc_url,
            "surfnet_setAccount",
            serde_json::json!([
                chain_registration_pda.to_string(),
                {
                    "lamports": 1_000_000_000u64,
                    "owner": ga_program_id.to_string(),
                    "executable": false,
                    "rent_epoch": 0u64,
                    "data": hex_encode(bytemuck::bytes_of(&registration)),
                }
            ]),
        );
        assert!(
            resp.get("error").is_none(),
            "surfnet_setAccount failed for chain registration: {resp}"
        );
    }

    let body = vaa.bytes[vaa.body_offset..].to_vec();
    let ix = Instruction {
        program_id: ga_program_id,
        accounts: vec![
            AccountMeta::new(payer.pubkey(), true),
            AccountMeta::new_readonly(shim_program_id, false),
            AccountMeta::new_readonly(gs_pda, false),
            AccountMeta::new_readonly(guardian_signatures_kp.pubkey(), false),
            AccountMeta::new(bitmap_pda, false),
            AccountMeta::new_readonly(NOREPLAY_PROGRAM_ID, false),
            AccountMeta::new_readonly(noreplay_authority, false),
            AccountMeta::new(source_account_pda, false),
            AccountMeta::new(dest_account_pda, false),
            AccountMeta::new_readonly(system_program::ID, false),
            AccountMeta::new_readonly(chain_registration_pda, false),
        ],
        data: submit_vaas_ix_data(gs_bump, &body),
    };
    let blockhash = rpc.get_latest_blockhash().expect("blockhash submit_vaas");
    let tx = Transaction::new_signed_with_payer(
        &[set_compute_unit_limit_ix(SUBMIT_VAAS_CU_LIMIT), ix],
        Some(&payer.pubkey()),
        &[&payer],
        blockhash,
    );
    let sig = rpc
        .send_and_confirm_transaction(&tx)
        .expect("submit_vaas send_and_confirm");
    eprintln!("[submit-vaas-e2e] submit_vaas tx={sig}");

    let bitmap_after = rpc
        .get_account_with_commitment(&bitmap_pda, CommitmentConfig::confirmed())
        .expect("bitmap PDA fetch")
        .value
        .expect("bitmap PDA exists after submit_vaas");
    assert_eq!(
        bitmap_after.owner, NOREPLAY_PROGRAM_ID,
        "bitmap PDA owned by noreplay"
    );
    assert_eq!(bitmap_after.data.len(), 129, "bitmap PDA is 129 bytes");
    let bit_index = (vaa.sequence % 1024) as usize;
    let byte = bitmap_after.data[1 + bit_index / 8];
    assert!(
        byte & (1 << (bit_index % 8)) != 0,
        "bit {bit_index} set in noreplay bitmap after submit_vaas"
    );

    // `guardian_set_index = 0` on the `submit_vaas` path.
    assert_canonical_log_in_tx(
        &rpc_url,
        &sig.to_string(),
        vaa.emitter_chain,
        &vaa.emitter_address,
        vaa.sequence,
        &vaa.digest,
        0,
    );

    // Source: wrapped burn debits.
    let src_after = rpc
        .get_account(&source_account_pda)
        .expect("source Account PDA exists");
    assert_eq!(src_after.owner, ga_program_id);
    let src_layout: &BalanceAccountLayout = bytemuck::from_bytes(&src_after.data);
    let expected_src_balance = seed_amount
        .checked_sub(amount)
        .expect("seed - amount does not underflow");
    assert_eq!(
        src_layout.balance, expected_src_balance,
        "source ledger debited by transfer amount"
    );

    // Dest: native unlock debits.
    let dst_after = rpc
        .get_account(&dest_account_pda)
        .expect("dest Account PDA exists");
    assert_eq!(dst_after.owner, ga_program_id);
    let dst_layout: &BalanceAccountLayout = bytemuck::from_bytes(&dst_after.data);
    assert_eq!(
        dst_layout.balance, expected_src_balance,
        "destination ledger debited by transfer amount"
    );

    eprintln!("[submit-vaas-e2e] all phases green");
}

/// Pin `BalanceAccountLayout::LEN` for the e2e assertions.
const _: () = assert!(
    BalanceAccountLayout::LEN == 70,
    "BalanceAccountLayout::LEN drift — update the e2e assertions"
);
