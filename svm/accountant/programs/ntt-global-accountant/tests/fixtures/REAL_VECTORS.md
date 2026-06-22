# NTT accounting-parity corpus — format and ground-truth procedure

The corpus `ntt_parity_vectors.json` drives `tests/parity.rs`, which replays each
vector through the NTT operational program and asserts the resulting balance
accounts. This document specifies the corpus format, records that the current
vectors are **synthetic**, and gives the procedure for adding **real**
ground-truth vectors. The harness consumes synthetic and real vectors
identically — a real vector is just a corpus entry whose expected outcome was
produced by another implementation rather than hand-computed.

## What this corpus does and does not prove

These vectors are **synthetic, hand-computed-expectation** data. They validate
the program against the frozen D0 transfer-flow spec (the `ntt-accountant-migration.md`
Workstream D0 section): a spec-conformance check plus a regression guard, and they
close the relayer-unwrap end-to-end coverage gap (previously only the parser unit
tests in `crates/definitions/src/ntt.rs` exercised the `DeliveryInstruction`
branch — never the full observation → hub-substitute → account flow).

They are **not** cross-implementation parity. True byte-for-byte parity requires
**real NTT VAAs** replayed with **CosmWasm-produced expected outcomes**. Until such
vectors are sourced, the corpus proves the SVM program self-consistently matches
the spec we wrote, not that it matches the CosmWasm contract on real traffic.

## Corpus format

Top-level: `{ "schema_version": 1, "comment": "...", "vectors": [ ... ] }`.

Each vector:

| field | type | meaning |
|---|---|---|
| `name` | string | unique label; appears in assertion messages |
| `description` | string | which parity path the vector exercises |
| `source` | string | `synthetic`, `cosmwasm-test`, or `wormholescan` (provenance; see the source-guard test) |
| `emitter_chain` | u16 | VAA emitter chain == source chain |
| `emitter_address` | 0x..32 | VAA emitter; the **transfer/digest/NoReplay KEY** (left-aligned, right-zero-padded if short) |
| `sequence` | u64 | VAA sequence |
| `relayer` | object or `null` | when set, the body payload is a `DeliveryInstruction` wrapping the NTT message |
| `relayer.registered_emitter` | 0x..32 | the emitter registered as a relayer for `emitter_chain`; must equal `emitter_address` to trigger the unwrap |
| `relayer.sender` | 0x..32 | the inner `DeliveryInstruction.sender_address` — the **hub/peer routing key** |
| `ntt.decimals` | u8 | `TrimmedAmount` decimals |
| `ntt.raw_amount` | u64 | `TrimmedAmount` amount (pre-normalization) |
| `ntt.to_chain` | u16 | `NativeTokenTransfer.to_chain` (recipient chain) |
| `hub.chain` | u16 | hub token-identity chain — the accounted `token_chain` |
| `hub.address` | 0x..32 | hub token-identity address — the accounted `token_address`; **never** the NTT `source_token` |
| `peers.src_to_dst` | 0x..32 | peer transceiver on the recipient chain (forward `TransceiverPeer`) |
| `peers.dst_to_src` | 0x..32 | reverse `TransceiverPeer.peer_address`; must equal the routing `sender` to pass the cross-registration check |
| `pre_balances` | array | balance accounts pre-seeded before the replay (needed for wrapped-burn debits) |
| `expected.balances` | array | the EXACT post-state balance accounts |

`pre_balances` / `expected.balances` entries are
`{ chain: u16, token_chain: u16, token_address: 0x..32, amount: decimal-string }`.
`amount` is a base-10 string (parsed into the program's 256-bit `Uint256`; the
current corpus stays within `u128`).

### Routing semantics encoded by the harness

- Hub PDA is keyed by `(emitter_chain, sender)` where `sender` is the
  relayer-resolved address (== `emitter_address` for a non-relayer vector).
- Balance accounts are keyed by the HUB identity: source `(emitter_chain,
  hub.chain, hub.address)`, dest `(to_chain, hub.chain, hub.address)`.
- Native vs wrapped is decided by `source_chain == hub.chain`: equal ⇒ source is
  a native lock (credit), else a wrapped burn (debit). The destination is always
  an unlock/mint (credit). This mirrors the program's `apply_balances`.

### Adding a vector

Append an object to `vectors`. No harness code change is needed — `tests/parity.rs`
is data-driven. Pick `source` honestly (`synthetic` unless the expected outcome was
produced by another implementation). The source-guard test
(`ntt_parity_corpus_sources_are_documented`) will flag the first non-synthetic
vector so the "current vectors are synthetic" status here gets updated.

## Current vectors (all synthetic)

| name | path exercised |
|---|---|
| `native_transfer_lock_unlock` | native: hub chain == source chain → source lock (credit), dest unlock/mint (credit) |
| `wrapped_transfer_burn_mint` | wrapped: hub on a third chain → source burn (debit, pre-seeded), dest mint (credit) |
| `normalize_scale_up_identity_at_eight` | normalization identity at 8 decimals |
| `normalize_scale_down_truncation_18_decimals` | scale-down 18→8 with truncation (sub-1e10 remainder lost, not rounded) |
| `relayer_unwrapped_delivery_instruction` | relayer gap: emitter is a registered relayer; routing uses unwrapped `sender`, key uses VAA `emitter` |

> A scale-UP vector with non-identity scaling (3→8) is also covered implicitly:
> `native_transfer_lock_unlock`, `wrapped_transfer_burn_mint`, and the relayer
> vector all use `decimals=3, raw_amount=1000` → `100_000_000` (×10^5).

## Procedure to add REAL ground-truth vectors

A real vector pairs a real NTT VAA body with an expected outcome that another
implementation produced. Two sourcing routes:

### Route A — real VAA + CosmWasm-derived expectation

1. Obtain a real NTT VAA body. Source it from wormholescan
   (`https://wormholescan.io` → a known NTT transfer tx → "rawdata" → the VAA
   hex), or from a guardian archive. Strip the VAA envelope (version, guardian-set
   index, signatures) down to the **body** (`timestamp..payload`); the harness
   keys/digests over the body.
2. Derive the expected balance deltas from the CosmWasm NTT global accountant.
   Either replay the VAA through the CosmWasm contract's test app
   (`cosmwasm/contracts/ntt-global-accountant/tests/`, the `cw-multi-test`
   `Contract` harness in `tests/helpers/mod.rs`) and read back the
   `accountant/accounts` state, or read the post-state from a wormchain snapshot
   for that transfer's `(chain, token_chain, token_address)` accounts.
3. Translate into a corpus vector: set `emitter_*`/`sequence` from the body
   header, `ntt.*` from the inner `NativeTokenTransfer`, `hub.*` from the
   `transceiver_to_hub` mapping that applied at that height, `peers.*` from
   `transceiver_peers`, and `expected.balances` from the CosmWasm post-state.
   Mark `source: "wormholescan"` (or `"cosmwasm-test"` if derived from the suite).

### Route B — port a CosmWasm test fixture directly

The CosmWasm suite (`cosmwasm/contracts/ntt-global-accountant/tests/`) builds its
transfer observations programmatically via `serde_wormhole` + `Amount`/`Address`
builders rather than as raw NTT `TransceiverMessage` bytes, so its transfer cases
are not directly liftable as NTT wire vectors. Use Route A for transfers.

## Known real artifact (DeliveryInstruction) — and why it is not yet an e2e vector

`cosmwasm/contracts/ntt-global-accountant/src/structs/relayer.rs` carries a **real
testnet DeliveryInstruction** (wormholescan tx
`0xb6de172a31d41e0aff2c928c4601d5328b05ccfd382ad853d0b1a61c3bfed869`, TESTNET).
Decoded against the program's `parse_delivery_instruction`:

- VAA emitter: chain `10003`, address `0x..7b1bd7a6b4e61c2a123ac6bc2cbfc614437d0470`, sequence `259`.
- `DeliveryInstruction.sender_address` = `0x..e493cc4f069821404d272b994bb80b1ba1631914` (the hub/peer routing key).
- inner `NativeTokenTransfer`: `decimals=8, amount=1000` (`0x03e8`) ⇒ normalized `1000`; `to_chain=10002`.

The **DeliveryInstruction layer parses cleanly** through the program's parser
(`sender`, inner payload, `num_messages=0`, strict no-trailing-bytes — all
confirmed). However, the inner **`TransceiverMessage` does not match the program's
current `parse_ntt_transfer` layout**: this older testnet message has the shape
`prefix(4) ‖ source_ntt_manager(32) ‖ ntt_manager_payload_len(u16) ‖ NttManagerMessage ‖ …`,
whereas the program expects a newer
`prefix(4) ‖ source_ntt_manager(32) ‖ recipient_ntt_manager(32) ‖ ntt_manager_payload_len(u16) ‖ …`
(the `994e5454` NTT prefix lands at body offset 80 in the real message vs the
offset-136 the parser expects). The program would therefore reject this specific
historical message.

**TODO (parity / spec ambiguity):** confirm which `TransceiverMessage` layout the
mainnet NTT corpus actually uses at the migration cutover height (the
`ntt-messages` crate revision pinned in D0 vs. this older testnet shape). If both
appear in the real corpus, the parser must accept both — at which point this
testnet VAA becomes a genuine `source: "wormholescan"` end-to-end vector. Until
resolved, the relayer-unwrap is covered end-to-end only by the synthetic
`relayer_unwrapped_delivery_instruction` vector (program-parser-conformant inner
message), and the real DeliveryInstruction is documented here rather than
fabricated into the corpus.
