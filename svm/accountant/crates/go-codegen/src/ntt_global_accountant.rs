//! Go layouts and fixtures of the NTT global accountant.
//!
//! Items both accountants share (digest log, pending account, NoReplay, tx id lengths) come
//! from `global_accountant`. This module asserts that the shared parts agree and emits the
//! NTT-only layouts.

use core::mem::{offset_of, size_of_val};

use accountant_operational_core::accounts::{balance, chain_registration};
use accountant_operational_core::hash::double_keccak256;
use accountant_operational_core::support::pda;
use accountant_operational_core::support::quorum::{derive_pending_pda, observation_digests};
use accountant_test_fixtures::{NttCorpus, NttVector};
use accountant_test_harness::ntt::{hub_layout, peer_layout, NttInstruction, Observation};
use bytemuck::Zeroable;
use global_accountant_definitions::{
    normalize_trimmed_amount, parse_delivery_instruction, parse_native_token_transfer,
    GlobalAccountantError, Instruction, NttSubmitObservationsIxData, SubmitObservationsIxData,
    TransceiverHubKey, TransceiverHubLayout, TransceiverPeerKey, TransceiverPeerLayout, TxId,
    VaaBodyHeader, NTT_SUBMIT_OBSERVATION_PREFIX, SIGNATURE_TX_ID_LEN, TRANSCEIVER_HUB_SEED_PREFIX,
    TRANSCEIVER_PEER_SEED_PREFIX,
};
use solana_pubkey::Pubkey;

use crate::go::{field, n, Field, GoFile, GoType};

/// The program dispatches on one `u8` discriminator before the instruction data.
const DISPATCH_LEN: usize = 1;

/// Start of the hashed record: the tail of `NttSubmitObservationsIxData` from `chain`.
const FIELDS_START: usize = offset_of!(NttSubmitObservationsIxData, chain);

/// Guardian set of the fixture observations. The fields exclude it.
const GUARDIAN_SET_INDEX: u32 = 4;

/// Fields and content digests exclude the tx id. Thus the real-data vectors use a zero tx id.
const ZERO_TX_ID: TxId<'static> = TxId::Hash(&[0u8; 32]);

pub(crate) fn layout_file() -> String {
    // SECURITY: the NTT instruction data shares its head with the WTT instruction data. The
    // generic submit code reads the head through the WTT wire struct.
    assert_eq!(
        NttInstruction::SubmitObservations as u8,
        Instruction::SubmitObservations as u8,
        "discriminators differ"
    );
    assert_eq!(
        offset_of!(NttSubmitObservationsIxData, tx_id),
        offset_of!(SubmitObservationsIxData, tx_id),
        "tx_id offsets differ"
    );
    assert_eq!(
        offset_of!(NttSubmitObservationsIxData, tx_id_len),
        offset_of!(SubmitObservationsIxData, tx_id_len),
        "tx_id_len offsets differ"
    );
    assert_eq!(
        FIELDS_START,
        offset_of!(SubmitObservationsIxData, action),
        "the hashed records start at different offsets"
    );
    assert_eq!(
        size_of_val(&NttSubmitObservationsIxData::zeroed().tx_id),
        SIGNATURE_TX_ID_LEN
    );

    let mut go = GoFile::new();

    let fields_len = NttSubmitObservationsIxData::LEN - FIELDS_START;
    go.section("NttObservationFieldsAndDigest, ix_data.rs: the hashed record.");
    go.constant(
        "Length of the hashed record.",
        "nttObservationFieldsLen",
        fields_len,
    );
    go.wire_struct(
        "nttObservationFieldsWire is the tail of NttSubmitObservationsIxData from chain.",
        "nttObservationFieldsWire",
        "ntt observation fields",
        "nttObservationFieldsLen",
        fields_len,
        FIELDS_START,
        &[
            field!(NttSubmitObservationsIxData, chain, "Chain", GoType::Be16),
            field!(
                NttSubmitObservationsIxData,
                emitter,
                "Emitter",
                GoType::Bytes(32)
            ),
            field!(
                NttSubmitObservationsIxData,
                sequence,
                "Sequence",
                GoType::Be64
            ),
            field!(
                NttSubmitObservationsIxData,
                sender,
                "Sender",
                GoType::Bytes(32)
            ),
            field!(
                NttSubmitObservationsIxData,
                recipient_chain,
                "RecipientChain",
                GoType::Be16
            ),
            field!(
                NttSubmitObservationsIxData,
                trimmed_decimals,
                "TrimmedDecimals",
                GoType::U8
            ),
            field!(
                NttSubmitObservationsIxData,
                trimmed_amount,
                "TrimmedAmount",
                GoType::Be64
            ),
            field!(
                NttSubmitObservationsIxData,
                digest,
                "VaaDigest",
                GoType::Bytes(32)
            ),
        ],
    );

    let instruction_len = DISPATCH_LEN + NttSubmitObservationsIxData::LEN;
    go.section("NTT submit_observations instruction, ix_data.rs.");
    go.constants(&[
        (
            "NttInstruction::SubmitObservations.",
            "nttSubmitObservationsDiscriminator",
            u64::from(NttInstruction::SubmitObservations as u8),
        ),
        (
            "NttSubmitObservationsIxData::LEN.",
            "nttSubmitObservationsIxDataLen",
            n(NttSubmitObservationsIxData::LEN),
        ),
        (
            "One u8 discriminator plus the data.",
            "nttSubmitObservationsInstructionLen",
            n(instruction_len),
        ),
    ]);
    go.wire_struct(
        "nttSubmitObservationsIxDataWire is NttSubmitObservationsIxData.",
        "nttSubmitObservationsIxDataWire",
        "ntt submit_observations instruction data",
        "nttSubmitObservationsIxDataLen",
        NttSubmitObservationsIxData::LEN,
        0,
        &[
            field!(
                NttSubmitObservationsIxData,
                guardian_set_index,
                "GuardianSetIndex",
                GoType::U32
            ),
            field!(
                NttSubmitObservationsIxData,
                guardian_index,
                "GuardianIndex",
                GoType::U8
            ),
            field!(
                NttSubmitObservationsIxData,
                signature,
                "Signature",
                GoType::Bytes(65)
            ),
            field!(
                NttSubmitObservationsIxData,
                tx_id_len,
                "TxIDLen",
                GoType::U8
            ),
            field!(
                NttSubmitObservationsIxData,
                tx_id,
                "TxID",
                GoType::Bytes(SIGNATURE_TX_ID_LEN)
            ),
            Field {
                go_name: "Fields",
                ty: GoType::Wire("nttObservationFieldsWire", fields_len),
                offset: FIELDS_START,
                size: fields_len,
            },
        ],
    );
    go.wire_struct(
        "nttSubmitObservationsInstructionWire is the discriminator and NttSubmitObservationsIxData.",
        "nttSubmitObservationsInstructionWire",
        "ntt submit_observations instruction data",
        "nttSubmitObservationsInstructionLen",
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
                    "nttSubmitObservationsIxDataWire",
                    NttSubmitObservationsIxData::LEN,
                ),
                offset: DISPATCH_LEN,
                size: NttSubmitObservationsIxData::LEN,
            },
        ],
    );

    go.section("TransceiverHubLayout and TransceiverPeerLayout, state.rs.");
    go.constants(&[
        (
            "TransceiverHubLayout::LEN.",
            "transceiverHubLen",
            n(TransceiverHubLayout::LEN),
        ),
        (
            "TransceiverHubLayout::TAG.",
            "transceiverHubTag",
            u64::from(TransceiverHubLayout::TAG),
        ),
        (
            "TransceiverPeerLayout::LEN.",
            "transceiverPeerLen",
            n(TransceiverPeerLayout::LEN),
        ),
        (
            "TransceiverPeerLayout::TAG.",
            "transceiverPeerTag",
            u64::from(TransceiverPeerLayout::TAG),
        ),
    ]);
    go.wire_struct(
        "transceiverHubWire is TransceiverHubLayout.",
        "transceiverHubWire",
        "transceiver hub account",
        "transceiverHubLen",
        TransceiverHubLayout::LEN,
        0,
        &[
            field!(TransceiverHubLayout, tag, "Tag", GoType::U8),
            field!(TransceiverHubLayout, chain, "Chain", GoType::U16),
            field!(TransceiverHubLayout, hub_chain, "HubChain", GoType::U16),
            field!(TransceiverHubLayout, address, "Address", GoType::Bytes(32)),
            field!(
                TransceiverHubLayout,
                hub_address,
                "HubAddress",
                GoType::Bytes(32)
            ),
        ],
    );
    go.wire_struct(
        "transceiverPeerWire is TransceiverPeerLayout.",
        "transceiverPeerWire",
        "transceiver peer account",
        "transceiverPeerLen",
        TransceiverPeerLayout::LEN,
        0,
        &[
            field!(TransceiverPeerLayout, tag, "Tag", GoType::U8),
            field!(TransceiverPeerLayout, chain, "Chain", GoType::U16),
            field!(TransceiverPeerLayout, dest_chain, "DestChain", GoType::U16),
            field!(TransceiverPeerLayout, address, "Address", GoType::Bytes(32)),
            field!(
                TransceiverPeerLayout,
                peer_address,
                "PeerAddress",
                GoType::Bytes(32)
            ),
        ],
    );

    go.section("PDA seed prefixes, constants/seeds.rs.");
    go.seeds(&[
        (
            "TRANSCEIVER_HUB_SEED_PREFIX.",
            "transceiverHubSeedPrefix",
            TRANSCEIVER_HUB_SEED_PREFIX,
        ),
        (
            "TRANSCEIVER_PEER_SEED_PREFIX.",
            "transceiverPeerSeedPrefix",
            TRANSCEIVER_PEER_SEED_PREFIX,
        ),
    ]);

    go.section("GlobalAccountantError codes the NTT submit path branches on, error.rs.");
    for (name, code) in [
        (
            "solanaErrInvalidInstructionData",
            GlobalAccountantError::InvalidInstructionData,
        ),
        (
            "solanaErrMalformedNttMessage",
            GlobalAccountantError::MalformedNttMessage,
        ),
        (
            "solanaErrMissingTransceiverHub",
            GlobalAccountantError::MissingTransceiverHub,
        ),
        (
            "solanaErrMissingSourcePeer",
            GlobalAccountantError::MissingSourcePeer,
        ),
        (
            "solanaErrMissingDestinationPeer",
            GlobalAccountantError::MissingDestinationPeer,
        ),
        (
            "solanaErrPeersNotCrossRegistered",
            GlobalAccountantError::PeersNotCrossRegistered,
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

/// The observation a guardian builds for one corpus row, checked against what wormchain
/// committed for it.
fn observation_from_vector(corpus: &NttCorpus, v: &NttVector) -> Observation {
    let body = v.body();
    let (header, payload) = VaaBodyHeader::split(body).expect("corpus row has a valid VAA header");
    assert_eq!(header.emitter_chain(), v.chain, "{}", v.label());
    assert_eq!(header.emitter_address, v.emitter, "{}", v.label());
    assert_eq!(header.sequence(), v.sequence, "{}", v.label());

    let (sender, message) = if v.via_relayer {
        let delivery = parse_delivery_instruction(payload).expect("relayed row unwraps");
        (delivery.sender, delivery.inner_payload)
    } else {
        (v.emitter, payload)
    };
    let transfer = parse_native_token_transfer(message).expect("corpus row is a transfer");
    let trimmed_amount = u64::from_be_bytes(transfer.amount);
    let recipient_chain = u16::from_be_bytes(transfer.to_chain);

    let digest = double_keccak256(body);
    assert_eq!(digest, v.expected_digest, "{} digest", v.label());
    assert_eq!(
        normalize_trimmed_amount(transfer.decimals, trimmed_amount)
            .expect("amount normalizes")
            .0,
        v.expected_amount,
        "{} amount",
        v.label()
    );
    assert_eq!(
        recipient_chain,
        v.expected_recipient_chain,
        "{} recipient chain",
        v.label()
    );
    assert_eq!(
        corpus.hub_for(v.chain, sender),
        Some((v.expected_token_chain, v.expected_token_address)),
        "{} hub",
        v.label()
    );

    Observation {
        chain: v.chain,
        emitter: v.emitter,
        sequence: v.sequence,
        sender,
        recipient_chain,
        trimmed_decimals: transfer.decimals,
        trimmed_amount,
        digest,
    }
}

/// Discriminator plus `NttSubmitObservationsIxData`.
fn submit_observations_ix_data(ix: &NttSubmitObservationsIxData) -> Vec<u8> {
    let mut data = Vec::with_capacity(DISPATCH_LEN + NttSubmitObservationsIxData::LEN);
    data.push(NttInstruction::SubmitObservations as u8);
    data.extend_from_slice(bytemuck::bytes_of(ix));
    assert_eq!(data.len(), DISPATCH_LEN + NttSubmitObservationsIxData::LEN);
    data
}

/// Writes the real-data vectors of one corpus row, named `fixtureNtt<label>*`.
fn real_vectors(go: &mut GoFile, label: &str, corpus: &NttCorpus, v: &NttVector) -> Observation {
    let obs = observation_from_vector(corpus, v);
    let fields = obs.ix(0, 0, [0u8; 65], ZERO_TX_ID).fields_and_digest();
    assert_eq!(
        fields.len(),
        NttSubmitObservationsIxData::LEN - FIELDS_START,
        "the hashed record is the instruction data tail"
    );
    let (hub_chain, hub) = corpus.hub_for(obs.chain, obs.sender).expect("hub");
    let name = |suffix: &str| format!("fixtureNtt{label}{suffix}");

    go.hex_constant("", &name("BodyHex"), v.body());
    go.constant("", &name("Chain"), format!("uint16({})", obs.chain));
    go.hex_constant("", &name("EmitterHex"), &obs.emitter);
    go.constant("", &name("Sequence"), format!("uint64({})", obs.sequence));
    go.hex_constant("", &name("SenderHex"), &obs.sender);
    go.constant(
        "",
        &name("RecipientChain"),
        format!("uint16({})", obs.recipient_chain),
    );
    go.constant(
        "",
        &name("TrimmedDecimals"),
        format!("uint8({})", obs.trimmed_decimals),
    );
    go.constant(
        "",
        &name("TrimmedAmount"),
        format!("uint64({})", obs.trimmed_amount),
    );
    go.hex_constant("", &name("VaaDigestHex"), &obs.digest);
    go.hex_constant("", &name("FieldsHex"), &fields);
    go.hex_constant("", &name("ContentDigestHex"), &double_keccak256(&fields));
    go.constant("", &name("HubChain"), format!("uint16({hub_chain})"));
    go.hex_constant("", &name("HubAddressHex"), &hub);
    obs
}

pub(crate) fn fixtures_file() -> String {
    // The hex fixtures below embed these lengths.
    assert_eq!(NttSubmitObservationsIxData::LEN, 252);
    assert_eq!(TransceiverHubLayout::LEN, 70);
    assert_eq!(TransceiverPeerLayout::LEN, 70);

    let corpus = NttCorpus::load();
    for v in &corpus.vectors {
        observation_from_vector(&corpus, v);
    }
    let direct = corpus
        .vectors
        .iter()
        .find(|v| !v.via_relayer)
        .expect("a direct row");
    let relayed = corpus
        .vectors
        .iter()
        .find(|v| v.via_relayer)
        .expect("a relayed row");

    let mut go = GoFile::new();

    go.section("Mainnet NTT transfer published directly by the transceiver.");
    let direct_obs = real_vectors(&mut go, "Direct", &corpus, direct);
    go.section("Mainnet NTT transfer relayed through the Standard Relayer.");
    let obs = real_vectors(&mut go, "Relayed", &corpus, relayed);
    assert_eq!(direct_obs.sender, direct_obs.emitter);
    assert_ne!(obs.sender, obs.emitter);

    // Index-derived bytes make byte-order bugs in the Go decoder visible.
    let program_id = Pubkey::new_from_array(core::array::from_fn(|i| i as u8));
    let peer: [u8; 32] = core::array::from_fn(|i| 0x20 + i as u8);
    let signature: [u8; 65] = core::array::from_fn(|i| i as u8);
    let hash_tx_id: [u8; 32] = core::array::from_fn(|i| 0xE0 + i as u8);
    let signature_tx_id: [u8; 64] = core::array::from_fn(|i| 0xA0 + i as u8);
    let guardian_index: u8 = 3;

    // The relayed row has a sender that differs from the emitter, so a swapped seed shows.
    go.section("Relayed row routing: program id 0x00.., peer 0x20...");
    go.hex_constant("", "fixtureNttPeerHex", &peer);
    let (hub_chain, hub) = corpus.hub_for(obs.chain, obs.sender).expect("hub");
    let content = double_keccak256(&obs.ix(0, 0, [0u8; 65], ZERO_TX_ID).fields_and_digest());
    let (pending_pda, _) = derive_pending_pda(
        &program_id,
        obs.chain,
        &obs.emitter,
        obs.sequence,
        GUARDIAN_SET_INDEX,
        &content,
    );
    go.hex_constant(
        "Guardian set index 4.",
        "fixtureNttPendingPDAHex",
        pending_pda.as_array(),
    );
    go.constant(
        "",
        "fixtureNttGuardianSetIndex",
        format!("uint32({GUARDIAN_SET_INDEX})"),
    );
    let hub_key = TransceiverHubKey::new(obs.chain, obs.sender);
    go.hex_constant(
        "",
        "fixtureNttHubPDAHex",
        pda::derive(&program_id, &hub_key).0.as_array(),
    );
    let peer_src_key = TransceiverPeerKey::new(obs.chain, obs.sender, obs.recipient_chain);
    go.hex_constant(
        "",
        "fixtureNttPeerSrcPDAHex",
        pda::derive(&program_id, &peer_src_key).0.as_array(),
    );
    let peer_dst_key = TransceiverPeerKey::new(obs.recipient_chain, peer, obs.chain);
    go.hex_constant(
        "",
        "fixtureNttPeerDstPDAHex",
        pda::derive(&program_id, &peer_dst_key).0.as_array(),
    );
    go.hex_constant(
        "",
        "fixtureNttSourceBalancePDAHex",
        balance::derive_pda(&program_id, obs.chain, hub_chain, &hub)
            .0
            .as_array(),
    );
    go.hex_constant(
        "",
        "fixtureNttDestBalancePDAHex",
        balance::derive_pda(&program_id, obs.recipient_chain, hub_chain, &hub)
            .0
            .as_array(),
    );
    go.hex_constant(
        "",
        "fixtureNttRelayerRegistrationPDAHex",
        chain_registration::derive_pda(&program_id, obs.chain)
            .0
            .as_array(),
    );

    go.section("Account images of the relayed row's hub and source peer.");
    go.hex_constant(
        "",
        "fixtureNttHubAccountHex",
        bytemuck::bytes_of(&hub_layout(obs.chain, obs.sender, hub_chain, hub)),
    );
    go.hex_constant(
        "",
        "fixtureNttPeerSrcAccountHex",
        bytemuck::bytes_of(&peer_layout(
            obs.chain,
            obs.sender,
            obs.recipient_chain,
            peer,
        )),
    );

    go.section(
        "submit_observations data and signing digest for the relayed row, guardian set 4, guardian index 3.",
    );
    for (suffix, doc, tx_id) in [
        ("Hash", "32-byte tx id.", TxId::Hash(&hash_tx_id)),
        (
            "Signature",
            "64-byte tx id.",
            TxId::Signature(&signature_tx_id),
        ),
    ] {
        let ix = obs.ix(GUARDIAN_SET_INDEX, guardian_index, signature, tx_id);
        let digests = observation_digests(
            NTT_SUBMIT_OBSERVATION_PREFIX,
            tx_id,
            &ix.fields_and_digest(),
        );
        assert_eq!(
            digests.content, content,
            "the content digest does not depend on the tx id"
        );
        go.hex_constant(
            doc,
            &format!("fixtureNtt{suffix}TxIDIxDataHex"),
            &submit_observations_ix_data(&ix),
        );
        go.hex_constant("", &format!("fixtureNtt{suffix}TxIDHex"), tx_id.as_bytes());
        go.hex_constant(
            "",
            &format!("fixtureNtt{suffix}TxIDSigningDigestHex"),
            &digests.signing,
        );
    }

    go.finish()
}
