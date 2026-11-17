# A14-3: Emitter Address Uniqueness

## Status

Option 1 is in the code on the proposal branch `canton/overnight-u2`.
The owner must approve it before a merge (decision U2).
The test `testRedTeamA14OperatorCannotResetEmitterRegistry` passes.
The section [Implementation of option 1](#implementation-of-option-1) gives the change, the version decision, the migration, and the LocalNet rehearsal.

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

Migration: a new registry contract, created in a guardian ceremony after the unvet of 0.7.0.
The section [Migration from 0.7.0](#migration-from-070) gives the procedure.

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
- `SetRegistrationFee` is a new consuming choice on `EmitterRegistry`.
  Its controllers are the operator and `guardianGovernance`.
  It creates the successor with the new fee and the same `nextId`.
  It rejects a negative fee.
- `Test.TestCore:setup` creates `CoreState` and `EmitterRegistry` in one transaction.
  The operator and `guardianGovernance` sign that transaction.
- `canton/devnet/genesis_guardian_governance.sh` puts both create commands in one prepared transaction.
  The guardian owners sign it with the threshold.
- The Go watcher and the NTT package code do not change.
  The NTT tests change (refer to [NTT](#ntt)).

`SetRegistrationFee` uses the signatories as controllers, not a governance VAA.
The core governance actions in `sdk/vaa/payloads.go` have no registration fee action.
`guardianGovernance` is the guardian threshold party, so a fee change needs the guardian threshold.

### Tests

- `testRedTeamA14OperatorCannotResetEmitterRegistry` (`Test/TestRedTeamCore.daml`).
  The operator alone tries to create a registry that names the real `guardianGovernance`.
  The ledger rejects it with an authorization failure.
  The operator alone also cannot archive the real registry or run `SetRegistrationFee` on it.
  After these attempts, the real registry is the same contract with the same `nextId`.
  A second registration of the same owner gets emitter id 1 and a different address.
- `testSetRegistrationFee` (`Test/TestFees.daml`).
  The operator alone, `guardianGovernance` alone, and an outsider cannot change the fee.
  A negative fee fails.
  The operator and `guardianGovernance` together change the fee.
  `nextId` stays 2.
  The next registration pays the new fee and gets emitter id 2.
- `testRogueGovernanceEmitterCannotPublishOnRealCoreState` (`Test/TestGuardianVisibility.daml`).
  The operator and a rogue `guardianGovernance` can create a registry together.
  An emitter from that registry cannot publish against the real `CoreState`.
- `testGovernanceSeesRegistry` (`Test/TestGuardianVisibility.daml`).
  `guardianGovernance` is a stakeholder of the registry. An outsider is not.
- `testArchives` (`Test/TestCore.daml`). The operator alone cannot archive the registry.
- `canton/tools/emitter_id_scan_test.sh`. A table-driven test of the scan against a JSON Ledger API stub.
  A duplicate active `Emitter` fails the scan.
  A user with a submit right fails the scan.
  An archived `Emitter` with a higher id sets `firstId`.

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
- A change of `registrationFee` needs a guardian ceremony that runs `SetRegistrationFee`.
  The choice keeps `nextId`, so the ceremony does not enter `nextId` by hand.

### Version decision

A change to the signatories of a template is not a valid smart contract upgrade (SCU) of a contract.
Canton recomputes the signatories when a choice runs on a contract under a newer package.
For a 0.7.0 registry, the recomputed set includes `guardianGovernance`, but the stored set does not.
Thus the choice fails with `INTERPRETATION_UPGRADE_ERROR_VALIDATION_FAILED`. The LocalNet rehearsal shows this error.
The old registry cannot continue under 0.8.0.

`damlc` reports the change only as the warning `upgraded-template-expression-changed`.
The participants also accept the 0.8.0 upload as an upgrade of 0.7.0.

Decisions:

- `wormhole-core` is now version 0.8.0. The package name does not change.
  Thus 0.8.0 stays on the `wormhole-core` upgrade line, and package-name selection prefers it.
- `core/daml.yaml` keeps the SCU check: `upgrades:` names the vendored 0.7.0 DAR (`dars/wormhole-core-0.7.0.dar`, `7a22a1c4`).
  The check covers all other templates and choices.
- `Emitter`, `CoreState`, and the replay templates do not change.
  Their 0.7.0 contracts continue under 0.8.0.

### Migration from 0.7.0

The order prevents a race between the scan and the last 0.7.0 registration.
While any guardian participant vets 0.7.0, the operator can plant a 0.7.0 registry with any `nextId`.
`guardianGovernance` does not see that registry.
A registration through it before the scan is an `Emitter` that `guardianGovernance` observes, so the scan counts it.
After the unvet, no 0.7.0 registration can occur.
Thus the scan must start after the unvet.

1. Build the dependents on 0.8.0 (NTT-first rule).
   An integrator DAR that depends on 0.7.0 stops at step 7.
2. Upload the 0.8.0 DAR and the rebuilt dependent DARs to each guardian participant.
   Upload the same DARs to the operator participant.
   Vet them on each of these participants.
3. Announce the cutover to integrators (refer to [Integrator cutover](#integrator-cutover)).
4. Monitor `wormhole_canton_unpinned_publish_messages_total` on a canary watcher pinned to 0.8.0 (README §7.5).
5. Archive the 0.7.0 registry with the operator signature. Registrations stop.
6. Set `--cantonPackageID` to the 0.8.0 package id on each guardian, in one coordinated restart (README §7.5).
   Send reobservation requests for the 0.8.0 messages that the 0.7.0 watchers dropped.
7. Unvet 0.7.0 and the dependents that use 0.7.0 on all guardian participants.
   Use one topology transaction on each participant.
8. Run the scan (refer to [Scan](#scan)) on each guardian participant.
   All participants must give the same `firstId`. Stop if a scan fails.
9. Run a guardian ceremony. It creates a 0.8.0 `EmitterRegistry` with `nextId = firstId`.
   The ceremony uses the same `registrationFee` as the 0.7.0 registry.
10. Run the scan again with `EXPECT_ACTIVE_REGISTRIES=1`. Registrations start again.

> **WARNING:** Do not run the scan before step 7 is complete on all guardian participants.
> A 0.7.0 registration after the scan can repeat an id at or above `firstId`.

> **WARNING:** Do step 7. The watcher pin alone does not close A14-3.
> `Emitter` does not change, so an `Emitter` from a 0.7.0 registry can publish through 0.8.0.
> When no guardian participant vets 0.7.0, a 0.7.0 `RegisterEmitter` fails.
> The LocalNet rehearsal shows the failure (`INVALID_PRESCRIBED_SYNCHRONIZER_ID`).

> **CAUTION:** The scan must read the update history of `guardianGovernance`, not only the active contracts.
> An archived `Emitter` can have the highest id.
> The `nextId` of the operator is not an input. A compromised operator controls it.

### Integrator cutover

1. Announce the time of step 7 and the 0.8.0 package id.
   Registrations stop at step 2: a submission by package name selects 0.8.0, and 0.8.0 rejects the 0.7.0 registry.
2. Integrators that depend on `wormhole-core` rebuild on 0.8.0 before step 2.
   NTT is the first dependent (NTT-first rule).
   An integrator DAR that depends on 0.7.0 cannot run after step 7. Its submissions fail at prepare.
3. Integrators submit with the 0.8.0 package id, or by package name, from step 2.
   A 0.8.0 `PublishMessage` on a 0.7.0 `Emitter` succeeds.
4. Between step 2 and step 6, the 0.7.0 watchers drop each 0.8.0 message.
   Each drop has a WARN log and a counter increment. Reobservation after step 6 recovers each one.
5. Between step 6 and step 7, the 0.8.0 watchers drop each 0.7.0 message.
   These messages are not recoverable. Step 4 makes sure that integrators stopped the use of 0.7.0.
6. After step 10, integrators get the disclosure of the new registry from the disclosure service (README §4.7).

### Scan

`canton/tools/emitter_id_scan.sh` reads the ledger as `guardianGovernance`.
The token must belong to a user with exactly one right: `CanReadAs(guardianGovernance)`.
The script stops if the user has a different set of rights.

The script reads each update from a begin offset to the ledger end, with the ledger effects shape.
It records each `Emitter` and `EmitterRegistry` create of the (operator, `guardianGovernance`) pair, archived or active.
It also reads the active contracts at the same ledger end.

Use begin offset 0 to read from genesis.
A pruned participant needs a begin offset and the `historyMaxEmitterId` of an earlier scan that ended at that offset.

The script fails if one of these conditions occurs:

- Two active `Emitter` contracts have the same (operator, owner, emitterId).
- An active `Emitter` is not in the update history from genesis.
- An active `Emitter` has an id at or above `firstId`.
- The number of active registries is not `EXPECT_ACTIVE_REGISTRIES` (default 0).

`firstId` is the larger value of (highest history `emitterId` + 1) and the highest history registry `nextId`.

### NTT

NTT must be rebuilt on `wormhole-core` 0.8.0 before the core migration (NTT-first rule).
The NTT DARs refer to the `EmitterRegistry` type of 0.7.0 (`wl-native-token-transfers/canton/ntt/daml.yaml:28`).
`RegisterManager` and `RegisterManagerByVaa` need no code change.
Their callers pass the cid of the 0.8.0 registry.

A build of NTT on 0.8.0 (`ntt` 0.4.0, `59093de8`, not committed) compiles with no source change.
The NTT test package needs changes.
Its setup creates the registry with the operator alone, and that create now fails.
With the setup co-signed, 174 tests pass and 3 tests fail.
The 3 tests create or archive a registry with the operator alone: `testArchives`, `testRegistrationRejectsForeignRegistries`, and `testRedTeamQuorumCustodyRegistryCannotMintBridgedInstrument`.
These failures are the intended effect of A14-3. Update the tests to expect them.

### LocalNet rehearsal

`canton/localnet/registry_migration_rehearsal.sh` rehearses the migration on LocalNet with three guardian participants.
It uses the LocalNet scripts of the integration branch through `LOCALNET_SCRIPTS_DIR`.
The evidence logs are `$LOCALNET_STATE_DIR/evidence/registry_migration_rehearsal-*.log`.

Results of the run on 2026-11-17:

- Each participant accepted the 0.8.0 upload as an upgrade of 0.7.0.
- A RegisterEmitter by package name on the 0.7.0 registry failed with `INTERPRETATION_UPGRADE_ERROR_VALIDATION_FAILED`.
- The operator alone planted a 0.7.0 registry with `nextId` 36.
- The 0.7.0 watchers dropped the 0.8.0 publish with a WARN log. The 0.8.0 watchers reobserved it.
- After the unvet, a 0.7.0 `PublishMessage` and a 0.7.0 `RegisterEmitter` on the planted registry failed with `INVALID_PRESCRIBED_SYNCHRONIZER_ID`.
  Each of the three participants "has not vetted" 0.7.0.
- A 0.8.0 `RegisterEmitter` on the planted registry failed with `INTERPRETATION_UPGRADE_ERROR_VALIDATION_FAILED`.
- The scan on each participant gave `firstId` 36 from 187 `Emitter` creates.
- The operator alone could not create the 0.8.0 registry (`DAML_AUTHORIZATION_ERROR`).
  The operator and one owner key could not create it (`FAILED_TO_EXECUTE_TRANSACTION`).
  The operator and two of three owner keys created it with `nextId` 36.
- A new party registered emitter id 36 and published sequence 0.
  The three 0.8.0 watchers observed the message with the address of the `Emitter`.
- A duplicate `Emitter` from the operator and the owner (A14-2) failed the scan. After its archive, the scan passed.

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
