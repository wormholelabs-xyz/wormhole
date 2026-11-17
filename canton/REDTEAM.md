# Canton Core Bridge Red-Team Results

## Scope

This document records attacks against the Canton core bridge and the guardian watcher.
Each attack has a test that asserts the attack fails.
A failing test identifies an open finding.
The Results table gives the fix commit for each fixed finding.

## Test Locations

- Daml Script tests: `canton/test/daml/Test/TestRedTeamCore.daml`.
- Signed VAA fixtures: `node/pkg/cantonclient/redteam_vectorgen_test.go`.
- Watcher integration test: `node/pkg/watchers/canton/watcher_redteam_integration_test.go`.
- Look-alike package: `canton/test-lookalike`.

## Procedure

1. Run `dpm build --all` in `canton`.
2. Run `dpm test` in `canton/test`.
3. Run `go test -count=1 ./pkg/cantonclient/` in `node` to check the fixtures.
4. Run `go test -count=1 -tags integration -run TestCantonWatcherIgnoresLookalikePackage ./pkg/watchers/canton` in `node`.

To regenerate the fixtures, run step 3 with `CANTON_VECTORGEN=write`.

## Results

| ID | Attack | Result | Evidence | Severity | Proposed fix |
| - | - | - | - | - | - |
| A1 | Skip or redirect a fee. Use a shadow `CoreState` or omit the fee input. | Held. | `testRedTeamA1FeeCannotBeSkipped`; `TestFees.test*PinsGuardianGovernance`; `TestCantonWatcherGovernanceReaderIntegration`. | None. | None. |
| A2 | Forge a message from a look-alike package. | Held. | `TestCantonWatcherIgnoresLookalikePackage`. | None. | Keep the package-id pin. |
| A6-1 | Send a bad guardian set upgrade. | Held. | `testRedTeamA6GuardianSetUpgradeRules`. | None. | None. |
| A6-2 | Sign a VAA with bad signatures or the superseded set. | Held. | `testRedTeamA6QuorumAndSignerOrder`. | None. | None. |
| A6-3 | Use an expired guardian set. | Held. Fixed an off-by-one in a66170ea7. | `testRedTeamA6ExpiredGuardianSet`. | None. | None. |
| A9-1 | Send a malformed VAA encoding. | Held. | `testRedTeamA9MalformedEncodings`; `testRedTeamA9MalformedGovernancePayloads`. | None. | None. |
| A9-2 | Send a governance payload with a trailing byte. | Fixed in 15684243d. | `testRedTeamA9GovernancePayloadExactLength`; `testParseGovernanceRequiresExactPacketLength`. | Low. | None. |
| A9-3 | Publish fields that the watcher drops. | Fixed in 4143b6c74. | `testRedTeamA9PublishRejectsFieldsTheWatcherDrops`; `testRedTeamA9PublishFieldBoundaries`. | Low. | None. |
| A9-4 | Publish at the payload size limit. | Held. | `testRedTeamA9PublishPayloadBoundary`. | None. | None. |
| A9-5 | Replay a VAA in uppercase hex. | Held. | `testRedTeamA9HexCaseReplay`. | None. | None. |
| A14-1 | The operator creates an emitter for a third party. | Held. | `testRedTeamA14OperatorCannotForgeThirdPartyEmitter`. | None. | None. |
| A14-2 | The operator and the owner rewind the owner sequence. | Broken by design. | `testRedTeamA14ColludingOwnerDuplicatesOnlyOwnAddress` passes. | Low. | Record the limit. |
| A14-3 | The operator resets the registry counter. | Open. | `testRedTeamA14RegistryResetDuplicatesHonestOwnerAddress` fails. | Medium. | See [A14_EMITTER_ID_UNIQUENESS.md](A14_EMITTER_ID_UNIQUENESS.md). |
| R1 | Publish inside `try` and roll back. | Held. | `testRedTeamR1RollbackOfPublishRejected`. | None. | None. |
| R2 | Run attacker code in the fee charge. | Held. | `TestFees.testFeeValidationMatrix`. | None. | None. |
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
The payload tests publish 0, 749, 750, and 751 bytes.
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

- The payload is lowercase hex with an even length.
- The payload is 750 bytes or less.
- The nonce is in the range 0 to 4294967295.
- The consistency level is in the range 0 to 255.

### A14: Blast radius

The `Emitter` address is `keccak256(tag, registrar, owner, emitterId)`.
The owner is a signatory, so the operator cannot make a third-party address alone.

With the owner signature, the operator and the owner can repeat a sequence.
The effect stays on the address of that owner.
Other emitters do not change.
`guardianGovernance` observes both `Emitter` contracts.
The watcher emits both messages.
The processor (`node/pkg/processor/message.go:36`) signs each digest independently.
Thus, two VAAs with one message id can reach quorum.

The registry attack needs only a compromised operator.
`EmitterRegistry` has the operator as its only signatory (`State.daml:382`).
The operator can create a registry with any `nextId`.
`RegisterEmitter` does not check that the owner already has the id.
An honest owner that registers a second emitter can get the address of its first emitter.
Both emitters start at sequence 0.
Integrators that use the sequence for ordering or replay protection can fail.
This finding is open.
[A14_EMITTER_ID_UNIQUENESS.md](A14_EMITTER_ID_UNIQUENESS.md) compares four fixes and recommends one.

### R1: Rolled-back publish

The Daml engine rejects the transaction with `EffectfulRollbackError`.
A consuming exercise cannot be in a rollback node.
Thus, the ledger stream does not contain a rolled-back `PublishMessage`.

### R2: Fee allocation callback

`chargeFee` runs `Allocation_ExecuteTransfer` on an allocation that the payer supplies.
That choice can run any code.
A nested `PublishMessage` in a transaction that `guardianGovernance` sees can reach the watcher.
`chargeFee` first requires the instrument admin as a signatory (`Fees.daml:96-97`).
Thus, only the instrument admin implementation runs.

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
