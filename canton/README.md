# Wormhole Core Bridge & Watcher for Canton

This directory contains the Wormhole **core bridge** for the
[Canton Network](https://www.canton.network/) (implemented in
[Daml](https://docs.daml.com/)) and documents the guardian-node **watcher** that
observes it (implemented in Go under
[`node/pkg/watchers/canton`](../node/pkg/watchers/canton) with a Ledger-API
client under [`node/pkg/cantonclient`](../node/pkg/cantonclient)).

**Target deployment:** the **public Canton Network mainnet** — Canton **3.5.x**
protocol, **Daml SDK 3.5.1** (Daml-LF **2.3**, Protocol Version **35**). Two
consequences shape the design: Ledger API **v2**, and **contract keys that are
non-unique** (Canton keys give stable lookup but do not enforce uniqueness — so
identity uniqueness is registry-enforced and address integrity is cryptographic;
see §1, §4).

The goal is parity with the canonical EVM core bridge
([`ethereum/contracts/Implementation.sol`](../ethereum/contracts/Implementation.sol),
[`Messages.sol`](../ethereum/contracts/Messages.sol),
[`Governance.sol`](../ethereum/contracts/Governance.sol)) and conformance with
the Wormhole whitepapers:

- [0001 — Generic Message Passing](../whitepapers/0001_generic_message_passing.md)
- [0002 — Governance Messaging](../whitepapers/0002_governance_messaging.md)
- [0004 — Message Publishing](../whitepapers/0004_message_publishing.md)
- [node watcher README](../node/pkg/watchers/README.md)

> **Status:** initial implementation. The Daml packages **compile and tests pass
> on Daml SDK 3.5.1**, including on-chain VAA verification validated against a
> real guardian-signed VAA and the cross-language address derivation pinned by a
> shared test vector. The remaining mainnet-gating item — the exact Ledger-API
> protobuf surface — plus the deployment/observer topology are tracked in
> [Open Questions & Validation Items](#11-open-questions--validation-items).

---

## 1. Background: how Canton differs from EVM

Wormhole's core bridge has two responsibilities on every chain:

1. **Publish** messages — an on-chain caller emits a message; the guardians
   observe it, reach quorum, and produce a signed VAA.
2. **Verify** messages (VAAs) on-chain — needed so the chain can act on
   governance VAAs (guardian-set upgrades, fees, upgrades) and so higher-level
   protocols (token bridge, etc.) can verify inbound VAAs.

Canton is structurally unlike EVM/Solana/Sui, and those differences drive the
design:

| Concept                   | EVM                                         | Canton                                                                                                                                                 |
| ------------------------- | ------------------------------------------- | ------------------------------------------------------------------------------------------------------------------------------------------------------ |
| Smart-contract model      | Solidity contract with mutable storage      | Daml **templates**; state is the set of **active contracts** (UTXO-like). State is changed by **archiving** a contract and **creating** its successor. |
| Calling a method          | `CALL` to an address                        | **Exercising a choice** on a contract                                                                                                                  |
| Identity / "address"      | 20-byte account/contract address            | **Party ID** (a string `hint::fingerprint`) and **contract IDs**                                                                                       |
| Event log                 | `LOG`/`event` topics                        | Ledger-API **events** (`CreatedEvent`, `ExercisedEvent`) in the transaction stream                                                                     |
| Block / height            | block number                                | **participant offset** (monotone `int64`) into the update stream                                                                                       |
| Finality                  | probabilistic (reorgs) → consistency levels | deterministic: once a transaction is committed by the sequencer/mediator it is final. No reorgs.                                                       |
| Node API the watcher uses | JSON-RPC / `eth_getLogs`                    | **Ledger API v2** (gRPC), `UpdateService` streaming                                                                                                    |
| Crypto available on-chain | `ecrecover`, `keccak256` precompiles        | Daml `keccak256` and `secp256k1` built-ins (see §5)                                                                                                    |

The most consequential differences:

- **No `msg.sender`.** A Daml choice knows its acting parties, but Wormhole
  needs a stable 32-byte emitter address with a per-emitter monotonically
  increasing sequence. We solve this with an **`Emitter` registration**
  contract (§4.2), analogous to Sui's `EmitterCap`. The emitting party (`owner`)
  co-signs the `Emitter` and is part of the address derivation, so it is the
  faithful Canton analog of `msg.sender`: only that party's key can emit under
  its address (§4.2, §10).
- **No `ecrecover`.** Daml's `secp256k1` builtin _verifies_ a signature against
  a _public key_; it does not _recover_ the signer from the signature like
  Ethereum does. Guardian-set state on Canton therefore stores guardian
  **public keys** (not just their 20-byte addresses), bound to the canonical
  addresses from the governance VAA (§5).
- **No global mutable singletons; contract keys are non-unique.** The "core
  bridge contract" is modeled as a single long-lived **`CoreState`** contract,
  archived and recreated on every governance transition. Contract keys exist on
  Canton 3.5 (Daml-LF 2.3) but are **not unique** — multiple active contracts may
  share a key ([docs](https://docs.canton.network/appdev/modules/m3-contract-keys#contract-keys)).
  So keys give ergonomics (stable lookup / `ExerciseByKeyCommand`), not
  uniqueness: the `CoreState` is **co-signed by the `operator` and a
  `guardianGovernance` party** (a k-of-n threshold party) and keyed by the latter,
  so the operator cannot forge the guardian set (§4.1); per-emitter uniqueness is
  a **registry** that allocates ids plus an owner-bound, cryptographic address
  (§4.2).

---

## 2. Architecture overview

```mermaid
flowchart TD
    subgraph canton["Canton participant node — Daml: Wormhole.Core package"]
        direction TB
        integrator(["integrator party"])
        core["CoreState<br/>(operator-issued singleton)<br/>archives &amp; recreates on each transition"]
        gov["governance choices:<br/>GuardianSetUpgrade · SetMessageFee<br/>TransferFees · ContractUpgrade"]
        evt(["ExercisedEvent(PublishMessage)<br/>result = WormholeMessage{seq, nonce, payload, …}"])
        integrator -- "exercise PublishMessage" --> core
        gov -- "SubmitGovernanceVAA" --> core
        core -- emits --> evt
    end

    subgraph gd["guardiand"]
        direction TB
        client["node/pkg/cantonclient (gRPC client)<br/>• GetLedgerEnd → readiness / height<br/>• GetUpdates (stream) → WormholeMessage events<br/>• GetUpdateByOffset → reobservation"]
        watcher["node/pkg/watchers/canton (Watcher)<br/>decode event → common.MessagePublication → msgC"]
        client --> watcher
    end

    evt -- "Ledger API v2 (gRPC, UpdateService stream)" --> client
    watcher --> proc["guardian processor → quorum → signed VAA"]
```

The watcher is **read-only**: it never submits to Canton. It observes the
`PublishMessage` choice's result event, maps it to a
[`common.MessagePublication`](../node/pkg/common/chainlock.go), and hands it to
the processor exactly like every other watcher.

---

## 3. The VAA and the digest (unchanged across chains)

Canton uses the standard VAA v1 body and digest. Quoting
[0001](../whitepapers/0001_generic_message_passing.md) and matching
`Messages.sol`:

```
body = timestamp(4) ‖ nonce(4) ‖ emitterChain(2) ‖ emitterAddress(32)
       ‖ sequence(8) ‖ consistencyLevel(1) ‖ payload(var)

digest = keccak256( keccak256( body ) )          // double keccak, Ethereum-style
```

Guardians sign `digest` with secp256k1 (Ethereum ECDSA, 65-byte `r‖s‖v`).
Replay protection keys off `keccak256(body)` (the inner hash) exactly as
elsewhere in Wormhole. The on-chain Daml verifier reconstructs and re-hashes the
body and checks signatures against the stored guardian set (§5).

`ChainID` for Canton is **72** (`ChainIDCanton`), the next free mainnet ID after
Arc (71). It is registered in [`sdk/vaa/structs.go`](../sdk/vaa/structs.go).

---

## 4. On-chain design (Daml)

Daml package name: `wormhole-core` (the `core` package). Module layout under
[`core/daml/`](core/daml):

| Module                      | Responsibility                                              |
| --------------------------- | ----------------------------------------------------------- |
| `Wormhole.Core.Bytes`       | byte/hex helpers, big-endian integer (de)serialization      |
| `Wormhole.Core.VAA`         | VAA struct, parser, digest, signature/quorum verification   |
| `Wormhole.Core.GuardianSet` | guardian-set representation and expiry                      |
| `Wormhole.Core.State`       | the `CoreState` singleton + `EmitterRegistry` and `Emitter` |
| `Wormhole.Core.Governance`  | governance packet parsing and the governance choices        |
| `Wormhole.Core.Setup`       | initialization (`setup`)                                    |

### 4.1 `CoreState` — the singleton

```haskell
template CoreState
  with
    operator           : Party          -- advances the singleton; the "deployer"
    guardianGovernance : Party          -- guardians' governance anchor (threshold party)
    chainId            : Int            -- 72
    governanceChainId  : Int            -- 1 (Solana)
    governanceContract : Bytes32        -- 0x..0004 (the governance emitter)
    guardianSetIndex   : Int            -- current set index
    guardianSets       : Map Int GuardianSet   -- index -> set (with expiry)
    messageFee         : Int            -- fee required to publish (native units)
    consumedGovernance : Set Bytes32    -- replay protection (keyed by digest)
  where
    signatory operator, guardianGovernance
    key guardianGovernance : Party   -- lookup by the governance anchor is authentic
    maintainer key
```

Every state-mutating choice is `controller`-checked (controller `operator`),
archives the current `CoreState`, and `create`s the next one with updated fields.
This is the Daml idiom for mutable state and gives us deterministic,
replay-protected transitions.

**Singleton co-signed by a governance party, keyed for authenticity.** The
`CoreState` is co-signed by the `operator` and a **`guardianGovernance`** party.
In production that is a **decentralized-namespace external party** controlled by a
k-of-n threshold of guardian keys — the same construction as the Canton Network's
DSO party — rotated at the topology layer without touching this template (the
party id, and thus the key and every consumer's lookup, is stable across guardian
rotation). Two things follow:

- **The operator cannot forge the guardian set.** Because `guardianGovernance` is
  a signatory *and* the key maintainer, a compromised operator cannot `create` a
  `CoreState` bearing the real governance party — so a lookup by the known
  `guardianGovernance` party (`fetchByKey`/`ExerciseByKeyCommand`) can only ever
  resolve an **authentic** `CoreState`. Its guardian set is thus cryptographically
  anchored, not operator-controlled. (Keys are still non-unique, so an operator
  puppet could create a `CoreState` under a *different* governance party — but it
  is not findable by the real anchor and no consumer trusts it.)
- **Governance stays low-friction.** `guardianGovernance` actively signs only at
  **genesis**; each `SubmitGovernanceVAA` transition inherits its authority from
  the archived contract (the same authority-propagation as the `Emitter` owner
  through `PublishMessage`), so the operator submits governance alone. "Exactly
  one live `CoreState`" is still behavioral (operator + governance create one at
  setup), but the set's *contents* are now anchored.

Per-emitter **sequence numbers are stored in each `Emitter`** (§4.2), not in
`CoreState`. This is deliberate: in Canton, a contract's choice can only be
exercised by a stakeholder of that contract, and the emitter owner is _not_ a
stakeholder of the operator-owned `CoreState`. Keeping the sequence in the
`Emitter` (where the owner _is_ a stakeholder) lets the owner publish — and
advance its own sequence — **without the operator's authority**, preserving
EVM's permissionless `publishMessage`. `CoreState` therefore holds only shared
config and governance state.

> **Why an `operator` party?** Canton requires every contract to have a
> signatory. The `operator` is a designated, well-known party (configured at
> deploy time) that owns the singleton. It has **no special authority** over
> message contents or governance outcomes — governance is gated entirely by VAA
> verification (§4.4). Its role is mechanical: it is the party that the daml
> transaction is submitted as in order to advance the singleton. This mirrors
> how non-EVM chains (e.g. Near) have an account that "owns" the bridge object
> without being able to forge messages.

### 4.2 Message publishing

Mirrors `publishMessage(nonce, payload, consistencyLevel)` from
`Implementation.sol:15` and whitepaper 0004.

An integrator first registers an emitter. The operator runs an `EmitterRegistry`
(created at setup, keyed by the operator) that allocates a stable integer id;
`ApproveEmitter` allocates one and creates the keyed `Emitter` in one transaction:

```haskell
template EmitterRequest with
    requester : Party
    operator  : Party
  where
    signatory requester
    observer operator
    -- operator exercises ApproveEmitter, which allocates an emitterId from the
    -- EmitterRegistry and creates the keyed Emitter in one transaction. The
    -- requester's signature on this request authorizes the owner co-signatory.

-- One per operator; allocates monotonically increasing emitter ids.
template EmitterRegistry with
    operator : Party
    nextId   : Int
  where
    signatory operator
    key operator : Party
    maintainer key
    -- choice AllocateEmitterId : (ContractId EmitterRegistry, Int)

template Emitter
  with
    operator       : Party
    owner          : Party
    emitterId      : Int      -- registry-allocated; key._3
    emitterAddress : Bytes32  -- keccak256(tag ‖ operator ‖ owner ‖ emitterId), set at creation
    sequence       : Int      -- next sequence to assign (per-emitter)
  where
    signatory operator, owner   -- owner co-signs: the msg.sender analog
    key (operator, owner, emitterId) : (Party, Party, Int)
    maintainer key._1
```

**Emitter address derivation.** The 32-byte Wormhole emitter address is

```
keccak256( utf8("wormhole:emitter:v1")
         ‖ uint32be(len(registrar)) ‖ utf8(registrar)
         ‖ uint32be(len(owner))     ‖ utf8(owner)
         ‖ uint64be(emitterId) )
```

where `registrar` is the operator. The 4-byte length prefixes make the preimage
injective across the two variable-length party ids. It is computed **on-ledger**
at allocation (Daml `DA.Crypto.Text` is stable in 3.5) and stored in
`emitterAddress` for integrators to read, but the guardian watcher **re-derives**
it from the `WormholeMessage` fields and is the sole source of truth — a corrupt
on-ledger value cannot spoof it.

**Impersonation is cryptographically prevented (the `msg.sender` guarantee).**
Because the `owner` is both a **signatory** of the `Emitter` and part of the
address preimage, only a transaction carrying the owner's authority can create a
contract whose address is the owner's. A compromised `operator` therefore cannot
emit under an existing emitter's address: an operator-alone forge is rejected in
guardian replay (missing the owner's signature), and a colluding duplicate under
a different owner derives a **different**, untrusted address. This is the
invariant guardians verify — "the transaction is Daml-valid under the vetted
package" — with no bespoke watcher logic (see [§10](#10-trust-model--deployment-topology)).
Registry-allocated ids give per-owner uniqueness and let one party hold **many**
emitters, matching Sui's multiple `EmitterCap`s; id-sequence discipline is
operator-behavioral, but address integrity is not.

Publishing — a choice on the **`Emitter`** (controller = `owner`), so it needs
no operator authority:

```haskell
-- on Emitter
choice PublishMessage : WormholeMessage
  with
    nonce            : Int            -- uint32
    payload          : Bytes          -- <= 750 bytes (whitepaper 0004)
    consistencyLevel : Int            -- uint8; see §6
  controller owner
  do
    -- 1. assert payload length <= 750
    -- 2. (fee enforcement deferred; messageFee = 0 in v1)
    -- 3. archive self; create Emitter with sequence = sequence + 1 (SAME key)
    -- 4. return WormholeMessage{registrar, owner, emitterId, sequence, nonce,
    --      payload, consistencyLevel} -- watcher derives the address
    pure result
```

The choice's **exercise result** is the `WormholeMessage`. The sequence-bumped
`Emitter` is recreated under the **same key** (both signatures inherited from the
consumed contract), so the owner publishes again via
`exerciseByKey`/`ExerciseByKeyCommand` — no contract-id tracking. The watcher
reads the result directly from the `ExercisedEvent.exercise_result` on the
Ledger-API stream (see §7). The `WormholeMessage` record is the wire contract
between Daml and the watcher:

```haskell
data WormholeMessage = WormholeMessage with
    registrar        : Party     -- Emitter key._1 (operator); address input
    owner            : Party     -- Emitter key._2; address input (anti-impersonation)
    emitterId        : Int       -- Emitter key._3 (registry-allocated); address input
    sequence         : Int       -- uint64
    nonce            : Int       -- uint32
    consistencyLevel : Int       -- uint8
    payload          : Bytes
  deriving (Eq, Show)
```

Equivalent to the EVM `LogMessagePublished(sender, sequence, nonce, payload,
consistencyLevel)` event (`Implementation.sol:12`), except the `sender` (emitter
address) is **derived by the watcher** from `(registrar, owner, emitterId)`
rather than carried in the record. The VAA **timestamp** is not carried in the
record either: per whitepaper 0001/0004 the guardian derives it from the block,
so the watcher reads it from the transaction's **ledger effective time**
(`Transaction.effective_at`) — see §7.1.

**Sequence numbers** are per-emitter (per key), start at 0, increment by
1 — identical to EVM's `_state.sequences[emitter]`.

**Fees.** Whitepaper 0004 requires a fee in the chain's native token. Canton's
native unit is Canton Coin (CC), held as Daml contracts (Splice
`amulet`). For the initial implementation `messageFee` defaults to **0** and the
`feePayment` argument is a no-op placeholder; wiring real CC payment/`TransferFees`
is deferred (see Open Questions). The `SetMessageFee`/`TransferFees` governance
choices still update/move the accounting state so on-chain behavior matches the
governance protocol.

### 4.3 VAA verification choice

```haskell
-- on CoreState; pure verification, does not mutate state
nonconsuming choice ParseAndVerifyVAA : VerifiedVAA
  with
    encodedVAA : Bytes
    pubKeys    : [(Int, Bytes)]   -- guardianIndex -> 65-byte uncompressed pubkey
  controller operator
  do
    vaa <- parseVAA encodedVAA
    verifyVAA self vaa pubKeys     -- §5
    pure (toVerified vaa)
```

`pubKeys` are supplied by the caller because Daml verifies against a public key,
not by recovery (§5). They are _untrusted hints_; verification fails unless each
provided key hashes to the guardian address stored in the set.

### 4.4 Governance

Governance follows [0002](../whitepapers/0002_governance_messaging.md). A single
`SubmitGovernanceVAA` choice on `CoreState` (controller `operator`) verifies the
VAA and dispatches on the parsed action. Module identifier is **`"Core"`**
(`0x00..00436f7265`, 32 bytes). A governance VAA is accepted only if (matching
`Governance.sol:190`):

1. `verifyVAA` passes against the **current** guardian set;
2. `emitterChain == governanceChainId` and `emitterAddress == governanceContract`;
3. `module == "Core"` and `action` matches the choice;
4. `chain == chainId` (or `0` for chain-independent actions);
5. `digest ∉ consumedGovernance` — then it is inserted (replay protection).

Supported actions (parity with `GovernanceStructs.sol`):

| Action                 | ID  | Payload (after 32-byte module + 1-byte action)   | Behavior                                                                                                                                                                                   |
| ---------------------- | --- | ------------------------------------------------ | ------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------ |
| **ContractUpgrade**    | 1   | `chain(2) ‖ newContract(32)`                     | Records intent + emits an event. On Canton, "upgrade" = a [Daml package upgrade](https://docs.daml.com/upgrade/); `newContract` is interpreted as the new package-id (32 bytes). See note. |
| **GuardianSetUpgrade** | 2   | `chain(2) ‖ newIndex(4) ‖ len(1) ‖ keys(20·len)` | Validates `newIndex == current+1`; sets old set's `expirationTime = effectiveTime + 86400`; stores new set. Requires `pubKeys` hints to bind addresses (§5).                               |
| **SetMessageFee**      | 3   | `chain(2) ‖ fee(32)`                             | Updates `messageFee`.                                                                                                                                                                      |
| **TransferFees**       | 4   | `chain(2) ‖ amount(32) ‖ recipient(32)`          | Moves collected fees to `recipient`. (No-op accounting until CC payments are wired.)                                                                                                       |

`RecoverChainId` (EVM action 5) is **EVM-specific** (it repairs `block.chainid`
after a fork) and has no analog on Canton; it is intentionally omitted.

**ContractUpgrade note:** EVM upgrades swap a proxy implementation. Daml has no
in-place code mutation; upgrading the bridge logic means deploying a new package
version and having the `operator` migrate the `CoreState` to the new package via
Daml's smart-contract upgrade (data-preserving). The governance VAA records the
authorized target package-id on-ledger; the operator's migration is then
gated by that recorded value. This is the faithful Canton analog and is the one
governance action whose mechanism differs most from EVM — see Open Questions.

### 4.5 Guardian-set expiry

Matches EVM (`Setters.sol`): when a new set is installed, the previous set's
`expirationTime` is set to `effectiveTime + 86400` (1 day). `verifyVAA` accepts a
non-current set only while `effectiveTime < expirationTime`. The current set
never expires.

---

## 5. On-chain signature verification (the crux)

EVM uses `ecrecover(digest, v, r, s)` and compares the recovered 20-byte address
to `guardianSet.keys[i]`. Daml's
[`DA.Crypto.Text`](https://docs.canton.network/appdev/reference/daml-standard-library/da-crypto-text)
has **no recovery** builtin; the relevant functions (confirmed against the
3.x docs) are:

```haskell
keccak256             : BytesHex -> BytesHex
secp256k1             : SignatureHex -> BytesHex -> PublicKeyHex -> Bool  -- SHA-256s the message first
secp256k1WithEcdsaOnly : SignatureHex -> BytesHex -> PublicKeyHex -> Bool  -- verifies the message directly
```

We use **`secp256k1WithEcdsaOnly`** because Ethereum signs the 32-byte
`keccak256(keccak256(body))` digest _directly_ (no further hashing); plain
`secp256k1` would SHA-256 the digest first and never match. The design:

1. **Bind a caller-supplied pubkey → guardian address.** There is no recovery,
   so the verifier is given `pubKeys : [(Int, Bytes)]` (the signing guardians'
   uncompressed keys) and asserts `keccak256(pubKey)[12..32] == keys[index]`.
   Because the address is the keccak of the key, a wrong key can't pass — this
   re-introduces the EVM guarantee without recovery.

2. **DER-encode for the builtin.** `secp256k1WithEcdsaOnly` wants a **DER**
   signature and a **DER (SubjectPublicKeyInfo)** public key, both hex.
   `Wormhole.Core.VAA` DER-encodes the VAA's raw `(r, s)` (`derSignature`) and
   wraps the uncompressed point in the fixed secp256k1 SPKI prefix
   (`derPublicKey`). The 32-byte digest is passed as the message **as-is**.

3. **Verify.** `secp256k1WithEcdsaOnly (derSignature r s) digest (derPublicKey pubKey)`.

4. **Quorum** — identical to EVM `Messages.sol:90`:
   `required = floor(numGuardians * 2 / 3) + 1`. Signatures must be in
   **strictly ascending** guardian-index order (prevents double-counting).

### Making verification hint-free for integrators (recommended)

Supplying `pubKeys` on every call is acceptable for governance (rare,
operator-submitted) but is real friction for integrators (e.g. a token bridge),
whose relayers would otherwise have to look up the signing guardians' keys on
every inbound message. The recommended evolution: **persist the guardian public
keys in the guardian set** — i.e. `GuardianSet` stores `pubKeys` alongside `keys`,
bound to the canonical addresses _once_ at install time (the
GuardianSetUpgrade / setup choices already receive pubkey hints and verify
`keccak256(pubKey)[12..32] == addr`, so the binding is free). After that,
`verifyVAA` reads the keys from the set and callers pass **only the VAA bytes**,
making the integrator API equivalent to EVM's `parseAndVerifyVM(bytes)`. This is
a planned change to the current code, which still takes per-call hints.

### Validation status

The whole verification path is **empirically validated** against a real
Ethereum-style guardian signature on Daml SDK 3.5.1. `testParseAndVerifyVAA` and
`testGovernanceSetMessageFee` (in [`TestCore.daml`](test/daml/Test/TestCore.daml))
feed a genuine VAA — signed by the well-known devnet guardian and produced by
[`node/pkg/cantonclient/vectorgen_test.go`](../node/pkg/cantonclient/vectorgen_test.go)
— through `ParseAndVerifyVAA` / `SubmitGovernanceVAA` and both pass. That confirms:

- ✅ `keccak256` hashes the **decoded** hex bytes (the double-keccak digest
  matched the signature), so the `BytesHex` semantics are as assumed;
- ✅ `derSignature` (DER `r,s`) and `derPublicKey` (SPKI) are encoded correctly;
- ✅ `secp256k1WithEcdsaOnly` verifies the 32-byte digest **directly** (no
  pre-hash), and the pubkey→address keccak binding holds.

As of Daml SDK 3.5 the `DA.Crypto.Text` builtins are **stable** (the earlier
Alpha `-Wno-crypto-text-is-alpha` suppression is no longer needed), so the
previous mainnet-readiness concern about depending on an Alpha builtin is
resolved. All Daml crypto remains encapsulated in `Wormhole.Core.VAA`, and the
same builtins now also back the on-ledger address derivation in
`Wormhole.Core.Bytes` (`toHex` + `keccak256`).

---

## 6. Consistency / finality

Canton transactions are final once committed by the synchronizer
(sequencer + mediator); there are no reorgs. Like Sui, Canton therefore has a
single meaningful consistency level. The watcher treats every observed message
as final and publishes immediately. The `consistencyLevel` byte is passed
through from the `WormholeMessage` (integrators may set it; per
[0001](../whitepapers/0001_generic_message_passing.md) "all other chains … this
field will be `0`"), and the watcher does **not** gate on it.

---

## 7. Off-chain design (the watcher)

Lives in [`node/pkg/watchers/canton`](../node/pkg/watchers/canton); the gRPC
client is [`node/pkg/cantonclient`](../node/pkg/cantonclient). It mirrors the
structure of the Sui watcher
([`node/pkg/watchers/sui`](../node/pkg/watchers/sui)) and the `suiclient`
package.

### 7.1 `cantonclient` (Ledger API v2, gRPC)

There is no official Go binding for the Daml Ledger API, so the
`com.daml.ledger.api.v2` protobuf (the State + Update service closure, from
**Canton 3.5.5**) are vendored under
[`node/pkg/cantonclient/proto`](../node/pkg/cantonclient/proto) and compiled with
the repo's `buf` toolchain; the generated stubs are committed and CI-verified
(see [`node/pkg/cantonclient/README.md`](../node/pkg/cantonclient/README.md)).
The client exposes a small interface (mirroring `suiclient.SuiClient`):

```go
type CantonClient interface {
    // GetLedgerEnd returns the current participant offset (the "height").
    GetLedgerEnd(ctx context.Context) (int64, error)

    // SubscribeUpdates streams transactions from beginExclusive onward,
    // delivering each Wormhole message-publish event found in them.
    SubscribeUpdates(ctx context.Context, beginExclusive int64, templateID TemplateID, choiceName string, out chan<- CantonMessageEvent) (Subscription, error)

    // GetUpdateByOffset fetches a single transaction by offset (reobservation).
    GetUpdateByOffset(ctx context.Context, offset int64) (CantonTransaction, error)

    Close() error
}
```

Mapping to Ledger API v2 RPCs:

| Need                | Ledger API v2                                                                                                                       |
| ------------------- | ----------------------------------------------------------------------------------------------------------------------------------- |
| height / readiness  | `StateService.GetLedgerEnd` → `offset (int64)`                                                                                      |
| live message stream | `UpdateService.GetUpdates` (server stream); filter `Transaction.events[].exercised` by `template_id` + `choice == "PublishMessage"` |
| reobservation       | `UpdateService.GetUpdateByOffset` (or `GetTransactionByOffset`) for the offset encoded in the reobservation request                 |

The Wormhole message comes from the **`ExercisedEvent`** of the `PublishMessage`
choice: `exercise_result` is the `WormholeMessage` record (§4.2), decoded from
the Ledger-API `Value`/`Record` representation. The watcher derives the 32-byte
emitter address from the record's `registrar`, `owner`, and `emitterId` fields
using the canonical encoding in §4.2 (see `deriveEmitterAddress` in
[`watcher.go`](../node/pkg/watchers/canton/watcher.go)); the encoding is pinned
against the Daml side by a shared cross-language test vector.

**Party filter.** The `GetUpdates` request uses an `UpdateFormat` with a wildcard
template filter under **`filters_for_any_party`** by default — the watcher
observes `PublishMessage` from _every_ emitter on the participant and needs no
operator party id. (An optional `--cantonReadAsParty` narrows the stream to a
single party via `filters_by_party` instead.) `TRANSACTION_SHAPE_LEDGER_EFFECTS`
is required so `ExercisedEvent`s — which carry the choice result — are included.

### 7.2 `TxID` and offsets

The Ledger API has no 32-byte transaction hash. We use the **participant offset**
(`int64`) as the canonical transaction identifier:

- `MessagePublication.TxID` = the transaction's offset, big-endian, left-padded
  to 32 bytes (so it round-trips through the 32-byte `tx_hash` reobservation
  field).
- Reobservation decodes those 32 bytes back to an `int64` offset and calls
  `GetUpdateByOffset`.

The Daml `update_id` (a string) is carried in logs for human correlation but is
not the `TxID`, because reobservation plumbing (`gossipv1.ObservationRequest`)
uses raw bytes and offsets are the stable, range-queryable key.

### 7.3 `Run` loop (three goroutines, mirrors Sui)

```mermaid
flowchart TD
    run(["Run(ctx)"]) --> init["client = cantonclient.New(rpc)<br/>end = client.GetLedgerEnd(ctx)<br/>(connectivity check; set height; readiness)"]
    init --> ready["supervisor.Signal(healthy); readiness.SetReady"]
    ready --> pump["RunWithScissors: canton_data_pump<br/>SubscribeUpdates(from end) → decode → msgC"]
    ready --> height["RunWithScissors: canton_block_height<br/>every 5s: GetLedgerEnd → SetNetworkStats + readiness"]
    ready --> reobs["RunWithScissors: canton_fetch_obvs_req<br/>obsvReqC → GetUpdateByOffset → re-emit (IsReobservation=true)"]
    pump --> sel{"select: ctx.Done / errC"}
    height --> sel
    reobs --> sel
```

Each observed event becomes a `common.MessagePublication`
([`chainlock.go`](../node/pkg/common/chainlock.go)) with
`EmitterChain = vaa.ChainIDCanton`, `EmitterAddress = deriveEmitterAddress(registrar, owner, emitterId)`,
`Unreliable = false`, then is sent on `msgC`.

### 7.4 Configuration

`WatcherConfig` (implements `watchers.WatcherConfig`):

```go
type WatcherConfig struct {
    NetworkID  watchers.NetworkID
    ChainID    vaa.ChainID        // ChainIDCanton
    Rpc        string             // host:port of the Ledger API
    PackageID  string             // package-id of wormhole-core (template filter)
}
```

Wired into the guardian via
[`node/pkg/node/options.go`](../node/pkg/node/options.go) and a `--cantonRPC` /
`--cantonContract` flag pair in `node/cmd/guardiand`, following the Sui pattern.

---

## 8. File map

```
canton/
  README.md                      ← this document
  multi-package.yaml             ← multi-package project (core + test)
  Dockerfile                     ← devnet image (build DARs + run sandbox)
  core/                          ← `wormhole-core` package (templates; NO daml-script)
    daml.yaml
    daml/Wormhole/Core/Bytes.daml
    daml/Wormhole/Core/VAA.daml
    daml/Wormhole/Core/GuardianSet.daml
    daml/Wormhole/Core/State.daml
    daml/Wormhole/Core/Governance.daml
    daml/Wormhole/Core/Setup.daml
  test/                          ← `wormhole-core-test` package (Daml Scripts)
    daml.yaml                    ← data-dependency on core's DAR
    daml/Test/TestCore.daml      ← unit tests + devnet `setup` / `integrationPublish`
  devnet/
    start_sandbox.sh             ← starts the Ledger API v2 sandbox
    bootstrap.sh                 ← allocates Operator + creates CoreState
    wait_for_ledger.sh

node/pkg/cantonclient/           ← Ledger API v2 gRPC client
  proto/com/daml/ledger/api/v2/  ← real vendored protos (Canton 3.5.5)
  proto/gen/                     ← generated Go stubs (buf generate)
  vectorgen_test.go              ← regenerates the signed-VAA test vector (GEN_CANTON_VECTORS=1)
node/pkg/watchers/canton/        ← the watcher (config.go, watcher.go)
  watcher_integration_test.go    ← dpm-driven end-to-end test (integration tag)
sdk/vaa/structs.go               ← ChainIDCanton = 72
devnet/canton-devnet.yaml        ← Tilt k8s manifest (sandbox + bootstrap)
Tiltfile                         ← `canton` component (opt-in)
```

## 9. Local development

This project uses the Canton Network
[`dpm`](https://docs.canton.network/sdks-tools/cli-tools/dpm) toolchain (not the
legacy `daml` assistant). It is a **multi-package** project
([`multi-package.yaml`](multi-package.yaml)): `core` (the production
`wormhole-core` templates — no `daml-script`) and `test` (`wormhole-core-test`,
the Daml Scripts, with a data-dependency on core's DAR). Splitting keeps
`daml-script` and the test code out of the production DAR.

### Building & testing with `dpm`

```bash
# one-time: install the dpm CLI
curl https://get.digitalasset.com/install/install.sh | sh

cd canton
dpm install 3.5.1                # fetch the pinned SDK (Daml-LF 2.3)
dpm build --all                  # builds core, then test → *.daml/dist/*.dar
cd test && dpm test --all --show-coverage   # run the Scripts + coverage
```

`dpm test` runs every `Script` in
[`TestCore.daml`](test/daml/Test/TestCore.daml) on an in-memory ledger — no
running sandbox required. The suite passes, including governance and VAA
verification with a **real guardian-signed VAA** test vector (§5) and the
owner-bound emitter address derivation pinned against the shared cross-language
vector (`testAddressVector`). Note Daml coverage is **template/choice coverage
only**; the pure parser / DER / governance
helpers and the address encoding are additionally validated by Script
_assertions_ and the signed-VAA / address vectors, not by the coverage metric.

### Watcher integration test (dpm + Go)

The watcher's real transport is covered end-to-end by a Go integration test,
[`node/pkg/watchers/canton/watcher_integration_test.go`](../node/pkg/watchers/canton/watcher_integration_test.go)
(`//go:build integration`):

```
go test -tags integration -run TestCantonWatcherIntegration ./pkg/watchers/canton -v
```

It builds the DAR, boots a `dpm sandbox`, publishes a message via
`dpm script --upload-dar`, then connects the **real** `cantonclient` gRPC client
(real Ledger API v2 protos) and asserts the published `WormholeMessage` is
observed and mapped to a `common.MessagePublication`. It is skipped unless `dpm`
(+ a JDK) is present and is excluded from the default build. Notes learned the
hard way and baked into the test/devnet scripts: Canton's `sandbox` binds the
Ledger API on **6865** and does not take `--dar`/`--port`; and `dpm script` needs
**`--upload-dar`** so package vetting is synchronous (a bare upload races vetting
→ `PACKAGE_SELECTION_FAILED`).

### End-to-end with Tilt

Canton is wired into [`Tiltfile`](../Tiltfile) as an **opt-in** component
(`--canton`), mirroring the Sui setup:

```
tilt up -- --canton
```

This brings up:

- a **`canton`** pod running a Canton **sandbox** (Ledger API v2 on `:6865`)
  ([`devnet/start_sandbox.sh`](devnet/start_sandbox.sh));
- a **`canton-contracts`** bootstrap step that uploads+vets the `wormhole-core`
  DAR (`dpm script --upload-dar`), allocates the **Operator** party, and creates
  the `CoreState` with the standard devnet guardian via `Test.TestCore:setup`
  ([`devnet/bootstrap.sh`](devnet/bootstrap.sh)); and
- the **guardian** configured with `--cantonRPC canton:6865` (no
  `--cantonReadAsParty`: the watcher uses a wildcard "any party" filter, §7.1).

### Status: opt-in while the k8s devnet path is validated

Unlike Sui, the `canton` component is **not** enabled by `--ci`. The guardian
builds the real client by default (no build tag) and the watcher needs no
operator party id (wildcard filter), so there are no functional gates left — the
main item to finish is:

1. **Full k8s path validation.** Validated locally: the Go integration test (§9)
   covers the watcher↔ledger path; the sandbox `0.0.0.0` bind
   (`-C canton.participants.sandbox.ledger-api.address=0.0.0.0` → binds `*:6865`),
   the bootstrap `dpm script --upload-dar` flow, and the same-pod localhost wiring
   are all confirmed against a live `dpm sandbox`. What still needs a real cluster
   run: the `canton-node` **image build** (`canton/Dockerfile`, which fetches dpm
   and builds the DARs) and the **guardian connecting** to `canton:6865` across the
   k8s Service.

Today `--canton` stands up the **chain + contracts** for Daml-side development
(iterating on the templates, `dpm test`); the watcher↔ledger path itself is
covered by the integration test in §9.

---

## 10. Trust model & deployment topology

How does a set of `n` guardians (`k`-of-`n` threshold) run Wormhole on Canton
faithfully — see and attest only what actually happened under rules they accept —
given Canton has no global VM that every node re-executes? Two requirements, met
by two mechanisms:

- **"The code is what we published."** Daml code identity is content-addressed: a
  DAR's **package-id is the hash of the compiled package**, and every Ledger-API
  event carries it (the watcher already filters on `WatcherConfig.PackageID`).
  Publish source + a reproducible build, and each guardian's participant **vets**
  that exact package-id before its parties transact under it. A lookalike package
  simply does not match the filter.
- **"The events are real."** Canton's synchronizer (sequencer + mediator)
  **orders and tallies but never runs Daml**. Execution integrity comes from the
  **participant nodes hosting a transaction's stakeholders re-executing** the
  choice from the vetted DAR and confirming the result; a transaction commits only
  if the confirmation policy over those participants is met.

**Topology.** Each guardian runs a **participant node**, and the `operator` party
is **multi-hosted on the guardian participants with observation-only permission**.
That is sufficient and deliberately minimal: the operator is a signatory on every
core contract, so its projection is the full app trace; each guardian's
participant independently receives and validates every such transaction, and an
invalid one never appears on that participant's Ledger API — so a watcher cannot
sign it. Guardians are **not** given confirmation rights: the VAA quorum is
already the attestation layer, and Wormhole's job is to *refuse to attest*
forgeries, not to halt Canton (giving guardians confirmation duty would duplicate
the quorum and turn guardian downtime into a chain-halt).

**What a compromised operator can and cannot do.** It can degrade **liveness**
(stop approving registrations, stop relaying, evict observers — all fail-safe:
guardians then simply stop seeing and stop signing). It **cannot** produce
anything guardians would wrongly attest: emitting under an existing address
requires that owner's signature (§4.2), so an outbound forge is either an invalid
transaction (rejected in replay, never observed) or a valid contract with a
different, untrusted address.

Crucially, it also **cannot forge the guardian set** used for inbound
verification: `CoreState` is co-signed by the `guardianGovernance` threshold party
and keyed by it (§4.1), so the operator can neither mutate the real `CoreState`
nor fabricate one that a consumer would find by the governance anchor. This closes
the one gap that observation-only hosting does *not* cover — inbound state
integrity — by moving it from "trust the single operator" to "trust the k-of-n
governance party." Compromising *that* is a threshold compromise, i.e. genuinely
takeover-class, rather than a single-key bar. Residual inbound corruption (bugs in
a verification path, not forged state) can still only damage Canton-local state —
the same position Wormhole holds on every chain, where a compromised chain wrecks
its own state but cannot forge messages other chains accept. The guardians'
faithfulness invariant thus reduces to **"the transaction is Daml-valid under the
vetted package."**

The exact observer-party mechanics — a dedicated guardian-observer party named as
a template `observer` vs. replica-hosting the `operator`; external-party namespace
key custody controlling the hosting topology — are the subject of the
`feat/canton-public-observer` work and are tracked as an Open Question, not
implemented here.

---

## 11. Open Questions & Validation Items

1. **Ledger API v2 RPC surface.** The exact message/field names of
   `UpdateService.GetUpdates` / `GetUpdateByOffset` / `StateService.GetLedgerEnd`
   shift slightly across Canton releases (e.g. `begin_exclusive`/`end_inclusive`,
   `Update` one-of members). The vendored proto subset targets a specific release
   — confirm and pin against Daml SDK 3.5.1 / Canton 3.5.x.
2. **`guardianGovernance` threshold party.** `CoreState` is co-signed by and keyed
   on a `guardianGovernance` party (§4.1), so guardian-set integrity is anchored to
   it rather than the operator. Stand it up as a decentralized-namespace external
   party governed by a k-of-n threshold of guardian keys (DSO-style) and confirm:
   the genesis co-sign ceremony, the topology-level rotation flow (add/remove
   guardian keys without touching `CoreState`), and that consumers are configured
   with the correct anchor party id. Note the `CoreState` singleton count ("exactly
   one") is still behavioral — confirm the genesis automation creates exactly one.
   (Emitter/manager **address** integrity is separately owner-bound and
   owner-co-signed, §4.2.)
3. **Guardian observer topology.** Confirm the observer-party mechanics from
   §10 (dedicated observer party as template `observer` vs. replica-hosted
   `operator`; external-party namespace-key custody; how `--cantonReadAsParty`
   maps to the chosen party). Reconcile with `feat/canton-public-observer`. Note
   this observer party is distinct from the `guardianGovernance` signatory party
   (item 2): the observer is read-only, the governance party has signatory power.
4. **Integrator VAA verification.** Implement the hint-free guardian set (persist
   pubkeys, §5) and confirm the guardian-set distribution mechanism for
   integrators (explicit disclosure vs public-observer snapshot).
5. **Native fees.** Whether/when to wire real Canton Coin (Splice `amulet`)
   payments into `PublishMessage`/`TransferFees`, or whether Canton runs fee-less
   initially (`messageFee = 0`).
6. **ContractUpgrade mechanism.** Confirm the operator-driven Daml package
   upgrade flow and how strictly the recorded target package-id should gate it.
   Note contract keys **cannot** be added to an already-deployed template via
   smart-contract upgrade, so this keyed design must land before any mainnet
   deployment of the templates.
7. **Governance emitter.** Uses the standard governance emitter (Solana, chain 1,
   address `0x00..04`). Confirm for the target deployment/network.
