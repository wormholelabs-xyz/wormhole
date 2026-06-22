# Workstream D (D1–D4 transfer flow): NTT submit_observations + submit_vaas

Implements the NTT operational transfer flow per the frozen D0 spec in
`ntt-accountant-migration.md`. NTT gets its OWN orchestration over shared
`operational-core` LEAF helpers; it does NOT call the WTT-shaped core
orchestration (whose account layout + chain-registration check are WTT-specific).

## Step 1 — factor core leaf helpers into `pub`

New `crates/operational-core/src/instructions/quorum.rs` (`pub mod quorum`) holds
the product-neutral quorum primitives extracted out of `submit_observations.rs`:

- `verify_signature` / `read_guardian_key` / `secp256k1_recover` (sig verify vs GuardianSet PDA).
- `ParsedObservation` (now `pub`, with `pub` fields) + `from_data` + `populate_routing_from_body`.
- `PendingAction` enum + `decide_pending_action` + `create_pending_pda` + `wipe_pending_pda`.
- `accumulate_signature` — the bitmap toggle + quorum-threshold check, returning
  whether quorum was reached.
- consts (`SUBMIT_FIXED_LEN`, `SECP256K1_SIGNATURE_LEN`, …).

Make `pub`: `commit_log::emit`, `submit_observations::close_pending_pda` (move to quorum).
`noreplay::{is_marked,mark_used,derive_bucket_pda}`, `pda_init::init_or_upgrade_pda`,
`shim::verify_vaa`, `hash::double_keccak256`, `state::{pending,account}` are already `pub`.

WTT's `submit_observations::process` / `submit_vaas::process` are refactored to CALL
the `quorum` helpers (behavior-preserving — WTT mollusk suite proves it). WTT keeps its
own account layout + chain-registration check inline.

## Step 2 — NTT submit_observations + submit_vaas

`programs/ntt-global-accountant/src/instructions/{submit_observations.rs,submit_vaas.rs,ntt_transfer.rs}`.

NTT account layout (documented in code):
quorum accounts mirror WTT slots 0..6 minus the chain-registration slot:
  0 submitter, 1 pending_pda, 2 guardian_set, 3 noreplay_bucket, 4 system,
  5 noreplay_program, 6 noreplay_authority, 7 rent_recipient,
then NTT transfer accounts:
  8 relayer_registration_pda, 9 transceiver_hub_pda, 10 transceiver_peer_src_pda,
  11 transceiver_peer_dst_pda, 12 source_balance, 13 dest_balance.

submit_vaas account layout:
  0 submitter, 1 verify_vaa_shim_program, 2 guardian_set, 3 guardian_signatures,
  4 noreplay_bucket, 5 noreplay_program, 6 noreplay_authority, 7 system,
then NTT transfer accounts 8..13 as above.

ntt_transfer::apply_ntt_transfer runs the D0 flow:
  1. relayer-unwrap (relayer_registration PDA addr == emitter_address ⇒ DeliveryInstruction unwrap)
  2. hub-substitute (transceiver_hub PDA at (chain, sender) ⇒ (hub_chain, hub_address))
  3. parse_ntt_transfer ⇒ (amount, recipient_chain)
  4. peer cross-check (two transceiver_peer PDAs)
  5. apply accounting against (hub_chain, hub_address) token identity.

## Step 3 — tests

`programs/ntt-global-accountant/tests/submit_observations.rs`:
- happy-path transfer accounting (13 guardians to quorum, balances credited the
  normalized amount against the HUB token identity);
- missing-hub rejection;
- peer-cross-registration-fail rejection.

## Status: COMPLETE

- `quorum.rs` factored; WTT `submit_observations`/`submit_vaas` call it (WTT suite green).
- NTT `submit_observations`/`submit_vaas`/`ntt_transfer` implemented; entrypoint un-stubbed.
- Errors 30/31/32 added (MissingTransceiverHub / MissingTransceiverPeer / PeerRegistrationMismatch).
- `cargo check --offline --workspace` clean; `just clippy` clean; my files fmt-clean.
- `just test`: 136 host tests pass, 0 failed (WTT submit_observations 32/32 + submit_vaas 10/10
  UNCHANGED; NTT submit_observations 4/4; definitions 52/52; all others green).
