//! Go layouts and fixtures of the WTT global accountant.

use core::mem::{offset_of, size_of_val};

use accountant_operational_core::cpi::noreplay::derive_bucket_pda;
use accountant_operational_core::support::quorum::derive_pending_pda;
use accountant_test_fixtures::{Vaa, MAINNET_OTHER_SEQ2211, MAINNET_TRANSFER_SEQ1395207};
use accountant_test_harness::{
    content_digest, noreplay_authority_pda, observation_ix_from_body, signing_digest_with_tx_id,
    submit_observations_ix_data_with_tx_id,
};
use bytemuck::Zeroable;
use global_accountant_definitions::{
    AccountantDigestLog, AccountantPayerLog, ChainRegistrationLayout, GlobalAccountantError,
    Instruction, NoReplayBitmapAccount, NoReplayNamespace, PendingObservationsLayout,
    SubmitObservationsIxData, TokenBridgeTransfer, TxId, VaaBodyHeader, ACCOUNTANT_DIGEST_LOG_TAG,
    ACCOUNTANT_PAYER_LOG_TAG, ACCOUNT_SEED_PREFIX, CHAIN_REGISTRATION_SEED_PREFIX,
    GUARDIAN_SET_SEED, HASH_TX_ID_LEN, NOREPLAY_AUTHORITY_SEED_PREFIX, NOREPLAY_BITS_PER_BUCKET,
    PENDING_OBSERVATIONS_SEED_PREFIX, SIGNATURE_TX_ID_LEN,
};
use solana_pubkey::Pubkey;

use crate::go::{field, n, Field, GoFile, GoType};

/// The program dispatches on one `u8` discriminator before the instruction data.
const DISPATCH_LEN: usize = 1;

/// Start of the hashed record: the tail of `SubmitObservationsIxData` from `action`.
/// `real_vectors` asserts that the tail equals `fields_and_digest()`.
const FIELDS_START: usize = offset_of!(SubmitObservationsIxData, action);

/// Guardian set of the mainnet fixture VAAs.
const MAINNET_GUARDIAN_SET: u32 = 6;

/// Guardian indices covering every word of the 128-bit bitmap.
const SET_BITS: [u8; 6] = [0, 3, 5, 33, 64, 127];

/// Fields and content digests exclude the tx id. Thus the real-data vectors use a zero tx id.
const ZERO_TX_ID: TxId<'static> = TxId::Hash(&[0u8; 32]);

pub(crate) fn layout_file() -> String {
    let mut go = GoFile::new();

    go.section("AccountantDigestLog, constants/log.rs.");
    go.constant(
        "AccountantDigestLog::LEN.",
        "accountantDigestLogLen",
        AccountantDigestLog::LEN,
    );
    go.byte_array(
        "ACCOUNTANT_DIGEST_LOG_TAG.",
        "accountantDigestLogTag",
        &ACCOUNTANT_DIGEST_LOG_TAG,
    );
    go.wire_struct(
        "accountantDigestLogWire is AccountantDigestLog.",
        "accountantDigestLogWire",
        "accountant digest log",
        "accountantDigestLogLen",
        AccountantDigestLog::LEN,
        0,
        &[
            field!(
                AccountantDigestLog,
                tag,
                "Tag",
                GoType::Bytes(ACCOUNTANT_DIGEST_LOG_TAG.len())
            ),
            field!(AccountantDigestLog, chain, "Chain", GoType::Be16),
            field!(AccountantDigestLog, emitter, "Emitter", GoType::Bytes(32)),
            field!(AccountantDigestLog, sequence, "Sequence", GoType::Be64),
            field!(AccountantDigestLog, digest, "Digest", GoType::Bytes(32)),
            field!(
                AccountantDigestLog,
                guardian_set_index,
                "GuardianSetIndex",
                GoType::U32
            ),
        ],
    );

    go.section("AccountantPayerLog, constants/log.rs.");
    go.constant(
        "AccountantPayerLog::LEN.",
        "accountantPayerLogLen",
        AccountantPayerLog::LEN,
    );
    go.byte_array(
        "ACCOUNTANT_PAYER_LOG_TAG.",
        "accountantPayerLogTag",
        &ACCOUNTANT_PAYER_LOG_TAG,
    );
    go.wire_struct(
        "accountantPayerLogWire is AccountantPayerLog.",
        "accountantPayerLogWire",
        "accountant payer log",
        "accountantPayerLogLen",
        AccountantPayerLog::LEN,
        0,
        &[
            field!(
                AccountantPayerLog,
                tag,
                "Tag",
                GoType::Bytes(ACCOUNTANT_PAYER_LOG_TAG.len())
            ),
            field!(
                AccountantPayerLog,
                pending_pda,
                "PendingPDA",
                GoType::Bytes(32)
            ),
            field!(
                AccountantPayerLog,
                recorded_payer,
                "RecordedPayer",
                GoType::Bytes(32)
            ),
        ],
    );

    let signature_words = PendingObservationsLayout::zeroed().signatures.len();
    go.section("PendingObservationsLayout, state.rs.");
    go.constants(&[
        (
            "PendingObservationsLayout::LEN.",
            "pendingObservationsLen",
            n(PendingObservationsLayout::LEN),
        ),
        (
            "PendingObservationsLayout::TAG.",
            "pendingObservationsTag",
            u64::from(PendingObservationsLayout::TAG),
        ),
        (
            "PendingObservationsLayout::MAX_GUARDIANS.",
            "pendingObservationsMaxGuardians",
            u64::from(PendingObservationsLayout::MAX_GUARDIANS),
        ),
        (
            "u32 words in the signature bitmap.",
            "pendingObservationsSignatureWords",
            n(signature_words),
        ),
    ]);
    go.wire_struct(
        "pendingObservationsWire is PendingObservationsLayout.",
        "pendingObservationsWire",
        "pending observations account",
        "pendingObservationsLen",
        PendingObservationsLayout::LEN,
        0,
        &[
            field!(PendingObservationsLayout, tag, "Tag", GoType::U8),
            field!(PendingObservationsLayout, tx_id_len, "TxIDLen", GoType::U8),
            field!(PendingObservationsLayout, chain, "Chain", GoType::U16),
            field!(
                PendingObservationsLayout,
                guardian_set_index,
                "GuardianSetIndex",
                GoType::U32
            ),
            field!(
                PendingObservationsLayout,
                signatures,
                "Signatures",
                GoType::Words(signature_words)
            ),
            field!(
                PendingObservationsLayout,
                content_digest,
                "ContentDigest",
                GoType::Bytes(32)
            ),
            field!(PendingObservationsLayout, payer, "Payer", GoType::Bytes(32)),
            field!(PendingObservationsLayout, tx_id, "TxID", GoType::Bytes(64)),
        ],
    );

    let bitmap_bytes = NoReplayBitmapAccount::zeroed().bitmap.len();
    let namespace = NoReplayNamespace::new(0, [0; 32]);
    let (namespace_seed_a, _) = namespace.seed_chunks();
    go.section("solana-noreplay, constants/noreplay.rs.");
    go.constants(&[
        (
            "NoReplayBitmapAccount::LEN.",
            "noreplayBucketLen",
            n(NoReplayBitmapAccount::LEN),
        ),
        (
            "NOREPLAY_BITS_PER_BUCKET.",
            "noreplayBitsPerBucket",
            NOREPLAY_BITS_PER_BUCKET,
        ),
        (
            "NoReplayNamespace::LEN.",
            "noreplayNamespaceLen",
            n(NoReplayNamespace::LEN),
        ),
        (
            "NoReplayNamespace::seed_chunks split point.",
            "noreplayNamespaceSeedSplit",
            n(namespace_seed_a.len()),
        ),
    ]);
    go.wire_struct(
        "noreplayBucketWire is NoReplayBitmapAccount.",
        "noreplayBucketWire",
        "noreplay bucket account",
        "noreplayBucketLen",
        NoReplayBitmapAccount::LEN,
        0,
        &[
            field!(NoReplayBitmapAccount, bump, "Bump", GoType::U8),
            field!(
                NoReplayBitmapAccount,
                bitmap,
                "Bitmap",
                GoType::Bytes(bitmap_bytes)
            ),
        ],
    );
    go.wire_struct(
        "noreplayNamespaceWire is NoReplayNamespace.",
        "noreplayNamespaceWire",
        "noreplay namespace",
        "noreplayNamespaceLen",
        NoReplayNamespace::LEN,
        0,
        &[
            field!(NoReplayNamespace, chain, "Chain", GoType::Be16),
            field!(NoReplayNamespace, emitter, "Emitter", GoType::Bytes(32)),
        ],
    );

    let fields_len = SubmitObservationsIxData::LEN - FIELDS_START;
    go.section("ObservationFieldsAndDigest, ix_data.rs: the hashed record.");
    go.constant(
        "Length of the hashed record.",
        "observationFieldsLen",
        fields_len,
    );
    go.wire_struct(
        "observationFieldsWire is the tail of SubmitObservationsIxData from action.",
        "observationFieldsWire",
        "observation fields",
        "observationFieldsLen",
        fields_len,
        FIELDS_START,
        &[
            field!(SubmitObservationsIxData, action, "Action", GoType::U8),
            field!(SubmitObservationsIxData, chain, "Chain", GoType::Be16),
            field!(
                SubmitObservationsIxData,
                emitter,
                "Emitter",
                GoType::Bytes(32)
            ),
            field!(SubmitObservationsIxData, sequence, "Sequence", GoType::Be64),
            field!(
                SubmitObservationsIxData,
                token_chain,
                "TokenChain",
                GoType::Be16
            ),
            field!(
                SubmitObservationsIxData,
                token_address,
                "TokenAddress",
                GoType::Bytes(32)
            ),
            field!(
                SubmitObservationsIxData,
                recipient_chain,
                "RecipientChain",
                GoType::Be16
            ),
            field!(
                SubmitObservationsIxData,
                amount,
                "Amount",
                GoType::Bytes(32)
            ),
            field!(
                SubmitObservationsIxData,
                digest,
                "VaaDigest",
                GoType::Bytes(32)
            ),
        ],
    );

    assert_eq!(
        size_of_val(&SubmitObservationsIxData::zeroed().tx_id),
        SIGNATURE_TX_ID_LEN
    );
    let instruction_len = DISPATCH_LEN + SubmitObservationsIxData::LEN;
    go.section("submit_observations instruction, ix_data.rs.");
    go.constants(&[
        (
            "Instruction::SubmitObservations.",
            "submitObservationsDiscriminator",
            u64::from(Instruction::SubmitObservations as u8),
        ),
        (
            "SubmitObservationsIxData::LEN.",
            "submitObservationsIxDataLen",
            n(SubmitObservationsIxData::LEN),
        ),
        (
            "One u8 discriminator plus the data.",
            "submitObservationsInstructionLen",
            n(instruction_len),
        ),
        (
            "r ‖ s ‖ recovery_id.",
            "submitSignatureLen",
            n(size_of_val(&SubmitObservationsIxData::zeroed().signature)),
        ),
        ("HASH_TX_ID_LEN.", "hashTxIDLen", n(HASH_TX_ID_LEN)),
        (
            "SIGNATURE_TX_ID_LEN.",
            "signatureTxIDLen",
            n(SIGNATURE_TX_ID_LEN),
        ),
    ]);
    go.wire_struct(
        "submitObservationsIxDataWire is SubmitObservationsIxData.",
        "submitObservationsIxDataWire",
        "submit_observations instruction data",
        "submitObservationsIxDataLen",
        SubmitObservationsIxData::LEN,
        0,
        &[
            field!(
                SubmitObservationsIxData,
                guardian_set_index,
                "GuardianSetIndex",
                GoType::U32
            ),
            field!(
                SubmitObservationsIxData,
                guardian_index,
                "GuardianIndex",
                GoType::U8
            ),
            field!(
                SubmitObservationsIxData,
                signature,
                "Signature",
                GoType::Bytes(65)
            ),
            field!(SubmitObservationsIxData, tx_id_len, "TxIDLen", GoType::U8),
            field!(
                SubmitObservationsIxData,
                tx_id,
                "TxID",
                GoType::Bytes(SIGNATURE_TX_ID_LEN)
            ),
            Field {
                go_name: "Fields",
                ty: GoType::Wire("observationFieldsWire", fields_len),
                offset: FIELDS_START,
                size: fields_len,
            },
        ],
    );
    go.wire_struct(
        "submitObservationsInstructionWire is the discriminator and SubmitObservationsIxData.",
        "submitObservationsInstructionWire",
        "submit_observations instruction data",
        "submitObservationsInstructionLen",
        instruction_len,
        0,
        &[
            Field {
                go_name: "Discriminator",
                ty: GoType::U8,
                offset: 0,
                size: DISPATCH_LEN,
            },
            Field {
                go_name: "Data",
                ty: GoType::Wire(
                    "submitObservationsIxDataWire",
                    SubmitObservationsIxData::LEN,
                ),
                offset: DISPATCH_LEN,
                size: SubmitObservationsIxData::LEN,
            },
        ],
    );

    go.section("Token Bridge transfer payload, vaa.rs.");
    go.constants(&[(
        "TokenBridgeTransfer::LEN.",
        "tokenBridgeTransferLen",
        n(TokenBridgeTransfer::LEN),
    )]);
    go.wire_struct(
        "tokenBridgeTransferWire is TokenBridgeTransfer, the fixed head of a transfer payload.",
        "tokenBridgeTransferWire",
        "token bridge transfer",
        "tokenBridgeTransferLen",
        TokenBridgeTransfer::LEN,
        0,
        &[
            field!(TokenBridgeTransfer, action, "Action", GoType::U8),
            field!(TokenBridgeTransfer, amount, "Amount", GoType::Bytes(32)),
            field!(
                TokenBridgeTransfer,
                token_address,
                "TokenAddress",
                GoType::Bytes(32)
            ),
            field!(TokenBridgeTransfer, token_chain, "TokenChain", GoType::Be16),
            field!(
                TokenBridgeTransfer,
                recipient,
                "Recipient",
                GoType::Bytes(32)
            ),
            field!(
                TokenBridgeTransfer,
                recipient_chain,
                "RecipientChain",
                GoType::Be16
            ),
            field!(TokenBridgeTransfer, fee, "Fee", GoType::Bytes(32)),
        ],
    );

    go.section("PDA seed prefixes, constants/seeds.rs.");
    go.seeds(&[
        (
            "PENDING_OBSERVATIONS_SEED_PREFIX.",
            "pendingObservationsSeedPrefix",
            PENDING_OBSERVATIONS_SEED_PREFIX,
        ),
        (
            "NOREPLAY_AUTHORITY_SEED_PREFIX.",
            "noreplayAuthoritySeedPrefix",
            NOREPLAY_AUTHORITY_SEED_PREFIX,
        ),
        (
            "ACCOUNT_SEED_PREFIX.",
            "balanceAccountSeedPrefix",
            ACCOUNT_SEED_PREFIX,
        ),
        (
            "CHAIN_REGISTRATION_SEED_PREFIX.",
            "chainRegistrationSeedPrefix",
            CHAIN_REGISTRATION_SEED_PREFIX,
        ),
        (
            "GUARDIAN_SET_SEED. The Core Bridge's own seed.",
            "guardianSetSeedPrefix",
            GUARDIAN_SET_SEED,
        ),
    ]);

    go.section("GlobalAccountantError codes returned as ProgramError::Custom, error.rs.");
    for (name, code) in [
        (
            "solanaErrPayerMismatch",
            GlobalAccountantError::PayerMismatch,
        ),
        (
            "solanaErrAlreadyAccounted",
            GlobalAccountantError::AlreadyAccounted,
        ),
        (
            "solanaErrInvalidSignature",
            GlobalAccountantError::InvalidSignature,
        ),
        (
            "solanaErrInvalidGuardianIndex",
            GlobalAccountantError::InvalidGuardianIndex,
        ),
        (
            "solanaErrAlreadySigned",
            GlobalAccountantError::AlreadySigned,
        ),
        (
            "solanaErrExpiredGuardianSet",
            GlobalAccountantError::ExpiredGuardianSet,
        ),
        (
            "solanaErrMissingChainRegistration",
            GlobalAccountantError::MissingChainRegistration,
        ),
        (
            "solanaErrUnregisteredEmitter",
            GlobalAccountantError::UnregisteredEmitter,
        ),
        (
            "solanaErrCpiInvocation",
            GlobalAccountantError::CpiInvocation,
        ),
        (
            "solanaErrInstructionNotFirst",
            GlobalAccountantError::InstructionNotFirst,
        ),
    ] {
        go.constant(
            &format!("GlobalAccountantError::{code:?}."),
            name,
            u32::from(code),
        );
    }

    go.finish()
}

/// Writes the real-data vectors of one fixture VAA, named `fixture<label>*`, and returns its
/// header. The fields exclude the guardian set, so the vectors use set 0.
fn real_vectors(go: &mut GoFile, label: &str, vaa: &Vaa) -> VaaBodyHeader {
    let body = vaa.body();
    let ix = observation_ix_from_body(0, 0, [0u8; 65], ZERO_TX_ID, body);
    let fields = ix.fields_and_digest();
    assert_eq!(
        &bytemuck::bytes_of(&ix)[FIELDS_START..],
        fields.as_slice(),
        "{label}: the hashed record is the instruction data tail"
    );

    let (header, _payload) = VaaBodyHeader::split(body).expect("fixture has a valid VAA header");
    go.hex_constant("", &format!("fixture{label}BodyHex"), body);
    go.hex_constant(
        "",
        &format!("fixture{label}EmitterHex"),
        &header.emitter_address,
    );
    go.hex_constant("", &format!("fixture{label}VaaDigestHex"), &ix.digest);
    go.hex_constant("", &format!("fixture{label}FieldsHex"), &fields);
    go.hex_constant(
        "",
        &format!("fixture{label}ContentDigestHex"),
        &content_digest(body),
    );
    go.constant(
        "",
        &format!("fixture{label}Sequence"),
        format!("uint64({})", header.sequence()),
    );
    *header
}

pub(crate) fn fixtures_file() -> String {
    // The hex fixtures below embed these lengths.
    assert_eq!(PendingObservationsLayout::LEN, 152);
    assert_eq!(SubmitObservationsIxData::LEN, 278);
    assert_eq!(AccountantDigestLog::LEN, 86);
    assert_eq!(AccountantPayerLog::LEN, 72);

    // Index-derived bytes make byte-order bugs in the Go decoder visible.
    let program_id_bytes: [u8; 32] = core::array::from_fn(|i| i as u8);
    let program_id = Pubkey::new_from_array(program_id_bytes);
    let emitter: [u8; 32] = core::array::from_fn(|i| 0x40 + i as u8);
    let digest: [u8; 32] = core::array::from_fn(|i| 0x80 + i as u8);
    let payer: [u8; 32] = core::array::from_fn(|i| 0xC0 + i as u8);
    let signature: [u8; 65] = core::array::from_fn(|i| i as u8);
    let hash_tx_id: [u8; 32] = core::array::from_fn(|i| 0xE0 + i as u8);
    let signature_tx_id: [u8; 64] = core::array::from_fn(|i| 0xA0 + i as u8);
    let chain: u16 = 2;
    let sequence: u64 = 100_000;
    let guardian_set_index: u32 = 4;
    let guardian_index: u8 = 3;

    let mut go = GoFile::new();
    go.constant(
        "VaaBodyHeader::LEN, vaa.rs. The payload follows it.",
        "fixtureVaaBodyHeaderLen",
        VaaBodyHeader::LEN,
    );

    go.section("ChainRegistrationLayout, state.rs.");
    go.constants(&[
        (
            "ChainRegistrationLayout::LEN.",
            "chainRegistrationLen",
            n(ChainRegistrationLayout::LEN),
        ),
        (
            "ChainRegistrationLayout::TAG.",
            "chainRegistrationTag",
            u64::from(ChainRegistrationLayout::TAG),
        ),
    ]);
    go.wire_struct(
        "chainRegistrationWire is ChainRegistrationLayout.",
        "chainRegistrationWire",
        "chain registration account",
        "chainRegistrationLen",
        ChainRegistrationLayout::LEN,
        0,
        &[
            field!(ChainRegistrationLayout, tag, "Tag", GoType::U8),
            field!(ChainRegistrationLayout, chain, "Chain", GoType::U16),
            field!(
                ChainRegistrationLayout,
                governance_sequence,
                "GovernanceSequence",
                GoType::U64
            ),
            field!(
                ChainRegistrationLayout,
                emitter_address,
                "EmitterAddress",
                GoType::Bytes(32)
            ),
        ],
    );

    go.section(
        "Index-derived vectors: program id 0x00.., emitter 0x40.., digest 0x80.., payer 0xC0...",
    );
    let mut pending = PendingObservationsLayout::new(
        chain,
        guardian_set_index,
        digest,
        payer,
        TxId::Signature(&signature_tx_id),
    );
    for index in SET_BITS {
        pending
            .set_signature(index)
            .expect("index below MAX_GUARDIANS");
    }
    assert_eq!(pending.num_signatures(), SET_BITS.len() as u32);
    let pending_bytes = bytemuck::bytes_of(&pending);
    assert_eq!(pending_bytes.len(), PendingObservationsLayout::LEN);
    go.hex_constant(
        "Chain 2, set index 4, bits 0, 3, 5, 33, 64 and 127, signature tx id 0xA0...",
        "fixturePendingObservationsAccountHex",
        pending_bytes,
    );

    let noreplay_authority = noreplay_authority_pda(&program_id);
    go.hex_constant(
        "",
        "fixtureNoreplayAuthorityPDAHex",
        noreplay_authority.as_array(),
    );

    // Sequences 1023 and 1024 straddle a bucket boundary.
    let boundary_chain: u16 = 56;
    let (bucket_1023, _) = derive_bucket_pda(&noreplay_authority, boundary_chain, &emitter, 1023);
    let (bucket_1024, _) = derive_bucket_pda(&noreplay_authority, boundary_chain, &emitter, 1024);
    assert_ne!(
        bucket_1023.as_array(),
        bucket_1024.as_array(),
        "sequence 1023 and 1024 must fall in different buckets"
    );
    go.hex_constant(
        "Chain 56, sequence 1023.",
        "fixtureNoreplayBucketPDASeq1023Hex",
        bucket_1023.as_array(),
    );
    go.hex_constant(
        "Chain 56, sequence 1024.",
        "fixtureNoreplayBucketPDASeq1024Hex",
        bucket_1024.as_array(),
    );

    let commit_log = AccountantDigestLog::new(chain, emitter, sequence, digest, guardian_set_index);
    assert_eq!(commit_log.as_bytes().len(), AccountantDigestLog::LEN);
    go.hex_constant(
        "Chain 2, sequence 100000, set index 4.",
        "fixtureACCDGSTLogHex",
        commit_log.as_bytes(),
    );

    let payer_log = AccountantPayerLog::new(digest, payer);
    assert_eq!(payer_log.as_bytes().len(), AccountantPayerLog::LEN);
    go.hex_constant(
        "Pending PDA 0x80.., recorded payer 0xC0...",
        "fixtureACCPAYRLogHex",
        payer_log.as_bytes(),
    );

    go.section(&format!(
        "Mainnet Solana Token Bridge transfer, sequence 1395207, guardian set {MAINNET_GUARDIAN_SET}."
    ));
    let transfer_body = MAINNET_TRANSFER_SEQ1395207.body();
    let transfer_header = real_vectors(&mut go, "Transfer", &MAINNET_TRANSFER_SEQ1395207);
    let (transfer_pending_pda, _) = derive_pending_pda(
        &program_id,
        transfer_header.emitter_chain(),
        &transfer_header.emitter_address,
        transfer_header.sequence(),
        MAINNET_GUARDIAN_SET,
        &content_digest(transfer_body),
        TxId::Hash(&hash_tx_id),
    );
    go.hex_constant(
        "Hash tx id 0xE0...",
        "fixtureTransferPendingPDAHex",
        transfer_pending_pda.as_array(),
    );
    go.constant(
        "",
        "fixtureTransferGuardianSetIndex",
        format!("uint32({MAINNET_GUARDIAN_SET})"),
    );

    go.section("Mainnet Solana Token Bridge message with non-transfer action 0x99, sequence 2211.");
    real_vectors(&mut go, "Other", &MAINNET_OTHER_SEQ2211);

    go.section("submit_observations data for the transfer body, guardian set 4, guardian index 3.");
    let hash = TxId::Hash(&hash_tx_id);
    let sig = TxId::Signature(&signature_tx_id);
    let submit_data = |tx_id| {
        let data = submit_observations_ix_data_with_tx_id(
            guardian_set_index,
            guardian_index,
            signature,
            tx_id,
            transfer_body,
        );
        assert_eq!(data.len(), DISPATCH_LEN + SubmitObservationsIxData::LEN);
        data
    };
    go.hex_constant(
        "32-byte tx id.",
        "fixtureSubmitObservationsIxDataHex",
        &submit_data(hash),
    );
    go.hex_constant("", "fixtureSubmitObservationsTxIDHex", &hash_tx_id);
    go.hex_constant(
        "64-byte tx id.",
        "fixtureSubmitObservationsSignatureTxIDIxDataHex",
        &submit_data(sig),
    );
    go.hex_constant(
        "",
        "fixtureSubmitObservationsSignatureTxIDHex",
        &signature_tx_id,
    );

    go.section(
        "What each guardian signs for the transfer above: keccak256(prefix ‖ tx_id ‖ fields).",
    );
    go.hex_constant(
        "32-byte tx id.",
        "fixtureTransferSigningDigestHex",
        &signing_digest_with_tx_id(hash, transfer_body),
    );
    go.hex_constant(
        "64-byte tx id.",
        "fixtureTransferSignatureTxIDSigningDigestHex",
        &signing_digest_with_tx_id(sig, transfer_body),
    );

    go.finish()
}
