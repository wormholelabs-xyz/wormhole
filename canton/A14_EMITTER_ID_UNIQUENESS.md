# A14-3: Emitter Address Uniqueness

## Status

This note is a design proposal. The fix is not in the code yet.
The test `testRedTeamA14RegistryResetDuplicatesHonestOwnerAddress` fails until a fix lands.

## Problem

Wormhole needs a unique message id on every runtime.
The message id is the tuple (chain, emitter address, sequence).

The Canton emitter address is `keccak256(tag, registrar, owner, emitterId)`.
`emitterAddressFor` computes it (`canton/core/daml/Wormhole/Core/State.daml:374-376`).
The watcher computes the same value (`node/pkg/watchers/canton/watcher.go:62-80`).

The `emitterId` comes from the `nextId` field of `EmitterRegistry` (`State.daml:382-441`).
The operator is the only signatory of `EmitterRegistry` (`State.daml:391`).
Thus the operator alone can create a new registry with any `nextId`.
`mkInitialEmitterRegistry` starts each registry at 0 (`canton/core/daml/Wormhole/Core/Setup.daml:62-64`).

An honest owner that registers through the new registry gets an id that it already has.
`RegisterEmitter` then creates an `Emitter` with the old address and sequence 0 (`State.daml:433-440`).
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
