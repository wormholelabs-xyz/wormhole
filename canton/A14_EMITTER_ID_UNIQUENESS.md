# A14-3: Emitter Address Uniqueness

## Status

Option 1 merged in #75 (`wormhole-core` 0.8.0).
Option 5 is on the branch `canton/overnight-u2-fixes`, also in 0.8.0.
The section [Implementation of options 1 and 5](#implementation-of-options-1-and-5) gives the change, the version decision, the deployment, and the scan.

## Problem

Wormhole needs a unique message id on every runtime.
The message id is the tuple (chain, emitter address, sequence).

The Canton emitter address is `keccak256(tag, registrar, owner, emitterId)`.
`emitterAddressFor` computes it (`canton/core/daml/Wormhole/Core/State.daml:388-390`).
The watcher computes the same value (`node/pkg/watchers/canton/watcher.go:68-84`).

The `emitterId` comes from the `nextId` field of `EmitterRegistry`.
Before option 1, the operator was the only signatory of `EmitterRegistry`.
Thus the operator alone could create a new registry with any `nextId`.
`mkInitialEmitterRegistry` starts each registry at 0 (`canton/core/daml/Wormhole/Core/Setup.daml:64-66`).

An honest owner that registers through the new registry gets an id that it already has.
`RegisterEmitter` then creates an `Emitter` with the old address and sequence 0 (`State.daml:478-484`).
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

Deployment: a registry created in the genesis ceremony.
The section [Deployment of 0.8.0](#deployment-of-080) gives the procedure.

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
`decodeWormholeMessage` (`node/pkg/cantonclient/cantongrpc.go:270-330`) must read that field.

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
`WormholeMessage` changes shape. It is the wire contract with the watcher (`State.daml:75`).

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

### Option 5: Emitter co-signed by guardianGovernance

Mechanism:

1. Add `guardianGovernance` as a signatory of `Emitter`.
2. `RegisterEmitter` and `PublishMessage` keep their bodies. They inherit the three signatures.
3. The watcher drops a `PublishMessage` event when its read-as party is not a stakeholder of the `Emitter`.

Protection:

- The operator and the owner cannot create a second `Emitter` of an issued identity (A14-2).
  The create needs the guardian threshold.
- With the owner equal to the operator, the operator alone cannot do it either.

Address formula: no change.

Watcher: a stakeholder check in `messagesFromTx` (refer to [Change](#change)).

NTT: no code change. The requester is already `guardianGovernance`.

Liveness: no new cost. `PublishMessage` already needs the confirmation threshold of `guardianGovernance` through `CoreState`.

Migration: a 0.7.0 `Emitter` cannot run under 0.8.0. Thus 0.8.0 starts from a fresh genesis.

## Comparison

| Option | Operator alone | Operator and owner | Address formula | Watcher | NTT | Cost |
| - | - | - | - | - | - | - |
| 1. Co-signed registry | Stops | Does not stop | Same | Pin only | None | Low |
| 2. Per-owner counter | Stops | Does not stop | v2 tag | Change | One shared counter | Medium |
| 3. Owner salt | Stops | Does not stop | v2 tag and salt | Change | Salt from manager address | Medium |
| 4. Watcher lineage | Stops | Stops | Same | Large change | None | High |
| 5. Co-signed `Emitter` | Stops | Stops | Same | Stakeholder check | None | Low |

## Recommendation

Use options 1 and 5.
Option 1 closes A14-3. Option 5 closes A14-2.
Both keep the address formula and NTT the same.
Option 4 is then not necessary.

Do not use option 2. A shared NTT counter and duplicate counters of one owner are new failure modes.
Use option 3 only if a guardian ceremony for the registry is not possible.

## Implementation of options 1 and 5

### Change

- `EmitterRegistry` signatories are `operator, guardianGovernance` (`State.daml:407`).
- `Emitter` signatories are `operator, owner, guardianGovernance` (`State.daml:579`).
  `guardianGovernance` is no longer an observer, because it is a signatory.
- `RegisterEmitter` keeps the controllers `requester, payer`.
  The choice body inherits the operator and `guardianGovernance` signatures from the registry.
  The address formula does not change.
- `RegisterEmitter` takes `expectedGuardianGovernance : Optional Party` as its last argument.
  `Some gg` makes the registration fail with "registry guardianGovernance mismatch" unless the registry names `gg`.
  The check runs before the fee charge. Clients and NTT `RegisterManager` pass `Some`.
- `SetRegistrationFee` is a consuming choice on `EmitterRegistry`.
  Its controllers are the operator and `guardianGovernance`.
  It creates the successor with the new fee and the same `nextId`.
  It rejects a negative fee and a fee above `maxRegistrationFee`.
- `ensure` clauses bound the counters:
  - `EmitterRegistry`: `0 <= nextId <= maxEmitterId + 1` and `0 <= registrationFee <= maxRegistrationFee`.
  - `Emitter`: `0 <= emitterId <= maxEmitterId` and `0 <= sequence <= maxEmitterSequence`.
  - `maxEmitterId` is 2^53 - 1, so every tool reads an id exactly. `maxEmitterSequence` is one below the `Int` maximum.
  - `maxRegistrationFee` is 10^16 units of 10^-10, that is 10^6 fee-instrument units.
  - A registry at `maxEmitterId + 1` issues no more ids: the next registration fails on the `Emitter` `ensure` clause.
- The watcher drops each `PublishMessage` event that is not consuming.
  It accepts a consuming event with `acs_delta` true. The participant sets `acs_delta` only when the read-as party is a stakeholder of a contract that existed before the transaction.
  The participant also sets `acs_delta` false when the same transaction creates the `Emitter`, for example two publishes in one transaction, or `RegisterEmitter` and a publish.
  For such an event, the watcher finds the `CreatedEvent` with the same contract id in the transaction.
  The watcher accepts the event only if that `CreatedEvent` is of the pinned `Emitter` template and package, and its signatories include the read-as party.
  The ledger effects shape of the stream and of `GetUpdateById` includes these `CreatedEvent`s.
  Each drop increments `wormhole_canton_publish_without_read_party_stakeholder_total` and writes a WARN log with the update id.
  The check applies to the stream and to reobservation.
- The watcher rejects an `emitterId` above `cantonclient.MaxEmitterID` (2^53 - 1).
  `TestMaxEmitterIDMatchesDaml` pins it to `maxEmitterId`.
- `Test.TestCore:setup` creates `CoreState` and `EmitterRegistry` in one transaction.
  The operator and `guardianGovernance` sign that transaction.
- `canton/devnet/genesis_guardian_governance.sh` prepares each create command as its own transaction, `CoreState` first.
  The guardian owners sign each one with the threshold.
  The script stops before the `EmitterRegistry` create when `CoreState` is not committed.
  It skips a create whose contract is already active for the pair, so a rerun creates only the missing contract.
- The NTT package code does not change.
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
- `testRedTeamA14GovernanceAloneCannotCreateOrArchiveRegistry` (`Test/TestRedTeamCore.daml`).
  `guardianGovernance` alone cannot create, archive, or re-price a registry.
- `testRedTeamA14ColludingOwnerCannotDuplicateEmitter` (`Test/TestRedTeamCore.daml`).
  The operator and the owner, the operator as its own owner, and the operator and `guardianGovernance` cannot create a rewound `Emitter`.
  Each attempt fails with an authorization failure.
- `testRedTeamA14EmitterSignatoriesAreOperatorOwnerAndGovernance` and `testGovernanceSignsEveryEmitter`.
  The signatories of an `Emitter` are exactly the operator, the owner, and `guardianGovernance`.
- `testEmitterIdBounds` (`Test/TestCore.daml`). A table over each `ensure` bound, including the exhausted registry.
- `testRegisterEmitterChecksExpectedGuardianGovernance` (`Test/TestFees.daml`).
- `testSetRegistrationFee` (`Test/TestFees.daml`).
  The operator alone, `guardianGovernance` alone, and an outsider cannot change the fee.
  A negative fee fails. A fee above `maxRegistrationFee` fails.
  The operator and `guardianGovernance` together change the fee.
  `nextId` stays 2.
  The next registration pays the new fee and gets emitter id 2.
- `testRogueGovernanceEmitterCannotPublishOnRealCoreState` (`Test/TestGuardianVisibility.daml`).
  The operator and a rogue `guardianGovernance` can create a registry together.
  An emitter from that registry cannot publish against the real `CoreState`.
- `testGovernanceSeesRegistry` (`Test/TestGuardianVisibility.daml`).
  `guardianGovernance` is a stakeholder of the registry. An outsider is not.
- `testArchives` (`Test/TestCore.daml`). The operator alone cannot archive the registry.
- `TestMessagesFromTxRequiresReadPartyStakeholder` and `TestGetTransactionByUpdateIDRequiresReadPartyStakeholder` (`node/pkg/cantonclient`).
- `TestReadPartySignsExercisedEmitter`, `TestGetTransactionByUpdateIDOnTransientEmitters`, and `TestSubscribeUpdatesOnTransientEmitters` (`node/pkg/cantonclient`).
  A `CreatedEvent` of another package, of another template, of another contract id, or without the read-as party as a signatory does not unlock a publish.
- `TestCantonWatcherPublishesOnTransientEmitters` (`node/pkg/watchers/canton`, tag `integration`, fixture `Test/WatcherTransientEmitter.daml`).
  Two publishes on one `Emitter` in one transaction, and `RegisterEmitter` and a publish in one transaction.
  The watcher emits each message with its sequence, on the stream and on reobservation.
  The same transaction shape on a rogue registry is dropped, on the stream and on reobservation.
- `TestCantonWatcherDropsWitnessOnlyPublish` (`node/pkg/watchers/canton`, tag `integration`).
  The real `guardianGovernance` witnesses a nested publish on an `Emitter` of a rogue registry.
  The forged message has the identity and sequence of a trusted emitter.
  The watcher emits the trusted message.
  The watcher drops the forged message on the stream and on reobservation.
- `canton/tools/emitter_id_scan_test.sh`. A table-driven test of the scan against a JSON Ledger API stub.
  CI runs it in the job `canton-tools`.
  Each rule in [Scan](#scan) has a failing row and a passing row.

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
- Option 5 adds no new confirmation: each `PublishMessage` already needs the threshold through `CoreState`.
- A change of `registrationFee` needs a guardian ceremony that runs `SetRegistrationFee`.
  The choice keeps `nextId`, so the ceremony does not enter `nextId` by hand.

### Version decision

A change to the signatories of a template is not a valid smart contract upgrade (SCU) of a contract.
Canton recomputes the signatories when a choice runs on a contract under a newer package.
For a 0.7.0 registry or `Emitter`, the recomputed set includes `guardianGovernance`, but the stored set does not.
Thus the choice fails with `INTERPRETATION_UPGRADE_ERROR_VALIDATION_FAILED`.
A 0.7.0 registry or `Emitter` cannot continue under 0.8.0.

`damlc` reports each change only as the warning `upgraded-template-expression-changed`.

Decisions:

- Options 1 and 5 are both in `wormhole-core` 0.8.0. There is no 0.9.0.
  No participant outside LocalNet vetted 0.7.0 or 0.8.0, and no guardian signed a chain-75 VAA from them.
- The package name does not change. Thus 0.8.0 stays on the `wormhole-core` upgrade line.
- `core/daml.yaml` keeps the SCU check: `upgrades:` names the vendored 0.7.0 DAR (`dars/wormhole-core-0.7.0.dar`, `7a22a1c4`).
  The check covers all other templates and choices.
- 0.8.0 starts from a fresh genesis. There is no migration package for 0.7.0 contracts.

### Deployment of 0.8.0

Every emitter address includes the registrar. A new operator party thus gives new addresses for all emitters.
No message id of 0.8.0 can then be equal to a message id of an earlier deployment.

1. Build the dependents on 0.8.0 (NTT-first rule).
2. Allocate a new operator party. Do not reuse the operator party of an earlier deployment.
3. Upload the 0.8.0 DAR and the rebuilt dependent DARs to each guardian participant and to the operator participant.
4. Vet these DARs on each of these participants.
   Do not vet an earlier `wormhole-core` version on a guardian participant.
5. Run the genesis ceremony (`canton/devnet/genesis_guardian_governance.sh`).
   It creates `CoreState`, then an `EmitterRegistry` with `nextId = 0`, as two co-signed prepared transactions.
   If the `CoreState` create fails, the script stops. Run it again: it creates only the missing contracts.
6. Run the scan (refer to [Scan](#scan)) on each guardian participant.
   Use begin offset 0, `EXPECT_ACTIVE_REGISTRIES=1`, and `EXPECTED_CORE_PACKAGE_ID` set to the 0.8.0 package id.
   Each scan must pass with `firstId` 0. Stop if a scan fails.
7. Set `--cantonPackageID` to the 0.8.0 package id on each guardian.
8. Announce the new operator party, the 0.8.0 package id, and the registry to integrators (refer to [Integrator cutover](#integrator-cutover)).

> **WARNING:** A guardian participant that vets an earlier `wormhole-core` version lets a submitter run choices under that version.
> The scan fails on such a participant.

> **CAUTION:** The prepared-transaction check of the ceremony is not yet implemented.
> Each owner signs the hash that the preparing participant returns.
> An owner that does not trust that participant must decode the prepared transaction and compare the hash with an independent computation before it signs.

### Integrator cutover

1. Integrators that depend on `wormhole-core` rebuild on 0.8.0. NTT is the first dependent (NTT-first rule).
2. Each integrator registers a new `Emitter` through the 0.8.0 registry, with `expectedGuardianGovernance = Some gg`.
   The new emitter has a new address, because the registrar is new.
3. Integrators register the new emitter address with their peers on other chains.
4. Integrators get the disclosure of the registry and `CoreState` from the disclosure service (README §4.7).

### Scan

`canton/tools/emitter_id_scan.sh` reads the ledger as `guardianGovernance`.
The token must belong to a user with exactly one right: `CanReadAs(guardianGovernance)`.
The script stops if the user has a different set of rights.
Each Ledger API call has a time limit (`CURL_MAX_TIME_SECONDS`, default 60).

The script reads the vetted packages of its participant with the name prefix `wormhole-core`.
It reads each update from a begin offset to the ledger end, with the ledger effects shape.
It records each `Emitter` and `EmitterRegistry` create of the (operator, `guardianGovernance`) pair, archived or active, whose package name is exactly `wormhole-core`.
It also reads the active contracts at the same ledger end, with the same filter.

Use begin offset 0 to read from genesis.
A pruned participant needs a begin offset and the `historyMaxEmitterId` of an earlier scan that ended at that offset.

`firstId` is the highest history `emitterId` + 1.
The `nextId` of a history registry has no effect on `firstId`. The summary reports it only.

The script fails if one of these conditions occurs:

- The participant vets a `wormhole-core` package other than `EXPECTED_CORE_PACKAGE_ID`, or does not vet `EXPECTED_CORE_PACKAGE_ID`.
- Two active `Emitter` contracts have the same (operator, owner, emitterId).
- An `emitterId` is not a canonical decimal integer in 0 to 2^53 - 1, or a registry `nextId` is not one in 0 to 2^53.
- An active `Emitter` is not in the update history from genesis.
- An active `Emitter` has an id at or above `firstId`.
- The number of active registries is not `EXPECT_ACTIVE_REGISTRIES` (default 0).
- An active registry has `nextId` other than `firstId`.

### NTT

NTT must be rebuilt on `wormhole-core` 0.8.0 (NTT-first rule).
`RegisterManager` and `RegisterManagerByVaa` need no code change.
`RegisterManager` must pass `expectedGuardianGovernance = Some guardianGovernance` to `RegisterEmitter`.
The NTT test package needs changes: tests that create or archive a registry or an `Emitter` without `guardianGovernance` now fail.

### LocalNet rehearsal

The LocalNet rehearsal of 2026-11-17 tested a migration from 0.7.0 to a 0.8.0 that changed only `EmitterRegistry`.
Option 5 also changes `Emitter`, so that migration path does not apply.
This branch removes its script.
Results of that run that still apply:

- Each participant accepted the 0.8.0 upload as an upgrade of 0.7.0.
- A choice on a 0.7.0 contract whose signatories 0.8.0 changes failed with `INTERPRETATION_UPGRADE_ERROR_VALIDATION_FAILED`.
- After the unvet of 0.7.0, each 0.7.0 submission failed with `INVALID_PRESCRIBED_SYNCHRONIZER_ID`.
- The operator alone could not create the 0.8.0 registry (`DAML_AUTHORIZATION_ERROR`).
  The operator and one owner key could not create it (`FAILED_TO_EXECUTE_TRANSACTION`).
  The operator and two of three owner keys created it.

### Open items

These items need a Daml change, thus a new `wormhole-core` package id. They are not in 0.8.0.

- Make `expectedGuardianGovernance` of `RegisterEmitter` a mandatory `Party`. With `Optional`, a client can skip the pin.
- `TestRedTeamCore.daml` "A9 signature count zero" expects `ArithmeticError`, not the `no quorum` check of `verifyVAA`.
  Add a case that also removes the signature bytes, and expect `no quorum`.

## Related operator-only contracts

| Contract | Signatories | Same class as A14-3 | Severity | Action |
| - | - | - | - | - |
| `CoreState` | operator, `guardianGovernance` | No. | None. | None. |
| `ReplayRootRegistry` | operator | No. It has no counter. Each claim needs the consumer signature. A rogue registry at fee 0 gives the same result as the real one. | None. The operator alone can archive it and stop new claims (liveness). | None. |
| `ReplayNode` | consumer, operator | No. The operator alone cannot create a root for a consumer. | None. | None. |
| `Emitter` | operator, owner, `guardianGovernance` | No. A rewind needs the guardian threshold (A14-2, option 5). | None. | None. |
| NTT `NttGovernance` | `guardianGovernance`, operator | No. The `nextId` counter needs the `guardianGovernance` signature. | None. | None. |
| NTT `NttManager`, `LockedLedger` | include `guardianGovernance` | No. Only `RegisterManager` creates them. | None. | None. |

No other core contract has an operator-only counter.
