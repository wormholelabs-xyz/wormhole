# Canton Core Bridge Red-Team Results

## Scope

This document records attacks against the Canton core bridge and the guardian watcher.
Each attack has a test that asserts the attack fails.
A failing test identifies an open finding.
The Results table gives the fix commit for each fixed finding.

## Test Locations

- Daml Script tests: `canton/test/daml/Test/TestRedTeamCore.daml` and `canton/test/daml/Test/TestOperatorIndependence.daml`.
- Signed VAA fixtures: `node/pkg/cantonclient/redteam_vectorgen_test.go`.
- Watcher integration tests: `node/pkg/watchers/canton/watcher_redteam_integration_test.go` and `watcher_witness_only_integration_test.go`.
- Look-alike package: `canton/test-lookalike`.

## Procedure

1. Run `dpm build --all` in `canton`.
2. Run `dpm test` in `canton/test`.
3. Run `go test -count=1 ./pkg/cantonclient/` in `node` to check the fixtures.
4. Run `go test -count=1 -tags integration -run 'TestCantonWatcherIgnoresLookalikePackage|TestCantonWatcherDropsWitnessOnlyPublish' ./pkg/watchers/canton` in `node`.

To regenerate the fixtures, run step 3 with `CANTON_VECTORGEN=write`.

## Results

| ID | Attack | Result | Evidence | Severity | Proposed fix |
| - | - | - | - | - | - |
| A1 | Skip or redirect a fee. Use a shadow `CoreState` or omit the fee input. | Held. | `testRedTeamA1FeeCannotBeSkipped`; `TestFees.test*PinsGuardianGovernance`; `testShadowAnchorsAreRejected`; `TestCantonWatcherGovernanceReaderIntegration`. | None. | None. |
| A1-2 | Redeem a forged VAA on an integrator through a shadow `CoreState` under a throwaway `guardianGovernance`. | Fixed on `canton/integrator-gg-pin`: `ExampleIntegrator.Redeem` pins `guardianGovernance`. 0.9.0 has no operator pin. | `Test.ExampleIntegrator.testIntegratorCoreStatePin`; `testOutsiderCannotForgeCoreStateOfRealGuardianGovernance`. | Medium. | Integrators pin `guardianGovernance`. |
| A2 | Forge a message from a look-alike package. | Held. | `TestCantonWatcherIgnoresLookalikePackage`. | None. | Keep the package-id pin. |
| A6-1 | Send a bad guardian set upgrade. | Held. | `testRedTeamA6GuardianSetUpgradeRules`. | None. | None. |
| A6-2 | Sign a VAA with bad signatures or the superseded set. | Held. | `testRedTeamA6QuorumAndSignerOrder`. | None. | None. |
| A6-3 | Use an expired guardian set. | Held. Fixed an off-by-one in a66170ea7. | `testRedTeamA6ExpiredGuardianSet`. | None. | None. |
| A9-1 | Send a malformed VAA encoding. | Held. | `testRedTeamA9MalformedEncodings`; `testRedTeamA9MalformedGovernancePayloads`. | None. | None. |
| A9-2 | Send a governance payload with a trailing byte. | Fixed in 15684243d. | `testRedTeamA9GovernancePayloadExactLength`; `testParseGovernanceRequiresExactPacketLength`. | Low. | None. |
| A9-3 | Publish fields that the watcher drops. | Fixed in 4143b6c74. | `testRedTeamA9PublishRejectsFieldsTheWatcherDrops`; `testRedTeamA9PublishFieldBoundaries`. | Low. | None. |
| A9-4 | Publish a payload above 750 bytes. | Accepted by design: no core bridge caps the payload size (EVM parity). | `testRedTeamA9PublishAcceptsAnyPayloadSize`; `TestSubscribeUpdatesUpdateSizeLimit`. | None. | The watcher receive limit `maxLedgerUpdateBytes` bounds one update. |
| A9-5 | Replay a VAA in uppercase hex. | Held. | `testRedTeamA9HexCaseReplay`. | None. | None. |
| A14-1 | A non-owner (in 0.8.x, the operator) creates an emitter for a third party. | Held. 0.9.0 removes the operator. | `testRedTeamA14FormerOperatorCannotForgeThirdPartyEmitter`; `testFormerOperatorHasNoCorePower`. | None. | None. |
| A14-2 | The owner (in 0.8.x, with the operator) rewinds the owner sequence. | Fixed on `canton/overnight-u2-fixes`: `guardianGovernance` co-signs `Emitter`. In 0.9.0, the owner and `guardianGovernance` sign it. | `testRedTeamA14ColludingOwnerCannotDuplicateEmitter`; `testRedTeamA14EmitterSignatoriesAreOwnerAndGovernance`; `testGovernanceSignsEveryEmitter`; `testCoreTemplateSignatoriesAndObservers`. | Low. | Fresh genesis of 0.9.0 ([A14](A14_EMITTER_ID_UNIQUENESS.md)). |
| A14-3 | A party resets the registry counter (in 0.8.x, the operator). | Fixed in #75. In 0.9.0, a reset needs `guardianGovernance` alone (k-of-n ceremony). | `testRedTeamA14FormerOperatorCannotResetEmitterRegistry`; `testRedTeamA14RegistryNeedsGuardianGovernance`; `testSetRegistrationFee`; `testOnboardingFeeAdminChangesOnlyOnboardingFees`; scan test. | Medium. | Fresh genesis of 0.9.0 ([A14](A14_EMITTER_ID_UNIQUENESS.md)). |
| A14-4 | Publish on a rogue-`guardianGovernance` `Emitter` inside a transaction that the real `guardianGovernance` witnesses. | Fixed on `canton/overnight-u2-fixes`: the watcher drops a publish where its read-as party is not a stakeholder. | `TestMessagesFromTxRequiresReadPartyStakeholder`; `TestCantonWatcherDropsWitnessOnlyPublish`. | Medium. | None. |
| R1 | Publish inside `try` and roll back. | Held. | `testRedTeamR1RollbackOfPublishRejected`. | None. | None. |
| R2 | Run attacker code in the fee charge with an allocation that the instrument admin did not sign. | Held. The admin-signed registry code is F1-core. | `TestFees.testFeeValidationMatrix`. | None. | None. |
| F1-core | Fee registry code acts as `guardianGovernance` or the `operator`. | Fixed in `wormhole-core` 0.8.3 (`feeCustody`). 0.9.0 removes the operator and rejects `feeCustody` as the fee instrument admin. | `testRedTeamF1FeeRegistryCannotActAsGuardianGovernance`; `testRedTeamF1FeeRegistryCannotPublishOnGuardianGovernanceEmitter`; `testRedTeamF1PrivilegedPartyCannotPayAFee`; `testCoreStateFeeRecipientIsADistinctCustodyParty`; `testFeeRegistryActsOnlyAsFeeCustody`; `testFeeCustodyReceivesFeesOnEveryChargePoint`; `testRedTeamF1FeeInstrumentAdminCannotPublishOnGuardianGovernanceEmitter`; `testChargeFeeRejectsEachPrivilegedParty`; `testCoreStateEnsureRejectsInconsistentGenesis`; `testGuardianGovernanceIsNeverAFeeAllocationController`. | High (0.8.0 to 0.8.2, first 0.8.3 and 0.9.0 builds). | Withdraw 0.8.0, 0.8.1, 0.8.2 and 0.8.3. |
| OI-1 | A former operator, or a relayed governance VAA, changes core state. | Held in 0.9.0. Each core function needs only caller authority. Governance keeps the genesis fields. | `testEachCoreFunctionNeedsOnlyCallerAuthority`; `testFormerOperatorHasNoCorePower`; `testRelayedGovernanceRejectsInvalidVAA`; `testGovernanceConsumedDigestCheck`; `testGovernanceGenesisFieldChecksRejectEachChange`; `testEmitterAddressUsesGuardianGovernanceAsRegistrar`. | None. | None. |
| R3 | Vet a malicious upgrade of `wormhole-core`. | Not tested. | None. | High if it occurs. | Control package vetting. |

## Details

### A2: Look-alike package

The forged message names a real emitter identity and sequence.

The look-alike `Emitter` has only the forger as signatory.
Its observer is the real `guardianGovernance`.
The control watcher has no package pin.
It emits a message with the victim address and the victim sequence.
The pinned watcher receives the same transaction and drops it.
`WatcherConfig.Create` (`config.go:60`) requires the pin outside unsafe dev mode.

### A6: Guardian set

The tests send these upgrades: a skipped, equal, or lower index.
They also use a wrong emitter chain, emitter address, or target chain, and an empty set.
The signature tests use fewer than quorum, a duplicate index, and an out-of-order index.
They also use a wrong key, an index out of range, and the superseded set for governance.
The superseded set checks VAAs through second 86400 and fails at second 86401.
EVM `Messages.sol:80` also accepts the expiry second.
The check truncates both times to whole seconds, as EVM `block.timestamp` does.
Thus a VAA at expiry plus 999999 microseconds passes.
Before a66170ea7, Canton failed at second 86400.

### A9: Malformed encodings

The tests use a truncated header, a truncated signature, a short body, and a trailing byte.
They change the signature count and the version.
They also use odd-length, non-hex, and empty input.
The payload tests publish 0, 750, 751, and 65536 bytes.
An uppercase replay fails with `action already consumed` or `digest already consumed`.

### A9: Governance payload length

Before 15684243d, `parseGovernance` read fixed offsets and ignored the bytes after them.
A quorum of guardians must sign the packet, so the exposure was low.
EVM rejects the packet, so the gap was a parity gap.

Fix: `parseGovernance` (`Governance.daml`) requires the exact length of each action.
The lengths come from the guardian serializers in `sdk/vaa/payloads.go`:

- ContractUpgrade: 67 bytes (`payloads.go:386-399`).
- GuardianSetUpgrade: 40 + 20 × key count bytes (`payloads.go:402-418`).
- SetMessageFee: 67 bytes (`payloads.go:621-631`).
- TransferFees: 99 bytes (`payloads.go:645-671`).

### A9: Fields that the watcher drops

Before 4143b6c74, `PublishMessage` checked only `byteLength payload <= 750`.
`byteLength` rounds down, so 1501 hex characters passed as 750 bytes.
`decodeWormholeMessage` (`cantongrpc.go:233-290`) rejects the bad fields.
`messagesFromTx` (`cantongrpc.go:191-198`) logs the error and drops the message.
The emitter sequence increased, and the payer paid the fee.
The guardians did not sign a VAA for that sequence.
An integrator that passed user input to these fields could lose user funds.

Fix: `PublishMessage` (`State.daml`) checks these conditions before the fee charge:

- The payload is a well-formed `MessagePayload`: lowercase hex with an even length (`README.md` §4.8).
- The nonce is in the range 0 to 4294967295.
- The consistency level is in the range 0 to 255.

### A14: Blast radius

The `Emitter` address is `keccak256(tag, registrar, owner, emitterId)`.
The owner is a signatory, so no other party makes the address of that owner alone.

In 0.7.0, the operator and the owner could create a second `Emitter` of the owner with a repeated sequence.
The processor (`node/pkg/processor/message.go:36`) signs each digest independently.
Thus, two VAAs with one message id could reach quorum.
In 0.8.0, `guardianGovernance` co-signs each `Emitter` (`State.daml:579`).
A second `Emitter` of an issued identity then needs the guardian threshold.

In 0.7.0, the registry attack needs only a compromised operator.
`EmitterRegistry` has the operator as its only signatory.
The operator can create a registry with any `nextId`.
`RegisterEmitter` does not check that the owner already has the id.
An honest owner that registers a second emitter can get the address of its first emitter.
Both emitters start at sequence 0.
Integrators that use the sequence for ordering or replay protection can fail.

Fix (`wormhole-core` 0.8.0): `guardianGovernance` co-signs `EmitterRegistry` (`State.daml:407`).
The operator alone cannot create, archive, or re-price a registry.
`guardianGovernance` alone cannot do these actions either.
`SetRegistrationFee` keeps `nextId`.
A registry under a rogue `guardianGovernance` cannot publish against the real `CoreState`.
It can publish against a shadow `CoreState` of the same rogue party.
The fix changes signatories, so a 0.7.0 registry or `Emitter` cannot continue under 0.8.0.
0.8.0 starts from a fresh genesis with a new operator party.
0.9.0 removes the operator. `guardianGovernance` is the sole signatory of `EmitterRegistry` and the registrar of each emitter address.
A registry reset then needs `guardianGovernance` alone, that is the k-of-n guardian ceremony.
[A14_EMITTER_ID_UNIQUENESS.md](A14_EMITTER_ID_UNIQUENESS.md) gives the options, the change, the deployment, and the scan.

A14-4: `guardianGovernance` witnesses each node below a node where it is an informee.
Thus a `PublishMessage` on a rogue-`guardianGovernance` `Emitter` below such a node reaches the stream of `guardianGovernance`.
`messagesFromTx` (`node/pkg/cantonclient/cantongrpc.go`) accepts a consuming `PublishMessage` event only when the read-as party is a stakeholder of the `Emitter`.
`acs_delta` true shows this for an `Emitter` that existed before the transaction.
For an `Emitter` that the same transaction creates, `acs_delta` is false. Then the `CreatedEvent` of that contract id must be of the package id that executed the publish, and must list the read-as party as a signatory.
`TestCantonWatcherDropsWitnessOnlyPublish` and `TestCantonWatcherPublishesOnTransientEmitters` confirm this on SDK 3.7.3 (fixtures `Test/WatcherWitnessOnly.daml` and `Test/WatcherTransientEmitter.daml`).

### R1: Rolled-back publish

The Daml engine rejects the transaction with `EffectfulRollbackError`.
A consuming exercise cannot be in a rollback node.
Thus, the ledger stream does not contain a rolled-back `PublishMessage`.

### R2: Fee allocation callback

`chargeFee` runs `Allocation_ExecuteTransfer` on an allocation that the payer supplies.
That choice can run any code.
A nested `PublishMessage` in a transaction that `guardianGovernance` sees can reach the watcher.
`chargeFee` first requires the instrument admin as a signatory.
Thus, only the instrument admin implementation runs.
That implementation runs with the authority of the allocation controllers. Refer to F1-core.

### F1-core: Fee registry authority

`Allocation_ExecuteTransfer` has three controllers: the executor, the sender and the receiver.
The executor and the sender are the payer. The receiver is `feeRecipient`.
The fee registry body runs with the authority of these controllers.
It also runs with the authority of the allocation signatories. `chargeFee` requires the instrument admin among them.
In 0.8.0 to 0.8.2, `feeRecipient` was `guardianGovernance`.
Thus the registry body ran as `guardianGovernance` on each paid publish, registration and replay-root claim.
With that authority, it can publish on each `guardianGovernance`-owned `Emitter`. The guardians sign the message.
A payer equal to `guardianGovernance` or the `operator` gives the same result.
A fee instrument admin equal to `guardianGovernance` or the `operator` gives the same result.
The first 0.8.3 build (`b5613c61`) accepted such an admin. The devnet genesis and the default test setup used it.

In 0.8.3:

- `feeRecipient` is `feeCustody`, a quorum party in the guardian namespace. It co-signs `CoreState`.
- The `CoreState` precondition rejects `feeRecipient` equal to `guardianGovernance` or the `operator`.
- The `CoreState` precondition rejects a fee instrument admin equal to `guardianGovernance` or the `operator`.
- When a fee is due, `chargeFee` rejects `guardianGovernance` and the `operator` as payer or receiver.
- When a fee is due, `chargeFee` also rejects them as the fee instrument admin.
- `chargeFee` requires the allocation executor to be the payer.

In 0.9.0 (no operator):

- The `CoreState` precondition rejects `feeRecipient` equal to `guardianGovernance`.
- The `CoreState` precondition rejects a fee instrument admin equal to `guardianGovernance` or `feeCustody`.
- When a fee is due, `chargeFee` rejects `guardianGovernance` as payer or receiver.
- When a fee is due, `chargeFee` also rejects `guardianGovernance` and `feeCustody` as the fee instrument admin.
- The first 0.9.0 build (`dd75f777`) accepted `guardianGovernance` as the fee instrument admin.

Tests:

- `testRedTeamF1FeeRegistryCannotActAsGuardianGovernance`
- `testRedTeamF1FeeRegistryCannotPublishOnGuardianGovernanceEmitter`
- `testRedTeamF1FeeInstrumentAdminCannotPublishOnGuardianGovernanceEmitter`
- `testRedTeamF1PrivilegedPartyCannotPayAFee`
- `testRedTeamF1CoreStateChargeChoicesRejectPrivilegedPayer`
- `testChargeFeeRejectsEachPrivilegedParty`
- `testCoreStateFeeRecipientIsADistinctCustodyParty`
- `testCoreStateEnsureRejectsInconsistentGenesis`
- `testFeeRegistryActsOnlyAsFeeCustody`
- `testFeeCustodyReceivesFeesOnEveryChargePoint`
- `testGuardianGovernanceIsNeverAFeeAllocationController`

Residual 1: the registry code runs as `feeCustody`. That registry is the instrument admin of the only holdings of
`feeCustody`. `testFeeRegistryActsOnlyAsFeeCustody` shows this residual.
With the authority of `feeCustody`, the registry code can register an `Emitter` that `feeCustody` owns.
It can then publish on that `Emitter`. No contract and no peer chain gives a meaning to such a message.

Residual 2: the registry code runs as the payer. It can publish on an `Emitter` that the payer owns.
The guardians sign that message. Use a fee payer that owns no `Emitter`.
The NTT payers do not own the transceiver `Emitter`: `gg` owns it.

### R3: Upgrade package

A package with the name `wormhole-core` and a higher version is a smart contract upgrade.
A submitter can select the upgrade to exercise a choice on an existing contract.
The choice body runs with the authority of the contract signatories.
On `CoreState`, the body can create a `CoreState` with a different guardian set.
The transaction needs the upgrade vetted on every informee participant.
The package-id pin stops forged messages at the watcher.
The pin does not stop a forged `CoreState`.
A test needs more than one participant, so this attack has no test.
Fix: guardians vet only the package ids that governance approves.
