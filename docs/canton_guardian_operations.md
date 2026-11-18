# Canton Guardian Operations

This document gives the operation responsibilities of a Wormhole guardian on the Canton Network.
It is a draft. Items with the label **Open item**, or with an ID U1 to U3, need a decision before MainNet.

For the design of the Canton core bridge and watcher, refer to [`canton/README.md`](../canton/README.md).
For the LocalNet rehearsal of each ceremony, refer to [`canton/localnet/README.md`](../canton/localnet/README.md).
For general guardian operation, refer to [`operations.md`](operations.md).

## 1. Summary of responsibilities

| Responsibility | Section | Frequency |
| - | - | - |
| Operate a Canton participant node on the Global Synchronizer | 3 | Continuous |
| Keep the participant on the minimum Splice version | 3.2 | Weekly releases |
| Host the `guardianGovernance` party | 4 | Once, then on changes |
| Hold one `guardianGovernance` owner key | 5 | Continuous |
| Sign `guardianGovernance` ceremonies | 5.2, 7 | On request |
| Apply the package vetting policy | 6 | On each new package version |
| Do the core package upgrade procedure | 6.7 | On each new `wormhole-core` version |
| Check fee withdrawals | 7.6 | On request |
| Respond to a key compromise or a guardian removal | 7.7 | On event |
| Configure and monitor the Canton watcher in `guardiand` | 8 | Continuous |
| Send reobservation requests after each restart of `guardiand` | 8.4 | On each restart |
| Upgrade all guardians that observe Canton together | 8.1 | On each `TxID` format change |
| Consolidate idle Canton Coin custody pots (custodians) | 9.2 | Before the expiry round of each pot |

## 2. Parties

Each guardian participant hosts one Wormhole party: `guardianGovernance` (`gg`).
No other guardian party exists.

| Property | Value |
| - | - |
| Hosting permission | `Confirmation` on every guardian participant. Never `Submission`. |
| Confirmation threshold | Greater than 1, and a majority of the hosts. One participant cannot confirm for `gg` alone. |
| Owner keys | One Ed25519 key for each guardian. A threshold of owner keys controls the `gg` namespace. |
| Signatory of | `CoreState`, `EmitterRegistry`, every `Emitter` and every NTT manager. |
| Messages | Each `PublishMessage` runs on an `Emitter` that `gg` signs. Thus guardians see each message. |
| Other roles | Fee recipient. Mint authority for Wormhole-administered tokens. |
| Watcher | `guardiand` reads the update stream as `gg` (`--cantonReadAsParty`). |

The `operator` party is not a guardian party. Wormhole Labs operates it.
Each deployment of `wormhole-core` 0.8.0 starts with a new operator party (Section 7.2).

`gg` owns the transceiver `Emitter` of each NTT deployment.
This includes a lock/unlock deployment that its admin registered alone.
Thus a `gg`-owned emitter does not show that the guardians approved the deployment.

### 2.1 Why the hosting permission is a MainNet gate

A host at `Submission` permission can submit a transaction for `gg` alone.
Thus a host at `Submission` removes the need for the owner key threshold.
A confirmation threshold of 1 lets one compromised participant confirm for `gg`.
Before MainNet, make sure that each guardian participant hosts `gg` at `Confirmation`.
Also make sure that the confirmation threshold is greater than 1 and a majority of the hosts.

### 2.2 Why `gg` hosting is a security control

`gg` signs a contract once.
After that, the choices of the contract run with the authority of `gg`.
No quorum signature is necessary at that time.
Thus, the code that runs with the authority of `gg` must be code that the guardians approved.

Canton enforces this through package vetting.
A transaction can use a package only if each participant that hosts an informee vetted that package.
Each guardian participant hosts `gg`.
Thus, a new package version cannot run in a `gg` transaction until each guardian participant vets it.

Package vetting is a topology transaction of the participant.
The participant key signs it, not the `gg` threshold keys.
Thus, the vetting decision of each guardian operator is a part of the `gg` trust model.

### 2.3 Rehearsal results for hosting

LocalNet hosts `gg` on three participants at `Confirmation` with a confirmation threshold of 2.
[`gg_confirmation_quorum.sh`](../canton/localnet/gg_confirmation_quorum.sh) disconnects two of the three hosts.
With fewer hosts online than the threshold, each `gg` transaction fails.
The error is `MEDIATOR_SAYS_TX_TIMED_OUT`, and it names `gg` as the only unresponsive party.
This failure affects governance VAAs, VAA checks and message publication.
With the threshold number of hosts online again, the governance VAA succeeds.
This rehearsal passed live on 2026-11-17 with `wormhole-core` 0.6.0 (Section 7.5).

CAUTION: With hosts offline, the synchronizer clock can stop.
The mediator timeout uses this clock.
Thus a request can wait for many minutes, and a host that connects again can still confirm it.
Do not treat a request that has no result as a failed request.
Make sure that a request failed before you submit it again.

Each guardian participant that goes offline reduces the margin to the threshold.
Thus participant availability is a liveness responsibility of each guardian.

## 3. Participant node

### 3.1 Requirements

- One Canton participant node per guardian, connected to the Global Synchronizer.
- The node runs as a Splice validator.
  For deployment, refer to the Global Synchronizer documentation at <https://docs.canton.network/global-synchronizer/understand/introduction.md>.
- Onboarding to each network needs an IP allowlist entry.
- Onboarding also needs an onboarding secret from a sponsor super validator.
- The participant needs traffic on the synchronizer.
  You buy traffic with Canton Coin.

**Open item:** the sponsor super validator for each guardian.

### 3.2 Version currency

The Global Synchronizer has weekly Splice releases.
The super validators set a minimum Splice version and a minimum Splice DAR version.
A participant below the minimum version cannot operate on the network.

1. Subscribe to the SV Operations Schedule at <https://sv-cal.canton.foundation/>.
2. Upgrade the participant before each minimum version date.
3. Read the Splice release notes before each upgrade.
4. Read the Canton release notes before each upgrade.

LocalNet uses the versions in [`canton/localnet/env.sh`](../canton/localnet/env.sh).
Refer to the Version Compatibility Dashboard for the MainNet versions.

### 3.3 Logical Synchronizer Upgrades

The super validators upgrade the protocol version through a Logical Synchronizer Upgrade (LSU).
An LSU has a topology freeze time and an upgrade time.
The period from the freeze time to the upgrade time is the LSU freeze window.

- In the LSU freeze window, no topology transaction is possible.
  Thus, you cannot vet packages, change party hosting or change owner keys in that period.
- A transaction that you prepare before the upgrade time fails if you submit it after the upgrade time.

1. Find the LSU events on the SV Operations Schedule.
2. Do not plan a `gg` ceremony in an LSU freeze window.
3. Do not plan a vetting change in an LSU freeze window.
4. Upgrade the participant before the upgrade time.
5. Use a release that supports the new protocol version.
6. If a `gg` ceremony is in progress at the upgrade time, stop it.
7. After the upgrade, prepare the transaction again.
8. Sign the new transaction hash.

### 3.4 Keys and backup

From Splice 0.10.0, the default configuration disables participant key download and repair commands.
Without key download, the identity dump does not include private keys.

1. Store the participant keys in a KMS.
2. Back up the participant database.
3. Test the restore procedure on DevNet.

**Open item:** the KMS provider for guardian participants.

## 4. Party hosting

Host the party on your participant before the genesis of the core bridge.
A participant that starts to host the party later does not receive prior contracts.
For a late start, Canton party replication or an ACS import is necessary.

### 4.1 Host `guardianGovernance`

1. Get the `guardianGovernance` party ID from the Wormhole Canton deployment record (Section 6.0).
2. Sign the `PartyToParticipant` proposal for your participant at `Confirmation` permission.
3. Make sure that the proposal does not give `Submission` permission to a participant.
4. Make sure that the proposal sets a confirmation threshold that is a majority of the hosts.
5. Grant the watcher Ledger API user only `CanReadAs` for `guardianGovernance`.
6. Do not grant `CanActAs` or `CanActAsAnyParty` to the watcher user.
7. Vet each package on the allowlist (Section 6) before genesis.
8. Show the topology on your participant.
9. Make sure that each guardian participant is a host at `Confirmation`.

A change to the `gg` hosting mapping needs the `gg` threshold.
Your participant can remove its own hosting at any time.

LocalNet rehearsal: [`gg_host.sh`](../canton/localnet/gg_host.sh) hosts `gg` on three participants.
[`gg_show_topology.sh`](../canton/localnet/gg_show_topology.sh) shows `threshold=2` for the namespace.
For the hosting, it shows `confirmationThreshold=2`, three hosts at `Confirmation` and `signingKeys=2-of-3`.
The LocalNet script user has `CanActAsAnyParty` ([`create_script_user.sh`](../canton/localnet/create_script_user.sh)).
Do not use this user as an example for a production watcher.

The devnet setup script rejects a `GG_CONFIRMATION_THRESHOLD` that is not a majority of the hosts.
With one host, the threshold is 1.

**Open item:** the production commands for this procedure.

**Open item:** the confirmation threshold of `gg` for MainNet.
A high threshold stops `gg` transactions when few hosts are offline.
A low threshold lets fewer compromised participants confirm for `gg`.

## 5. `guardianGovernance` owner keys

### 5.1 Key custody

`gg` is a decentralized-namespace external party.
Each guardian holds one owner key.
A threshold of owner keys signs each `gg` topology change and each `gg` submission.

- Key algorithm: Ed25519.
- Canton imports the public key as an X.509 SubjectPublicKeyInfo DER key.
- The signer command is a JSON array of strings: the program, then its arguments.
- The ceremony script adds the hex hash as the last argument and runs the program directly.
- The signer writes the 64-byte Ed25519 signature to standard output as lowercase hex.
- The devnet scripts read the signer commands from `GG_OWNER_SIGN_ARGV_1`, `GG_OWNER_SIGN_ARGV_2` and `GG_OWNER_SIGN_ARGV_3`.
- [`canton/devnet/guardian_key_tool.js`](../canton/devnet/guardian_key_tool.js) shows this contract for local tests only.

1. Generate the owner key in your HSM, KMS or offline signer.
2. Export the public key in DER SubjectPublicKeyInfo format.
3. Send the public key to the genesis coordinator.
4. Keep the private key in the signer.
5. Do not export the private key.

**Open item:** the threshold k of n for MainNet.
LocalNet uses 2 of 3.

### 5.2 Ceremonies

A ceremony is a request to sign a prepared hash with the `gg` owner key.

| Ceremony | When | Procedure |
| - | - | - |
| Genesis co-signature of `CoreState` and `EmitterRegistry` | Once for each deployment | 7.2 |
| `NttGovernance` root creation | Once | 7.1 |
| Owner key rotation (add or remove a guardian) | On guardian set change | 7.4 |
| Fee withdrawal | On request | 7.6 |
| Live-signed NTT registration or factory change | On request | 7.1 |

A guardian set upgrade on Canton is not a `gg` signing ceremony.
The operator submits the governance VAA, and the `gg` hosts confirm it (Section 7.3).

### 5.3 Signer requirements

The LocalNet signer `interactive_submit.js` is not a production signer.
It signs the hash that the participant sends.
It does not calculate the hash from the prepared transaction.
Thus it trusts the participant.

A production signer calculates the hash from the prepared transaction.
It signs only if its hash is equal to the hash in the request.

**Open item:** the tool that decodes and hashes a prepared transaction for guardians.

## 6. Package vetting policy

### 6.0 Trust sources

The procedures of this document use the data of these sources.

| Source | Used in | Location | Writer | Authentication |
| - | - | - | - | - |
| Guardian package allowlist | 4.1, 6.1, 7.1 | **Open item** | **Open item** | **Open item** |
| Coordinator | 6.7, 7.1, 7.4 | **Open item** | **Open item** | **Open item** |
| Genesis coordinator | 5.1, 7.2 | **Open item** | **Open item** | **Open item** |
| Wormhole Canton deployment record | 4.1, 7.2 | **Open item** | **Open item** | **Open item** |
| Wormhole release notes | 6.5, 6.7 | **Open item** | **Open item** | **Open item** |

A `ContractUpgrade` governance VAA records the action. It does not vet a package.
The vetting decision of each guardian is the control.

**Open item:** whether a `ContractUpgrade` governance VAA must come before a core package upgrade.

### 6.1 Rules

To unvet a package is to remove the vetting of that package from your participant.

1. Vet only package IDs that are on the guardian package allowlist.
2. Do not use automatic vetting for packages that are not Splice packages.
3. Record each vetting change with the package ID, the date and the reason.
4. Do not vet or unvet a package in an LSU freeze window.

### 6.2 Package classes

| Class | Packages | Approval |
| - | - | - |
| Wormhole | `wormhole-core`, `ntt`, `token-cip0056` | Check the reproducible build. Then vet the exact package ID. |
| Splice | Amulet and other Splice DARs | Follow the DSO schedule. The super validators set the minimum DAR versions. |
| Splice token-standard interfaces | `splice-api-token-*` | Interface packages only. Vet the IDs that the Wormhole packages depend on. |
| Third-party token registry | The registry of a lock/unlock deployment that `gg` takes custody for | Blocked by decision U1 (Section 10). |

Splice Amulet code runs with the authority of `gg`.
The core bridge charges message fees in Amulet, and `gg` receives the fees.
Thus, when your participant vets an Amulet version, you trust that version with the authority of `gg`.

**Open item:** whether the Splice validator automation vets new Amulet versions automatically on guardian participants.
If it does, record this as an accepted trust in DSO governance.

### 6.3 Rehearsal result: every host must vet

[`gg_package_vetting.sh`](../canton/localnet/gg_package_vetting.sh) checks the vetting gate on three `gg` hosts.

| Hosts that vet version 2.0.0 | Result of a create on 2.0.0 |
| - | - |
| One of three | Rejected at prepare. The error names the two other hosts with `has not vetted`. |
| Two of three (equal to the confirmation threshold) | Rejected at prepare. The error names the third host with `has not vetted`. |
| Three of three | Committed. |

The error code is `INVALID_PRESCRIBED_SYNCHRONIZER_ID`.
With 2.0.0 vetted on one host, a create by package name selects version 1.0.0.
Thus each host must vet a package, not only the threshold number of hosts.
One guardian that does not vet a package stops all `gg` transactions on that package.
This rehearsal passed live on 2026-11-17, with the two-of-three case (Section 7.5).

### 6.4 New `wormhole-core` versions

Each new version of `wormhole-core` has a new package ID.
Example: the change from version 0.8.0 to 0.8.1 gives a new package ID.
The build makes sure that each new version is a valid Daml upgrade of the previous release.

The procedure of Section 6.7 applies only to an upgrade that keeps the signatories and observers of each template.
Version 0.8.0 adds `gg` as a signatory of `EmitterRegistry` and `Emitter`.
Thus a 0.7.0 `EmitterRegistry` or `Emitter` cannot run under 0.8.0.
Version 0.8.0 starts from a fresh genesis (Section 7.2).

The watcher accepts a `PublishMessage` only from the package ID in `--cantonPackageID`.
An `ExercisedEvent` carries the package ID of the package that executes the choice.
The package that created the `Emitter` has no effect on this match.
Thus a watcher on one package ID drops each message that a different package version executes.
A dropped message uses its emitter sequence number on the ledger.

Each informee participant must vet the package that executes the choice.
`gg` is an informee of each `PublishMessage`.
Thus a package that no guardian participant vets cannot execute `PublishMessage`.

A dependent package calls `wormhole-core` by package ID.
Example: `ntt` 0.3.2 calls `wormhole-core` 0.8.0.
Thus each dependent package needs a new build on the new core version.
The new build must be a valid upgrade of the old build.

Third-party packages can also call `wormhole-core` by package ID.
You cannot list these packages.
The counter `wormhole_canton_unpinned_publish_messages_total` shows their messages under the old package ID.

For the procedure, refer to Section 6.7.

### 6.5 Check a Wormhole package

1. Get the release commit and the expected package ID from the Wormhole release notes.
2. Build the DAR from the release commit with the pinned SDK version.
3. Read the package ID of your build.
4. Compare your package ID with the expected package ID.
5. Vet the package only if the two IDs are equal.

### 6.6 Vet a package

1. Upload the DAR to your participant.
2. Vet the package ID on the Global Synchronizer.
3. List the vetted packages of your participant.
4. Make sure that the list contains the new package ID.

For the Canton Console commands, refer to <https://docs.canton.network/global-synchronizer/reference/canton-console-commands.md>.

### 6.7 Core package upgrade

Use this procedure for each new `wormhole-core` version that keeps the signatories and observers of each template.
The sequence prevents a submitter from using the old version to bypass the checks of the new version.

NOTE: This procedure replaces the procedure in Section 7.5 of `canton/README.md`.
That section still has the earlier sequence, which removes the vetting of the old package as the last step.
Pull request #73 changes that section to agree with this procedure.
Pull request #73 also adds a scan tool and a LocalNet rehearsal for this procedure.
Until that merge, do the canary scan of Section 6.7.2 manually.
Use the sequence of this document, not the sequence of that section.

The coordinator announces the time of step 10 and the maximum canary time.
The maximum canary time starts at step 5.
It must be shorter than the pruning retention of each guardian participant.
Example: a maximum canary time of 72 hours.

Do these steps on your guardian:

1. Make sure that the procedure does not start in an LSU freeze window.
2. Record the ledger end offset of your participant. This is the start offset.
3. Get the new core package ID and the new dependent package IDs from the Wormhole release notes.
4. Check each package ID (Section 6.5).
5. Vet the new core package ID on your participant (Section 6.6).
6. Vet the new dependent package IDs on your participant.
7. Tell the coordinator that you vetted the new package IDs.
8. Do the canary scan (Section 6.7.2) until the coordinator announces the result of the canary gate.
9. If the canary gate does not pass before the maximum canary time, do the abort procedure (Section 6.7.4).
10. At the agreed time, unvet the old core package ID and each old dependent package ID in one topology transaction.
11. Make sure that your participant vets the new core package ID and does not vet the old core package ID.
12. Do the drain gate (Section 6.7.3). Keep `--cantonPackageID` set to the old core package ID until the gate passes.
13. Set `--cantonPackageID` to the new core package ID.
14. Restart `guardiand`.
15. Get the dropped update IDs (Section 6.7.1).
16. Send a reobservation request for each dropped update ID (Section 8.1).
17. Make sure that the watcher shows messages from the new package ID.

Each participant has different offsets. Use only the offsets of your participant.

Do not set `--cantonPackageID` to a package ID that your participant did not vet.
The reobservation steps of Section 6.7.4 are the only exceptions.
Do not do step 10 while a vetted package calls the old core package ID.
Do not do step 10 before each guardian confirms step 6.
Do not do step 10 in an LSU freeze window (Section 3.3).

After step 10, a submission under the old core package ID does not commit.
Thus no value moves without a message that the guardians can sign.

WARNING: From step 5 to step 14, your guardian does not sign messages under the new core package ID.
This includes the messages of each dependent package on the new core version.
These messages get VAAs only after step 16.
Make decision U3 (Section 10.1) before a MainNet upgrade uses this procedure.

Step 16 reads each update from your participant.
Participant pruning removes old updates, and then the reobservation request fails.
Make sure that the retention of your participant is longer than the time from step 2 to step 16.

**Open item:** contracts that the old version created, after the unvet of the old version.
Vetting can also apply to the package that created an input contract.
Before step 10, test this on a test network.
The contracts are `CoreState`, `EmitterRegistry`, `Emitter` and the replay contracts.

**Open item:** a LocalNet rehearsal of this procedure.

#### 6.7.1 Get the dropped update IDs

1. After step 14, do the canary scan (Section 6.7.2) from the start offset to the ledger end.
2. Keep each `PublishMessage` with the new core package ID as the executing package ID.
3. Compare the update IDs with the WARN logs of your watcher.
   The log text is `canton: dropped PublishMessage executed by a package other than the pinned package`.
   The `updateId` field of each log is a dropped update ID.
4. If the two lists are different, use the scan list. Tell the coordinator about the difference.
5. Scan from the gate offset of Section 6.7.3 to the ledger end.
   Make sure that no `PublishMessage` has the old core package ID.

After a stream error, the watcher continues from the offset of its last observation.
Thus it reads the drops after that offset again.
The WARN log and the counter `wormhole_canton_unpinned_publish_messages_total` then count such a drop two times.
Compare the sets of update IDs, not the counter values.

#### 6.7.2 Canary scan and canary gate

The canary scan is a read of the ledger with a Ledger API user that has no right to submit.

1. Make a Ledger API user with only the right `CanReadAs` for `guardianGovernance`.
2. Read the updates of `guardianGovernance` from the start offset with `UpdateService.GetUpdates`.
   Use the ledger effects transaction shape.
3. Keep each `ExercisedEvent` of the choice `PublishMessage` on the `Emitter` template.
4. Read the executing package ID from the `template_id` of each event.
5. Record the offset, the update ID and the executing package ID of each event.

WARNING: Do not start a second `guardiand` with your guardian key as a canary.
That node signs the messages of a package that the quorum did not agree to.
It also uses the gossip identity of your main node.

The coordinator can also run a canary watcher pinned to the new core package ID with a key that is not a guardian key.
That watcher counts each publish under another package in `wormhole_canton_unpinned_publish_messages_total`.
Use the sum over all labels, `other` included.

The canary gate passes only when each condition is true:

1. Quiet window: for the agreed time, no new publish under a package other than the new core package ID occurs.
   A publish under another package starts the quiet window again.
2. Positive control: after the quiet window, the coordinator publishes a control message under the new core package ID.
   The canary scan must find it.
3. A VAA exists for each publish under the old core package ID since the start offset.

The gate passes at the first complete quiet window with the positive control.
The gate fails if the maximum canary time ends first.

#### 6.7.3 Drain gate

A request with a sequencing time before the unvet can commit after the unvet.
Your watcher, still pinned to the old core package ID, must observe it before the restart.

1. After step 11, record the ledger end offset of your participant. This is the unvet offset.
2. Read the dynamic synchronizer parameters.
3. Add `confirmation_response_timeout`, `mediator_reaction_timeout` and `ledger_time_record_time_tolerance`.
4. Add 30 seconds to the sum. This is the drain time.
5. Wait for the drain time.
6. Wait for the control message of the coordinator under the new core package ID.
7. Find the WARN log for the drop of the control message. Thus your watcher read each update before it.
8. Record the offset of the control message on your participant. This is the gate offset.
9. Scan from the start offset to the gate offset. Keep each `PublishMessage` with the old core package ID.
10. Make sure that your guardian observed each of these messages.
11. Tell the coordinator the result.

#### 6.7.4 Failure branches

**The canary gate does not pass before the maximum canary time.** Do the abort procedure:

1. In one topology transaction, unvet the new core package ID and each new dependent package ID.
   Keep the old core package ID and the old dependent package IDs vetted.
2. Make sure that your participant vets the old core package ID and does not vet the new core package ID.
3. Wait for the drain time (Section 6.7.3, step 4).
4. Wait for the control message of the coordinator under the old core package ID.
5. Make sure that your watcher observes the control message.
6. Record the offset of the control message on your participant. This is the abort offset.
7. Scan from the start offset to the abort offset. Keep each `PublishMessage` with the new core package ID.
8. With a quorum of guardians, set `--cantonPackageID` to the new core package ID and restart `guardiand`.
9. Send a reobservation request for each update ID of step 7.
10. When each VAA exists, set `--cantonPackageID` to the old core package ID and restart `guardiand`.
11. Scan from the restart of step 8 to the ledger end.
12. Send a reobservation request for each `PublishMessage` with the old core package ID.

A reobservation reads a committed update. Thus step 8 needs no vetting of the new core package ID.
After step 1, a submission under the new core package ID fails at prepare.
The coordinator tells the integrators that the upgrade stopped.

**Your guardian is late or fails after step 10.** A new message gets a VAA only when a quorum observes it.
After each restart of a guardian, send the reobservation requests of step 16 again.

**A message under the old core package ID has no VAA after step 14:**

1. A quorum of guardians sets `--cantonPackageID` to the old core package ID again and restarts.
   No vetting change is necessary. A reobservation reads a committed update.
2. Send a reobservation request for each of these update IDs.
3. When each VAA exists, set `--cantonPackageID` to the new core package ID again and restart.
4. Send a reobservation request for each new message of this period.

**An LSU freeze window starts during step 10.** When one participant completes step 10, each transaction under the old core package ID stops.
Start the drain gate at the first unvet. Complete the unvet after the upgrade time.

**An informee participant does not vet the new core package ID before step 10.**
Its `gg` transactions fail after step 10, and its contracts stay frozen until it vets.
The coordinator tells each integrator about this risk before step 10.

## 7. Ceremony procedures

### 7.1 Sign a `gg` transaction

Use this procedure for each `gg` signing ceremony.

1. Make sure that the time is not in an LSU freeze window.
2. Get the prepared transaction and its hash from the coordinator.
3. Decode the prepared transaction.
4. Make sure that each package ID in the transaction is on the allowlist (Section 6).
5. Make sure that the transaction does the action that the request states.
6. Calculate the hash yourself.
7. Compare your hash with the hash in the request.
8. Sign the hash only if the two hashes are equal.
9. Send the signature to the coordinator.

### 7.2 Genesis co-signature

Each deployment of `wormhole-core` 0.8.0 starts from a fresh genesis with a new operator party.
Contracts of an earlier version do not move to 0.8.0.
For the reason, refer to [`A14_EMITTER_ID_UNIQUENESS.md`](../canton/A14_EMITTER_ID_UNIQUENESS.md).

The genesis ceremony has two prepared transactions.
The first transaction creates the `CoreState`. The second transaction creates the `EmitterRegistry`.
The `operator` and the `gg` owner key threshold sign each of the two transactions.
The prepare endpoint accepts one command in each transaction.
The operator creates the `ReplayRootRegistry` alone.

Before the ceremony:

1. Make sure that each guardian participant hosts `gg` (Section 4.1).
2. Make sure that each guardian participant vetted the agreed `wormhole-core` package ID.
3. Make sure that your participant vets no other `wormhole-core` package.

Check the `CoreState` transaction:

1. Get the `CoreState` arguments from the genesis coordinator.
2. Make sure that the arguments contain the agreed guardian set.
3. Make sure that the arguments contain the agreed governance chain and governance contract.
4. Make sure that the arguments set `chainId` to exactly 75.
5. Make sure that the arguments set `feeRecipient` to the `guardianGovernance` party.
6. Make sure that `feeInstrument` is the agreed instrument admin and instrument ID.
7. Make sure that `operator` is the agreed new operator party.
8. Make sure that `guardianSetIndex` is 0.
9. Make sure that `messageFee` is the agreed value.
10. Make sure that `consumedGovernance` is empty.
11. Make sure that the template package ID is the agreed `wormhole-core` package ID.
12. Sign the prepared hash (Section 7.1).

Check the `EmitterRegistry` transaction:

1. Make sure that your participant shows exactly one `CoreState` before you sign this transaction.
2. Make sure that `operator` and `guardianGovernance` are the parties of the `CoreState`.
3. Make sure that `nextId` is 0.
4. Make sure that `registrationFee` is the agreed value.
5. Make sure that the template package ID is the agreed `wormhole-core` package ID.
6. Sign the prepared hash (Section 7.1).

After genesis:

1. Make sure that your participant shows exactly one `CoreState`.
2. Make sure that your participant shows exactly one `EmitterRegistry` with `nextId` 0 and your `gg` party.
3. Make sure that your participant shows exactly one `ReplayRootRegistry`.
4. Run [`emitter_id_scan.sh`](../canton/tools/emitter_id_scan.sh) with begin offset 0 and `EXPECT_ACTIVE_REGISTRIES=1`.
   Set `EXPECTED_CORE_PACKAGE_ID` to the agreed `wormhole-core` package ID.
   Use a Ledger API user that has only the right `CanReadAs` for `guardianGovernance`.
5. Make sure that the scan passes with `firstId` 0.

If the `CoreState` transaction fails, stop. Do not sign the `EmitterRegistry` transaction.
Do the `CoreState` ceremony again.

If the `EmitterRegistry` transaction fails, the `CoreState` exists without a registry.
Do the `EmitterRegistry` ceremony again. Do not create a second `CoreState`.

An emitter ID is an integer from 0 to 2^53 - 1 (`maxEmitterId`).

LocalNet rehearsal: [`gg_genesis.sh`](../canton/localnet/gg_genesis.sh).
The `operator` and two of three owner keys sign each of the two transactions.
A `CoreState` with one of three owner keys fails with `FAILED_TO_EXECUTE_TRANSACTION`.
After a failure, the script creates only the missing contracts.

### 7.3 Guardian set upgrade on Canton

The `GuardianSetUpgrade` governance VAA changes the guardian set in `CoreState`.
The operator submits it with `SubmitGovernanceVAA`.
The current guardian set must sign the VAA.
The `gg` hosts confirm the transaction. The owner keys do not sign it.

The old guardian set stays valid until its expiry time.
The expiry time is the ledger time of the upgrade plus 86400 seconds.
The expiry check compares whole seconds: the old set is valid while `now <= expiry`.
Thus the old set is valid for all of the expiry second, as on EVM.
The old set is not valid one second after the expiry second.
A governance VAA must have the signatures of the current set.

1. Sign the `GuardianSetUpgrade` VAA through the usual guardian procedure.
2. Keep your participant online until the operator submits the VAA.
3. Make sure that `CoreState` shows the new guardian set index.
4. Make sure that `CoreState` shows the expiry time of the old set.

LocalNet rehearsal: [`gg_guardian_set_rotation.sh`](../canton/localnet/gg_guardian_set_rotation.sh).
The rehearsal checks these results:

- A VAA of the new set passes `ParseAndVerifyVAA`.
- A VAA of the old set passes until the expiry time.
- A governance VAA of the old set fails with `governance: not signed by current guardian set`.
- A VAA of the new set with an old key fails with `public key does not match guardian address`.
- With a set of three guardians, a VAA with two signatures fails with `no quorum: have 2, need 3`.

The rotation from a one-guardian set to a three-guardian set passed live on 2026-11-17.
All five results above passed in that run.

LocalNet uses the clock time, thus it does not reach the expiry time.
The Daml Script test `Test.TestCore:testGuardianSetUpgradeExpiry` checks the expiry boundary.

### 7.4 Owner key rotation

An owner key rotation removes one owner key and adds a new owner key.
The namespace threshold does not change.

The owner keys are the namespace owners and the signing keys of each party in the namespace.
Thus the rotation changes the decentralized namespace and the `PartyToParticipant` mapping of each party.
If the namespace also has the `nttCustody` party of an NTT custody deployment, the rotation has three topology changes.

The rotation has two phases:

1. Sign phase: each owner signs each topology change. No change goes to the synchronizer.
2. Load phase: one load call submits all the changes. The `nttCustody` change is first, and the namespace change is last.

If the new owner has no root namespace delegation, the load phase loads it first.
This delegation alone gives no authority over the `gg` namespace.
A signer failure in the sign phase stops the rotation before the first load.
The topology then does not change.

NOTE: The sign phase, the one load call and the stored signatures come with pull request #76.
The LocalNet script of this branch does not have them.
That script signs and loads the namespace change first. Then it signs and loads the signing key change.
Thus, with that script, a signer failure after the namespace change gives a half-applied rotation.
With that script, `--resume` requests new signatures from the owners.

Procedure:

1. Make sure that the time is not in an LSU freeze window.
2. Get the public key of the new owner in DER SubjectPublicKeyInfo format.
3. Sign each topology change of the rotation with your owner key.
4. After the load, make sure that the namespace contains the new owner key.
5. Make sure that the namespace does not contain the removed owner key.
6. For each party in the namespace, make sure that the signing keys contain the new owner key.
7. For each party in the namespace, make sure that the signing keys do not contain the removed owner key.
8. Make sure that the namespace threshold and the signing key threshold of each party did not change.
9. Make sure that all the changes have the same effective time.

After the rotation, a signature from the removed key is not valid.
A `gg` transaction with the removed key fails with `FAILED_TO_EXECUTE_TRANSACTION`.
A namespace change that the removed key signs fails with `TOPOLOGY_UNAUTHORIZED_TRANSACTION`.

WARNING: Canton validates each topology change of a load call alone.
Thus a load can apply only a part of the changes. The rotation is then half-applied.
In that state, the signing keys of a party can still contain the removed owner key.
The removed key then still counts for the signing threshold of that party.
Complete the rotation with `--resume` immediately.
Do not start a different ceremony before the rotation is complete.

**Open item:** the measured behavior of a partial load on LocalNet.

#### Recovery from a half-applied rotation

The rotation script shows `ROTATION HALF-APPLIED`.
From pull request #76, it also shows the parties that still accept the removed key.

1. Do not generate a new key for the new owner.
2. Run the rotation again with `--resume`.
   From pull request #76, `--resume` loads the changes that the owners signed in the sign phase.
3. Do steps 4 to 9 of the procedure again.

LocalNet rehearsal: [`gg_rotate_owner_key.sh`](../canton/localnet/gg_rotate_owner_key.sh).
The LocalNet script loads the namespace change and the signing key change in two load calls.
For a half-applied rotation, the script shows `ROTATION HALF-APPLIED`.
The `--resume` flag finishes the rotation with the existing key of the new owner.
On 2026-11-17, a live run with an injected half-applied rotation passed.
The `--resume` run then finished the rotation and passed all checks.

### 7.5 Rehearsal evidence

Each LocalNet `gg_*.sh` script writes an evidence log.
The log is in `localnet-state/evidence/` next to the repository.
The first line of the log shows the arguments and the Git commit.
The evidence logs are on the rehearsal host. They are not in the repository.

These rehearsals passed live on 2026-11-17 with `wormhole-core` 0.6.0:

| Rehearsal | Result | Commit | Evidence log |
| - | - | - | - |
| Confirmation threshold (Section 2.3) | Passed | `edf996e50` | `gg_confirmation_quorum-20261117T031200Z.log` |
| Vetting gate, first run (Section 6.3) | Passed | `d09dd82ec` | `gg_package_vetting-20261117T031441Z.log` |
| Vetting gate, second run (Section 6.3) | Passed | `3b7f20ca4` | `gg_package_vetting-20261117T031454Z.log` |
| Guardian set rotation to three guardians, with the below-quorum rejection (Section 7.3) | Passed | `3b7f20ca4` | `gg_guardian_set_rotation-20261117T031504Z.log` |
| Owner key rotation with recovery (Section 7.4) | Passed after `--resume` | `dd01beeb0` | `gg_rotate_owner_key-20261117T031708Z.log`, `gg_rotate_owner_key-20261117T031721Z.log` |

**Open item:** run these rehearsals again with `wormhole-core` 0.8.0.

Limits of the rehearsal:

- One container runs all three participants and the synchronizer.
- The owner keys are files on the host, not keys in guardian custody.
- The checks disconnect participants. They do not stop the participant processes.

### 7.6 Fee withdrawal

A `TransferFees` governance VAA creates a `FeeWithdrawalAuthorization`.
`gg` is its only signatory, and the `operator` is an observer.
The contract has no choice. Thus only a transaction that `gg` signs can archive it.
The fees are holdings of `gg`. Thus the transfer of the fees is a `gg` signing ceremony.
The `amount` field is in units of 10^-10 of the fee instrument.

1. Make sure that an active `FeeWithdrawalAuthorization` exists with the same amount.
2. Make sure that the Canton receiver is the agreed party for the 32-byte `recipient` of the VAA.
3. Make sure that the transaction archives the `FeeWithdrawalAuthorization`.
4. Sign the prepared hash (Section 7.1).

**Open item:** the mapping from the 32-byte `recipient` to a Canton party.

### 7.7 Key compromise and guardian removal

**Owner key compromise.** Do the owner key rotation (Section 7.4) immediately to remove the compromised key.

**Owner key loss.** If fewer than the threshold number of owner keys stay, no `gg` transaction or topology change is possible.
[`gg_show_topology.sh`](../canton/localnet/gg_show_topology.sh) shows the namespace threshold.

**Participant key, OAuth2 client secret or `operator` party compromise.**
**Open item:** the procedure for each of these cases.

**Guardian removal:**

1. Remove the owner key of the guardian with an owner key rotation (Section 7.4).
2. Remove the `gg` hosting from the participant of the guardian.
   That participant can remove its own hosting. Other changes to the hosting need the `gg` threshold.
3. Make sure that the confirmation threshold is a majority of the hosts that stay.
4. Remove the Ledger API user of the watcher of the guardian.

## 8. Watcher configuration

The Canton watcher in `guardiand` reads the Ledger API of your participant.

| Flag | Value |
| - | - |
| `--cantonRPC` | gRPC endpoint of the Ledger API v2 of your participant. |
| `--cantonPackageID` | Package ID of `wormhole-core`. Mandatory outside unsafe dev mode. Change it on each new core version (Section 6.7). |
| `--cantonReadAsParty` | The `guardianGovernance` party ID. Mandatory outside unsafe dev mode. |
| `--cantonAuthTokenURL` | OAuth2 token endpoint for the Ledger API. |
| `--cantonAuthClientID` | OAuth2 client ID (client-credentials grant). |
| `--cantonAuthClientSecret` | OAuth2 client secret. Use the `GUARDIAND_CANTONAUTHCLIENTSECRET` environment variable. |

Outside unsafe dev mode, the watcher does not start without `--cantonPackageID` and `--cantonReadAsParty`.
Set the three OAuth2 flags together, or set none of them.
OAuth2 needs TLS, thus unsafe dev mode cannot use it.

An empty package ID matches each package that declares the `Emitter` template.
Thus a look-alike package can make forged messages.
An empty party ID reads the updates of each party on the participant.

The watcher drops a `PublishMessage` on an `Emitter` of which `gg` is not a stakeholder.
The counter `wormhole_canton_publish_without_read_party_stakeholder_total` counts these drops.
Each drop also writes a WARN log with the update ID.

The same transaction can create an `Emitter` and publish on it.
The watcher accepts such a publish only if the pinned package created the `Emitter` and `gg` signs it.
In unsafe dev mode with an empty package ID or party ID, the watcher drops each such publish.

The watcher uses `UpdateService.GetUpdates`.
The watcher keeps the offset of the last message in memory.
After a stream error in the same `guardiand` process, the watcher continues from that offset.
If no message arrived before the stream error, the watcher starts at the ledger end.
After a restart of `guardiand`, the watcher starts at the ledger end.
Thus a restart skips each Canton message of the restart period.
The participant offset is local to your participant. It does not leave the guardian.

### 8.1 Transaction ID and reobservation

The `TxID` of a Canton message is the hex-decoded Canton update ID.
It has exactly 34 bytes, and the first two bytes are `0x12 0x20`.
The update ID is the same on each participant that hosts the transaction.
Thus all guardians report the same `TxID` for one message.

To request a reobservation, give the update ID to `guardiand`:

```
guardiand admin send-observation-request --socket <admin socket path> 75 <update id>
```

The watcher gets the update with `UpdateService.GetUpdateById`.
It applies the same party, template and `--cantonPackageID` filters as the stream.

Participant pruning removes old updates. A reobservation of a pruned update fails.
Keep updates longer than the agreed reobservation period.

**Open item:** the reobservation period.

Earlier `guardiand` versions used a 32-byte participant offset as the `TxID`.
A guardian on one encoding rejects the reobservation requests of a guardian on the other encoding.

WARNING: Upgrade all guardians that observe Canton together, because the old and new `TxID` formats reject each other's reobservation requests.

### 8.2 Failure behavior

If the participant stops, the watcher does not see new messages.
The guardian then does not sign Canton messages.
This failure does not cause an incorrect signature.

If `guardiand` stops or restarts, the watcher does not read the messages of that period later.
A message gets a VAA only when a quorum of guardians observes it.
Use the procedure in Section 8.4 after each restart.

### 8.3 Monitoring

1. Monitor the health of the participant.
2. Monitor the traffic balance of the participant.
3. Monitor the connection of the participant to the synchronizer.
4. Monitor the watcher for errors.
5. Monitor the watcher metrics in the table below.

| Metric | Alert condition |
| - | - |
| `wormhole_canton_current_height` | The value does not increase for 5 minutes. |
| `wormhole_canton_observations_confirmed_total` | The value does not increase while the ledger has new `PublishMessage` updates. |
| `wormhole_canton_unpinned_publish_messages_total` | The value increases. |
| `wormhole_canton_publish_without_read_party_stakeholder_total` | The value increases. |
| Traffic balance of the participant | **Open item** |
| Number of `gg` hosts online | **Open item** |

`wormhole_canton_current_height` is the ledger end of your participant.
The watcher reads it at the start and each 5 seconds.
It is not the offset of the last message that the watcher processed.

### 8.4 Find the messages of a restart period

1. Find the last value of `wormhole_canton_current_height` before the stop. This is the start offset.
2. Use a value from 60 seconds before the stop, because the watcher reads the ledger end each 5 seconds.
3. Read the ledger end after the restart. This is the end offset.
4. Read the updates of `guardianGovernance` between the two offsets with the Ledger API.
5. Keep each update that has a `PublishMessage` exercise of the pinned package ID.
6. Send a reobservation request for each update ID (Section 8.1).

A second reobservation of one message has no bad effect.

## 9. Cross-chain limits

### 9.1 Rate limits

Canton has no accountant coverage. Peer chains must set NTT inbound rate limits for Canton peers as the cross-chain bound.
The Governor has no Canton configuration (`node/pkg/governor`).

**Open item:** who checks the inbound rate limits of each peer chain.

### 9.2 Custody pot duty

**F4: expiry of idle Canton Coin custody pots.**
This duty applies to each custodian of a lock/unlock deployment of Canton Coin.
After the quorum accepts custody, `gg` is the custodian.
The DSO can archive an idle custody pot as dust when its holding fee reaches its amount.
After that, the custody reserve is less than the ledger balance, and `Release` fails.
Each lock and each release makes a new pot. Thus only an idle pot ages.

1. Monitor the expiry round of each custody pot.
2. Before that round, run `ConsolidateCustody` with the pot and one more custodian holding.
3. Make sure that a new pot exists from the current round.

A call with only the pot does not refresh the pot.
The NTT red-team finding is F4 in `canton/REDTEAM.md` of the NTT repository.

## 10. Open items and decisions

### 10.1 Decisions

**U1: `gg` custody of third-party token registries.**
NTT lets the guardian quorum accept custody of a lock/unlock deployment.
After that, `gg` is the sender in transfers on the third-party registry.
Thus the registry code runs with the authority of `gg`.
The NTT red-team finding F1 shows that such a registry can mint other `gg` tokens.
It can also move other holdings that `gg` owns, for example message fees.
The proposed fixes are:

- A dedicated custody party, separate from the `gg` mint authority.
- A separate mint authority party, or a `CoinFactory` that the operator also signs.
- An allowlist of registry templates for quorum custody.

Until a decision, do not vet a third-party registry package for `gg` custody.

**U3: a set of package IDs in the watcher for the upgrade period.**
The core package upgrade (Section 6.7) has a signing outage from step 5 to step 14.
The proposal lets the watcher accept a set of package IDs.
Each guardian adds the new package ID before step 5.
Each guardian removes the old package ID after step 10.
Then no coordinated restart is necessary, and messages under the new package ID get VAAs without reobservation.
The proposal (pull request #74) is not merged.
Until a decision, use the procedure in Section 6.7.

NOTE: Pull request #74 adds an optional retiring package ID to the watcher.
The flags are `--cantonRetiringPackageID`, `--cantonRetiringPackageVersion` and `--cantonRetiringPackageAcceptedUntil`.
With these flags, the watcher accepts two package IDs for a limited period.
Section 7.5 of `canton/README.md` gives the procedure with two package IDs.
If you do not set the retiring flags, the procedure in Section 6.7 applies with no change.

### 10.2 Open items

| Item | Section |
| - | - |
| Sponsor super validator for each guardian | 3.1 |
| KMS provider for guardian participants | 3.4 |
| Production commands for party hosting | 4.1 |
| Confirmation threshold of `gg` for MainNet | 4.1 |
| Threshold k of n of the owner keys for MainNet | 5.1 |
| Production tool to decode and hash prepared transactions | 5.3 |
| Location, writer and authentication of each trust source | 6.0 |
| `ContractUpgrade` governance VAA before a core package upgrade | 6.0 |
| Automatic vetting of Amulet on guardian participants | 6.2 |
| Contracts of the old core version after the unvet | 6.7 |
| LocalNet rehearsal of the core package upgrade | 6.7 |
| Measured behavior of a partial load in the owner key rotation | 7.4 |
| Rehearsals with `wormhole-core` 0.8.0 | 7.5 |
| Mapping from the 32-byte `recipient` to a Canton party | 7.6 |
| Procedure for a participant key, OAuth2 client secret or `operator` party compromise | 7.7 |
| Reobservation period and participant pruning | 8.1 |
| Alert values for the traffic balance and the number of `gg` hosts | 8.3 |
| Persistent offset for the watcher | 8.4 |
| Check of the inbound rate limits of each peer chain | 9.1 |
| F4: expiry of idle Canton Coin custody pots | 9.2 |
| U1: `gg` custody of third-party token registries | 10.1 |
| U3: a set of package IDs in the watcher for the upgrade period | 10.1 |

### 10.3 Resolved items

| Item | Resolution |
| - | - |
| Multi-participant hosting of `gg` | Passed live on 2026-11-17 with `wormhole-core` 0.6.0 on three LocalNet participants at `Confirmation` with threshold 2 (Section 2.3). |
| Vetting on a subset of hosts | Rejected at prepare. Each host must vet. Passed live on 2026-11-17 with `wormhole-core` 0.6.0 (Section 6.3). |
| Mandatory `gg` party in the watcher | `--cantonReadAsParty` is mandatory outside unsafe dev mode (Section 8). |
| Owner key rotation | Passed live on 2026-11-17 with `wormhole-core` 0.6.0, with recovery from a half-applied rotation (Section 7.4). |
| Guardian set rotation to three guardians | Passed live on 2026-11-17 with `wormhole-core` 0.6.0, with the below-quorum rejection (Section 7.3). |
| Guardian set expiry boundary | The check compares whole seconds, `now <= expiry`, as on EVM (Section 7.3). |
| `TxID` on different participants | The `TxID` is the update ID, the same on each participant (Section 8.1). |
| U2: emitter address uniqueness against a compromised operator | `gg` co-signs `EmitterRegistry` and `Emitter` from `wormhole-core` 0.8.0 (Section 7.2). |
| Confirmation threshold of a multi-host `gg` | The devnet setup script requires a majority of the hosts (Section 4.1). |
