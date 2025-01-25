use solana_program_test::{tokio, BanksClient, ProgramTest};
use solana_sdk::{hash::Hash, signature::Keypair};
use wormhole_svm_definitions::{
    find_guardian_set_address, CORE_BRIDGE_CONFIG, CORE_BRIDGE_PROGRAM_ID,
    VERIFY_VAA_SHIM_PROGRAM_ID,
};

#[tokio::test]
async fn test_dummy() {
    let (mut _banks_client, _payer, _recent_blockhash) = start_test().await;
}

async fn start_test() -> (BanksClient, Keypair, Hash) {
    let mut program_test =
        ProgramTest::new("wormhole_verify_vaa_shim", VERIFY_VAA_SHIM_PROGRAM_ID, None);
    program_test.add_program("core_bridge", CORE_BRIDGE_PROGRAM_ID, None);
    program_test.add_account_with_base64_data(
        CORE_BRIDGE_CONFIG,
        1_057_920,
        CORE_BRIDGE_PROGRAM_ID,
        "BAAAAAQYDQ0AAAAAgFEBAGQAAAAAAAAA",
    );
    program_test.add_account_with_base64_data(
        find_guardian_set_address(u32::to_be_bytes(4), &CORE_BRIDGE_PROGRAM_ID).0,
        3_647_040,
        CORE_BRIDGE_PROGRAM_ID,
        "BAAAABMAAABYk7WnbD9zlkVkiIW9zMBs1wo80/9suVJYm96GLCXvQ5ITL7nUpCFXEU3oRgGTvfOi/PgfhqCXZfR2L9EQegCGsy16CXeSaiBRMdhzHTnL64yCsv2C+u0nEdWa8PJJnRbnJvayEbOXVsBCRBvm2GULabVOvnFeI0NUzltNNI+3S5WOiWbi7D29SVinzRXnyvB8Tj3I58Rp+SyM2I+4AFogdKO/kTlT1pUmDYi8GqJaTu42PvAACsAHZyezX76i2sKP7lzLD+p2jq9FztE2udniSQNGSuiJ9cinI/wU+TEkt8c4hDy7iehkyGLDjN3Mz5XSzDek3ANqjSMrSPYs3UcxQS9IkNp5j2iWozMfZLSMEtHVf9nL5wgRcaob4dNsr+OGeRD5nAnjR4mcGcOBkrbnOHzNdoJ3wX2rG3pQJ8CzzxeOIa0ud64GcRVJz7sfnHqdgJboXhSH81UV0CqSdTUEqNdUcbn0nttvvryJj0A+R3PpX+sV6Ayamcg0jXiZHmYAAAAA",
    );
    program_test.prefer_bpf(true);

    program_test.start().await
}
