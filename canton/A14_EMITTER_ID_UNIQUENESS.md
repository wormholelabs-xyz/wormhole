# A14-3: Emitter Address Uniqueness

## Status

Option 1 is in the code on the proposal branch `canton/overnight-u2`.
The owner must approve it before a merge (decision U2).
The test `testRedTeamA14OperatorCannotResetEmitterRegistry` passes.
The section [Implementation of option 1](#implementation-of-option-1) gives the change, the version decision, and the migration.

## Problem

Wormhole needs a unique message id on every runtime.
The message id is the tuple (chain, emitter address, sequence).

The Canton emitter address is `keccak256(tag, registrar, owner, emitterId)`.
`emitterAddressFor` computes it (`canton/core/daml/Wormhole/Core/State.daml:374-376`).
The watcher computes the same value (`node/pkg/watchers/canton/watcher.go:62-80`).

The `emitterId` comes from the `nextId` field of `EmitterRegistry`.
Before option 1, the operator was the only signatory of `EmitterRegistry`.
Thus the operator alone could create a new registry with any `nextId`.
`mkInitialEmitterRegistry` starts each registry at 0 (`canton/core/daml/Wormhole/Core/Setup.daml:64-66`).

An honest owner that registers through the new registry gets an id that it already has.
`RegisterEmitter` then creates an `Emitter` with the old address and sequence 0 (`State.daml:442-450`).
The owner co-signs the new `Emitter`, so the watcher accepts both emitters.
Two messages with one message id then reach quorum.
The processor signs each digest independently (`node/pkg/processor/message.go:36`).

## Effect on NTT

NTT mints each transceiver `Emitter` through `RegisterManager`.
The requester is `guardianGovernance` for every deployment (`wl-native-token-transfers/canton/ntt/daml/Wormhole/Ntt/Governance.daml:270-274`).
Thus all NTT transceivers have the same owner.
The caller supplies `emitterRegistryCid` (`Governance.daml:98`).
The only check is on the operator and `guardianGovernance` fields of the new `Emitter` (`Governance.daml:278-279`).
A registry that the operator resets passes this check.

After a reset, a new NTT deployment can get the transceiver address of an existing deployment.
The new deployment then publishes from the same address, starting at sequence 0.
A destination chain that accepts a transceiver peer by address alone accepts these messages.
The NTT manager address in the payload uses the `NttGovernance` counter (`Governance.daml:281`).
That counter has `guardianGovernance` as a signatory (`Governance.daml:49`), so the operator cannot reset it.
Thus the manager peer check on the destination still stops a forged transfer.
The A14-3 risk for NTT is a transceiver-level collision, not a direct mint.

## Options

### Option 1: Registry co-signed by guardianGovernance

Mechanism:

1. Add `guardianGovernance` as a signatory of `EmitterRegistry`.
2. Create the registry in the genesis ceremony, together with `CoreState`.
3. `RegisterEmitter` keeps its body. It inherits both signatures, as `CoreState` choices do (`State.daml:137`).

Protection:

- The operator alone cannot create a registry. A reset needs the guardian threshold.
- The operator and one owner can still repeat the address of that owner (A14-2).

Address formula: no change.

Watcher: no change.
The package-id pin (`node/pkg/watchers/canton/config.go:19-23`) must name the new package.
Without the pin, a registry of the old package stays usable.

NTT: no code change.
`RegisterManager` passes the new registry cid.
The `guardianGovernance` signatory gives the registry the same trust level as `NttGovernance`.

Migration:

1. Read the highest `emitterId` in the `Emitter` contracts that `guardianGovernance` observes (`State.daml:534`).
2. Run a guardian ceremony that creates the new registry with `nextId` equal to that value plus 1.
3. Archive the old registry.
4. Pin the watcher to the new package id.

The signatory change is not compatible with a smart contract upgrade.
Thus the migration needs a new registry contract, not an upgrade of the old one.
The test must change: an operator-only `createCmd` of the registry must fail.

### Option 2: Per-owner counter co-signed by the owner

Mechanism:

1. Add a template `OwnerEmitterCounter` with the field `nextOwnerEmitterId`.
2. Make the operator and the owner its signatories.
3. `RegisterEmitter` consumes the counter of the requester and creates its successor.
4. The address uses `nextOwnerEmitterId` in place of the global `nextId`.

Protection:

- The operator alone cannot create or reset a counter, because the owner signs it.
- The operator and the owner can repeat an address of that owner (A14-2).
- An honest owner that opens two counters by mistake gets duplicate addresses.
  Canton contract keys are not unique, so the ledger cannot stop this.

Address formula: the id becomes per owner. Existing emitters keep their global ids.
The id ranges overlap, so the tag must change to `wormhole:emitter:v2` for new emitters.

Watcher: `deriveEmitterAddress` must select the tag from a new message field.
`decodeWormholeMessage` (`node/pkg/cantonclient/cantongrpc.go:233-290`) must read that field.

NTT: all NTT transceivers share one owner, `guardianGovernance`.
Thus all NTT registrations contend on one counter.
The counter needs the signature of `guardianGovernance`, so a guardian ceremony creates it.

Migration: new template, new tag, a watcher release, and a guardian ceremony for the NTT counter.

### Option 3: Owner-supplied registration salt

Mechanism:

1. Add a field `registrationSalt : Bytes32` to `RegisterEmitter` and to `Emitter`.
2. The owner client generates a random salt for each registration.
3. The address becomes `keccak256(tag_v2, registrar, owner, emitterId, registrationSalt)`.
4. `PublishMessage` puts the salt in `WormholeMessage`.

The ledger cannot hash a contract id. The Daml standard library documents that `contractIdToText` returns `None` in a choice body.
Thus a random salt from the owner replaces a salt from the consumed registry cid.

Protection:

- The operator alone cannot repeat an address, because it does not choose the salt.
- The operator and the owner can repeat an address of that owner (A14-2).

Address formula: a new tag and a fifth input.
`WormholeMessage` changes shape. It is the wire contract with the watcher (`State.daml:75-83`).

Watcher: decode the salt and derive the v2 address.
Keep v1 for emitters without a salt.

NTT: the NTT admin, not `guardianGovernance`, picks the arguments of `RegisterManager`.
An admin that copies the salt of another deployment gets its address after a reset.
Thus NTT must derive the salt from `nttManagerAddressFor operator admin nextId` (`Governance.daml:281`).
That value cannot repeat, because `guardianGovernance` signs the `NttGovernance` counter.

Migration: new `Emitter` field, new `WormholeMessage` field, a watcher release, and an NTT release.

### Option 4: Watcher refuses a second emitter lineage

Mechanism:

1. The watcher reads the `Emitter` create events that `guardianGovernance` observes.
2. For each identity (registrar, owner, emitterId), it records the contract id of the live `Emitter`.
3. It accepts a `PublishMessage` only on that live contract.
4. Each accepted `PublishMessage` moves the record to the successor contract id.
5. It drops a message from a second `Emitter` with the same identity.

Protection:

- The operator alone: the second `Emitter` of the honest owner does not publish.
- The operator and the owner: the rewound `Emitter` of A14-2 does not publish either.

Address formula: no change.

Watcher: a large change.
The watcher must keep a persistent map from identity to contract id.
The map must have a capacity limit, and it grows with each emitter.
A restart must rebuild the map from the active contract set or from a snapshot.
All guardians must see the same first `Emitter`.
One synchronizer gives one order. More than one synchronizer gives no such guarantee.

NTT: no code change. A second NTT transceiver with a repeated address cannot publish.
That deployment stops, but no duplicate message id occurs.

Migration: a watcher release and a persistent store. No Daml change.

## Comparison

| Option | Operator alone | Operator and owner | Address formula | Watcher | NTT | Cost |
| - | - | - | - | - | - | - |
| 1. Co-signed registry | Stops | Does not stop | Same | Pin only | None | Low |
| 2. Per-owner counter | Stops | Does not stop | v2 tag | Change | One shared counter | Medium |
| 3. Owner salt | Stops | Does not stop | v2 tag and salt | Change | Salt from manager address | Medium |
| 4. Watcher lineage | Stops | Stops | Same | Large change | None | High |

## Recommendation

Use option 1 now.
It closes A14-3 with one signatory change and a guardian ceremony.
It keeps the address formula, the watcher, and NTT the same.

Add option 4 later as defense in depth.
It is the only option that also closes A14-2.
Do it when the watcher has a persistent store.

Do not use option 2. A shared NTT counter and duplicate counters of one owner are new failure modes.
Use option 3 only if a guardian ceremony for the registry is not possible.

## Implementation of option 1

### Change

- `EmitterRegistry` signatories are `operator, guardianGovernance` (`State.daml:398`).
- `RegisterEmitter` keeps the controllers `requester, payer`.
  The choice body inherits the operator and `guardianGovernance` signatures from the registry.
  The body and the address formula do not change.
- `Test.TestCore:setup` creates `CoreState` and `EmitterRegistry` in one transaction.
  The operator and `guardianGovernance` sign that transaction.
- `canton/devnet/genesis_guardian_governance.sh` puts both create commands in one prepared transaction.
  The guardian owners sign it with the threshold.
- The Go watcher, the NTT code, and the integration test helpers do not change.

### Tests

- `testRedTeamA14OperatorCannotResetEmitterRegistry` (`Test/TestRedTeamCore.daml`).
  The operator alone tries to create a registry that names the real `guardianGovernance`.
  The ledger rejects it with an authorization failure.
  A second registration of the same owner gets emitter id 1 and a different address.
- `testRogueGovernanceEmitterCannotPublishOnRealCoreState` (`Test/TestGuardianVisibility.daml`).
  The operator and a rogue `guardianGovernance` can create a registry together.
  An emitter from that registry cannot publish against the real `CoreState`.
- `testGovernanceSeesRegistry` (`Test/TestGuardianVisibility.daml`).
  `guardianGovernance` is a stakeholder of the registry. An outsider is not.
- `testArchives` (`Test/TestCore.daml`). The operator alone cannot archive the registry.

### Contention and liveness

The registry stays one contract. Each `RegisterEmitter` consumes it.
Thus the contention does not change.

`guardianGovernance` is now a signatory of the consumed registry and of its successor.
Thus each participant that hosts `guardianGovernance` is an informee of each `RegisterEmitter`.
Each host with the confirmation permission is also a confirmer.
A registration completes only when the confirmation threshold of `guardianGovernance` approves it.

Effect on liveness:

- If fewer guardian participants than the threshold are available, registrations stop.
- Publishes already depend on the same threshold, because `PublishMessage` exercises choices on `CoreState`.
- Before option 1, a registration with a zero fee did not need a guardian participant.
- A change of `registrationFee` now needs a guardian ceremony. The ceremony archives the registry and creates a new one.

### Version decision

A change to the signatories of a template is not a smart contract upgrade (SCU).
Canton recomputes the signatories when a choice runs on a contract under a newer package.
For a 0.7.0 registry, the recomputed set includes `guardianGovernance`, but the stored set does not.
Thus the choice fails. The old registry cannot continue under 0.8.0.

`damlc` reports the change only as the warning `upgraded-template-expression-changed`.
Thus the compile-time SCU check cannot show that 0.8.0 is compatible.

Decisions:

- `wormhole-core` is now version 0.8.0.
- `core/daml.yaml` has no `upgrades:` field.
  The 0.8.0 package starts a new deployment line with a registry migration.
- When 0.8.0 ships, vendor its DAR in `canton/dars/` and set `upgrades:` to it for 0.9.0.
- `Emitter`, `CoreState`, and the replay templates do not change.
  Their 0.7.0 contracts continue under 0.8.0.

### Migration from 0.7.0

1. Upload the 0.8.0 DAR to all guardian participants and to the operator participant.
   Vet it on each of these participants.
2. Tell integrators the time when registrations stop.
3. Record `nextId` of the 0.7.0 registry.
   Archive the 0.7.0 registry with the operator signature. Registrations stop.
4. Read the highest `emitterId` in the `Emitter` contracts that `guardianGovernance` observes (`State.daml:543`).
   Set `firstId` to the larger value of (highest `emitterId` + 1) and the recorded `nextId`.
5. Run a guardian ceremony. It creates a 0.8.0 `EmitterRegistry` with `nextId = firstId`.
   The ceremony uses the same `registrationFee` as the 0.7.0 registry.
6. Update the watcher pin: set `--cantonPackageID` to the 0.8.0 package id (README §7.5).
7. Unvet 0.7.0 on all guardian participants.

> **WARNING:** Do step 7. The watcher pin alone does not close A14-3.
> `Emitter` does not change, so an `Emitter` from a 0.7.0 registry can publish through 0.8.0.
> `guardianGovernance` observes each `Emitter`, so its participants are informees of the create.
> When no guardian participant vets 0.7.0, a 0.7.0 `RegisterEmitter` fails.
> This step has no test on more than one participant.

> **CAUTION:** Step 4 must read the guardian view of all emitters.
> A registry that starts at a used id repeats the A14-3 collision.

### NTT

NTT must be rebuilt on `wormhole-core` 0.8.0 before the core migration (NTT-first rule).
The NTT DARs refer to the `EmitterRegistry` type of 0.7.0 (`wl-native-token-transfers/canton/ntt/daml.yaml:28`).
`RegisterManager` and `RegisterManagerByVaa` need no code change.
Their callers pass the cid of the 0.8.0 registry.

## Related operator-only contracts

| Contract | Signatories | Same class as A14-3 | Severity | Action |
| - | - | - | - | - |
| `CoreState` | operator, `guardianGovernance` | No. | None. | None. |
| `ReplayRootRegistry` | operator | No. It has no counter. Each claim needs the consumer signature. A rogue registry at fee 0 gives the same result as the real one. | None. The operator alone can archive it and stop new claims (liveness). | None. |
| `ReplayNode` | consumer, operator | No. The operator alone cannot create a root for a consumer. | None. | None. |
| `Emitter` | operator, owner | No. The operator and the owner can rewind (A14-2). | Low. | Recorded as A14-2. |
| NTT `NttGovernance` | `guardianGovernance`, operator | No. The `nextId` counter needs the `guardianGovernance` signature. | None. | None. |
| NTT `NttManager`, `LockedLedger` | include `guardianGovernance` | No. Only `RegisterManager` creates them. | None. | None. |

No other core contract has an operator-only counter.
Thus this branch changes only `EmitterRegistry`.
