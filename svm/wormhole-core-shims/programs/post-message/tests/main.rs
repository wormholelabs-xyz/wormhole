use solana_program_test::{tokio, BanksClient, ProgramTest};
use solana_sdk::{
    hash::Hash, pubkey::Pubkey, signature::Keypair, signer::Signer, transaction::Transaction,
};
use wormhole_svm_definitions::{
    CORE_BRIDGE_CONFIG, CORE_BRIDGE_FEE_COLLECTOR, CORE_BRIDGE_PROGRAM_ID,
    POST_MESSAGE_SHIM_PROGRAM_ID,
};
use wormhole_svm_shim::post_message;

#[tokio::test]
async fn test_post_message_no_emitter_sequence() {
    let (mut banks_client, payer_signer, recent_blockhash) = start_test().await;

    let emitter_signer = Keypair::new();
    let (transaction, bump_costs) = set_up_post_message_transaction(
        b"All your base are belong to us",
        &payer_signer,
        &emitter_signer,
        recent_blockhash,
        None, // additional_inputs
    )
    .await;

    let out = banks_client
        .simulate_transaction(transaction)
        .await
        .unwrap();
    assert!(out.result.unwrap().is_ok());

    let details = out.simulation_details.unwrap();
    let logs = details.logs;

    let is_core_bridge_cpi_log = |line: &String| {
        line.contains(format!("Program {} invoke [2]", CORE_BRIDGE_PROGRAM_ID).as_str())
    };

    // CPI to Core Bridge.
    assert_eq!(
        logs.iter()
            .filter(|line| {
                line.contains(format!("Program {} invoke [2]", CORE_BRIDGE_PROGRAM_ID).as_str())
            })
            .count(),
        1
    );
    assert_eq!(
        logs.iter()
            .filter(|line| { line.contains("Program log: Sequence: 0") })
            .count(),
        1
    );

    let core_bridge_log_index = logs.iter().position(is_core_bridge_cpi_log).unwrap();

    // Self CPI.
    assert_eq!(
        logs.iter()
            .skip(core_bridge_log_index)
            .filter(|line| {
                line.contains(
                    format!("Program {} invoke [2]", POST_MESSAGE_SHIM_PROGRAM_ID).as_str(),
                )
            })
            .count(),
        1
    );

    // Wormhole Core Bridge re-derives the sequence account when it needs to be
    // created (cool). So we need to subtract the sequence bump cost twice for
    // the first message.
    assert_eq!(
        details.units_consumed - bump_costs.message - 2 * bump_costs.sequence,
        // 53_418
        46_901
    );
}

#[tokio::test]
async fn test_cannot_post_message_invalid_message() {
    let (mut banks_client, payer_signer, recent_blockhash) = start_test().await;

    let emitter_signer = Keypair::new();
    let (transaction, _) = set_up_post_message_transaction(
        b"All your base are belong to us",
        &payer_signer,
        &emitter_signer,
        recent_blockhash,
        Some(AdditionalTestInputs {
            invalid_message: Some(Pubkey::new_unique()),
            ..Default::default()
        }),
    )
    .await;

    let out = banks_client
        .simulate_transaction(transaction)
        .await
        .unwrap();
    let details = out.simulation_details.unwrap();
    dbg!(&details);
    assert!(out.result.unwrap().is_err());
    //assert!(details.logs.contains(&"Program log: AnchorError caused by account: message. Error Code: ConstraintSeeds. Error Number: 2006. Error Message: A seeds constraint was violated.".to_string()));
    assert!(details
        .logs
        .contains(&"Program log: Message (account #2) seeds constraint violated".to_string()));
}

#[tokio::test]
async fn test_cannot_post_message_invalid_core_bridge_program() {
    let (mut banks_client, payer_signer, recent_blockhash) = start_test().await;

    let emitter_signer = Keypair::new();
    let (transaction, _) = set_up_post_message_transaction(
        b"All your base are belong to us",
        &payer_signer,
        &emitter_signer,
        recent_blockhash,
        Some(AdditionalTestInputs {
            invalid_core_bridge_program: Some(Pubkey::new_unique()),
            ..Default::default()
        }),
    )
    .await;

    let out = banks_client
        .simulate_transaction(transaction)
        .await
        .unwrap();
    let details = out.simulation_details.unwrap();
    dbg!(&details);
    assert!(out.result.unwrap().is_err());
    //assert!(details.logs.contains(&"Program log: AnchorError caused by account: wormhole_program. Error Code: ConstraintAddress. Error Number: 2012. Error Message: An address constraint was violated.".to_string()));
    assert!(details.logs.contains(
        &"Program log: Wormhole program (account #10) address constraint violated".to_string()
    ));
}

#[tokio::test]
async fn test_post_message() {
    let (mut banks_client, payer_signer, recent_blockhash) = start_test().await;

    let first_message = b"All your base";
    let emitter_signer = Keypair::new();
    let (transaction, _) = set_up_post_message_transaction(
        first_message,
        &payer_signer,
        &emitter_signer,
        recent_blockhash,
        None, // additional_inputs
    )
    .await;

    // Send one to create the emitter sequence account.
    banks_client.process_transaction(transaction).await.unwrap();

    let subsequent_message = b"are belong to us";
    assert_ne!(&first_message[..], &subsequent_message[..]);

    let recent_blockhash = banks_client.get_latest_blockhash().await.unwrap();
    let (transaction, bump_costs) = set_up_post_message_transaction(
        subsequent_message,
        &payer_signer,
        &emitter_signer,
        recent_blockhash,
        None, // additional_inputs
    )
    .await;

    let out = banks_client
        .simulate_transaction(transaction)
        .await
        .unwrap();
    assert!(out.result.unwrap().is_ok());
    assert_eq!(
        out.simulation_details.unwrap().units_consumed - bump_costs.message - bump_costs.sequence,
        // 30_901
        24_382
    );
}

async fn start_test() -> (BanksClient, Keypair, Hash) {
    let mut program_test = ProgramTest::new(
        "wormhole_post_message_shim",
        POST_MESSAGE_SHIM_PROGRAM_ID,
        None,
    );
    program_test.add_program("core_bridge", CORE_BRIDGE_PROGRAM_ID, None);
    program_test.add_account_with_base64_data(
        CORE_BRIDGE_CONFIG,
        1_057_920,
        CORE_BRIDGE_PROGRAM_ID,
        "BAAAAAQYDQ0AAAAAgFEBAGQAAAAAAAAA",
    );
    program_test.add_account_with_base64_data(
        CORE_BRIDGE_FEE_COLLECTOR,
        2_350_640_070,
        CORE_BRIDGE_PROGRAM_ID,
        "",
    );
    program_test.prefer_bpf(true);

    program_test.start().await
}

#[derive(Debug, Default)]
struct AdditionalTestInputs {
    invalid_message: Option<Pubkey>,
    invalid_core_bridge_program: Option<Pubkey>,
}

async fn set_up_post_message_transaction(
    payload: &[u8],
    payer_signer: &Keypair,
    emitter_signer: &Keypair,
    recent_blockhash: Hash,
    additional_inputs: Option<AdditionalTestInputs>,
) -> (Transaction, BumpCosts) {
    let emitter = emitter_signer.pubkey();
    let payer = payer_signer.pubkey();

    let AdditionalTestInputs {
        invalid_message,
        invalid_core_bridge_program,
    } = additional_inputs.unwrap_or_default();

    // Use an invalid message if provided.
    let (message, message_bump) = invalid_message.map(|key| (key, 0)).unwrap_or(
        wormhole_svm_definitions::find_shim_message_address(&emitter),
    );
    // Use an invalid core bridge program if provided.
    let core_bridge_program = invalid_core_bridge_program.unwrap_or(CORE_BRIDGE_PROGRAM_ID);

    let (sequence, sequence_bump) =
        wormhole_svm_definitions::find_emitter_sequence_address(&emitter);

    let transfer_fee_ix =
        solana_sdk::system_instruction::transfer(&payer, &CORE_BRIDGE_FEE_COLLECTOR, 100);
    let ix = post_message::PostMessage {
        core_bridge_config: &CORE_BRIDGE_CONFIG,
        message: Some(&message),
        emitter: &emitter,
        sequence: Some(&sequence),
        payer: &payer,
        fee_collector: &CORE_BRIDGE_FEE_COLLECTOR,
        core_bridge_program: &core_bridge_program,
        event_authority: Default::default(),
        data: post_message::PostMessageData {
            nonce: 420,
            payload,
            finality: post_message::Finality::Finalized,
        },
    }
    .instruction(&POST_MESSAGE_SHIM_PROGRAM_ID);

    let mut transaction = Transaction::new_with_payer(&[transfer_fee_ix, ix], Some(&payer));
    transaction.sign(&[&payer_signer, &emitter_signer], recent_blockhash);

    (
        transaction,
        BumpCosts {
            message: bump_cu_cost(message_bump),
            sequence: bump_cu_cost(sequence_bump),
        },
    )
}

struct BumpCosts {
    message: u64,
    sequence: u64,
}

fn bump_cu_cost(bump: u8) -> u64 {
    1_500 * (255 - u64::from(bump))
}