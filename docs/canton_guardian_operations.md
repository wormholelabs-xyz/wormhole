# Canton Guardian Operations

This document gives the operation responsibilities of a Wormhole guardian on the Canton Network.
It is a draft. Items with the label **Open item**, or with an ID U1 to U5, need a decision before MainNet.

For the design of the Canton core bridge and watcher, refer to [`canton/README.md`](../canton/README.md).
For the LocalNet rehearsal of each ceremony, refer to [`canton/localnet/README.md`](../canton/localnet/README.md).
For general guardian operation, refer to [`operations.md`](operations.md).

## 1. Summary of responsibilities

| Responsibility | Section | Frequency |
| - | - | - |
| Operate a Canton participant node on the Global Synchronizer | 3 | Continuous |
| Keep the participant on the minimum Splice version | 3.2 | Weekly releases |
| Host the `guardianGovernance` party | 4 | Once, then on changes |
| Host the `feeCustody` party | 4.2 | Once, then on changes |
| Accept the genesis offer once | 7.2 | Once for each deployment |
| Keep the Canton participant without public ports | 3.5 | Continuous |
| Follow the snapshot rules when a host joins | 4.3, 4.4 | On each new host |
| Hold one `guardianGovernance` owner key | 5 | Continuous |
| Sign `guardianGovernance` ceremonies | 5.2, 7 | On request |
| Apply the package vetting policy | 6 | On each new package version |
| Do the core package upgrade procedure | 6.7 | On each new `wormhole-core` version |
| Check fee withdrawals | 7.6 | On request |
| Respond to a key compromise or a guardian removal | 7.7 | On event |
| Replace the onboarding fee admin | 7.9 | Emergency only |
| Serve disclosures of `gg` contracts to submitters | 7.10 | On request |
| Configure and monitor the Canton watcher in `guardiand` | 8 | Continuous |
| Send reobservation requests after each restart of `guardiand` | 8.4 | On each restart |
| Upgrade all guardians that observe Canton together | 8.1 | On each `TxID` format change |
| Consolidate idle Canton Coin custody pots (custodians) | 9.2 | Before the expiry round of each pot |

## 2. Parties

Two guardian parties exist from genesis: `guardianGovernance` (`gg`) and `feeCustody`.
Each guardian participant hosts the two parties. `feeCustody` is in the `gg` namespace.
The proposed NTT custody deployment (decision U1) adds the party `nttCustody` in the `gg` namespace.
Each custody party copies the hosts, the confirmation threshold and the owner keys of `gg` (Section 7.4).
This rule applies at all times, also after a hosting change or a key change (decision U6).
`feeCustody` signs `CoreState`. Thus its hosts confirm each `PublishMessage`, each VAA check and each governance transaction.
With the hosts and the threshold of `gg`, `feeCustody` adds no liveness dependency.
The last step of each hosting procedure and each key procedure is the custody hosting comparison:

1. Run [`gg_show_topology.sh`](../canton/localnet/gg_show_topology.sh) or its production equivalent.
2. Find the rows of `gg`, `feeCustody` and `nttCustody`.
3. Make sure that the three rows show the same hosts and host permissions.
4. Make sure that the three rows show the same confirmation threshold and signing keys.

| Property | Value |
| - | - |
| Hosting permission | `Confirmation` on every guardian participant. Never `Submission`. |
| Confirmation threshold | Greater than 1, and a majority of the hosts. One participant cannot confirm for `gg` alone. |
| Owner keys | One Ed25519 key for each guardian. A threshold of owner keys controls the `gg` namespace. |
| Signatory of | `CoreState`, `EmitterRegistry`, `ReplayRootRegistry`, every `Emitter`, every `ReplayNode`, `NttGovernance`, every NTT manager and each `FeeWithdrawalAuthorization`. |
| Messages | Each `PublishMessage` runs on an `Emitter` that `gg` signs. Thus guardians see each message. |
| Other roles | Mint authority for Wormhole-administered tokens. |
| Watcher | `guardiand` reads the update stream as `gg` (`--cantonReadAsParty`). |

| `feeCustody` property | Value |
| - | - |
| Hosting and confirmation threshold | The same as `gg` at all times (decision U6). |
| Owner keys and signing key threshold | The same as `gg` (decision U5). |
| Signatory of | `CoreState` only. |
| Role | Fee recipient of `CoreState`. It owns the collected fees. |
| Holdings | Only holdings of the fee instrument. |

The fee registry code runs on each paid charge with the authority of the payer and `feeCustody`.
It also has the authority of the fee instrument admin, which signs each fee allocation.
`CoreState` rejects `gg` as the fee recipient.
`CoreState` also rejects `gg` and `feeCustody` as the fee instrument admin.
A charge with a fee above 0 rejects `gg` as the payer.
Thus fee registry code never runs with the authority of `gg`.

Two residuals stay. A red-team test on `wormhole-core` 0.8.3 confirmed each of them:

- Fee registry code can publish on an `Emitter` that the payer owns. The guardians sign that message.
- Fee registry code can register an `Emitter` that `feeCustody` owns. It can then publish on that `Emitter`.

Rules for these residuals:

- Use a fee payer that owns no `Emitter`. Tell each integrator to obey this rule.
- No contract and no peer chain gives a meaning to a signature of `feeCustody`.
- No contract and no peer chain gives a meaning to a message from an `Emitter` that `feeCustody` owns.
- `feeCustody` owns no `Emitter`. `feeCustody` is not the admin of an NTT deployment.
- `feeCustody` is not `nttCustody`. If it is, Amulet code runs as the owner of all NTT reserves.

The operator is not a guardian party. Wormhole Labs operates it.
The operator does the deployment work: it uploads the DARs, allocates its party and offers genesis (Section 7.2).
After genesis, the operator is a signatory of no contract and an observer of no contract.
The operator is the onboarding fee admin of `EmitterRegistry` and `ReplayRootRegistry`.
This is a plain field, not a signatory. It controls only `SetRegistrationFee` and `SetClaimFee`.
If the operator participant is offline or its key is lost, only these two fee changes stop.
Publish, observation, verification, replay protection, registration, claims, governance and NTT continue.

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
The error is `MEDIATOR_SAYS_TX_TIMED_OUT`. On 0.6.0 it named `gg` as the only unresponsive party.
On 0.8.3 it names `gg` and `feeCustody`, the `CoreState` co-signers.
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

### 3.5 Network exposure

The validator connects out on port 443 to the super validators.
Each super validator allowlists the egress IP of the validator.
The participant and the validator app need no inbound port from the internet.

1. Do not expose a port of the Canton participant or the validator app to the internet.
2. Bind the Ledger API, the JSON Ledger API and the Admin API to a private interface.
3. Admit only the validator app, the watcher and your gateway to the Ledger API ports.
4. Admit only operator hosts to the Admin API. It serves topology, repair and ACS import commands.

`guardiand` still needs its P2P ports. Refer to [`operations.md`](operations.md).

Users and relayers do not connect to guardian participants.
They submit through their own validator, with disclosed contracts.
A guardian that serves contract disclosure or the CIP-0056 registry API does so from a separate gateway.
The gateway has its own public IP and reaches the JSON Ledger API through a private link or mutual TLS.
Its Ledger API user has only `CanReadAs`.
For the gateway, refer to `canton/disclosure/README.md` in the native-token-transfers repository.

## 4. Party hosting

Host the party on your participant before the genesis of the core bridge.
A participant that starts to host the party later does not receive prior contracts.
For a late start, Canton party replication is necessary (Section 4.3).

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

### 4.2 Host `feeCustody`

Do steps 2 to 4 and 8 to 9 of Section 4.1 for the `feeCustody` party before genesis.
The `feeCustody` mapping must have the hosts, the confirmation threshold and the signing keys of `gg`.
Do not grant the watcher user rights for `feeCustody`.

LocalNet rehearsal: `CUSTODY_PARTY_NAME=feeCustody` [`custody_host.sh`](../canton/localnet/custody_host.sh).

### 4.3 Add a host after genesis

A new guardian participant must host `gg`, `feeCustody` and `nttCustody` after these parties have contracts.
Thus the new host needs Canton offline party replication.
For the steps, refer to the Canton party management documentation at
<https://docs.canton.network/global-synchronizer/production-operations/party-management.md>.

The replication has 13 steps in a fixed order. In summary:

1. The new participant vets each package on the allowlist (Section 6).
2. The source host stops automatic pruning until the export is complete.
3. The new participant authorizes the hosting with the onboarding flag.
4. The new participant disconnects from all synchronizers.
5. The new participant sets `manualConnect = true`, so that a restart does not connect it before the import.
6. A threshold of owner keys signs the `PartyToParticipant` change with the onboarding flag.
   The change adds the new host at `Observation` (Section 4.4, rule 3).
7. The source host exports the active contract set (ACS) with `export_party_acs`.
8. The new participant makes a backup of its database.
9. The new participant imports the ACS with
   `import_party_acs(..., party = Some(<party>), contractImportMode = ContractImportMode.Validation)`.
10. The new participant connects again.
11. Poll `clear_party_onboarding_flag` until it returns `FlagNotSet`.
12. Do the custody hosting comparison (Section 2).

Do these steps for `gg`, `feeCustody` and `nttCustody` together.

The export contains the ACS at the offset where the new hosting becomes active.
Each honest existing host exports the same contract set for one new host.

### 4.4 Trust in the ACS snapshot

The new host trusts the host that gives it the snapshot.
The import checks that the contents of each contract agree with its contract ID.
The contract ID is a hash, not a signature.
Thus the import cannot find an invented contract, a removed contract or an archived contract.

A bad snapshot has these effects:

- A removed contract makes the new host reject valid transactions that use it.
  When the mediator approves such a transaction, the new host stops with the fatal error below.
- An invented contract makes the new host approve transactions that use it.
  The honest hosts reject them with `LOCAL_VERDICT_INACTIVE_CONTRACTS`.
- If the bad hosts and the deceived hosts reach the confirmation threshold, the mediator approves the transaction.
- Each honest host then logs the alarm "Mediator approved a request that has been locally rejected" (`SYNC_SERVICE_ALARM`).
- Then it stops with "with failed activeness check is approved" and disconnects from the synchronizer.
  The participant setting `crashAfterFailedValidation` causes this stop. It is `true` by default.
- ACS commitments between participants show `ACS_COMMITMENT_MISMATCH` in a later commitment interval.

Do not set `crashAfterFailedValidation = false`.

LocalNet rehearsal on 2026-11-17 with three hosts and a confirmation threshold of 2:

- An existing host gave a new host a snapshot with an invented `Coin` of 1,000,000 tokens.
- The import accepted the `Coin` with `ContractImportMode.Validation`.
- The bad host and the new host approved an NTT `Transfer` of the `Coin`. The mediator approved it.
- The honest host rejected the transaction. Then it stopped with the fatal error and restarted in a loop.
  LocalNet runs all these nodes in one process.
- An import of the invented `Coin` on the honest host stopped the loop.
  After this import, a reobservation on the honest host returned the forged message.
- `ACS_COMMITMENT_MISMATCH` occurred on each host in a later interval. The test did not attribute it to one contract.
- Without the invented `Coin` on the new host, or with a threshold of 3, the mediator rejected the transaction.

Rules for each new host:

1. Get the snapshot of each new host from a different existing guardian.
2. Make sure that the possibly colluding `gg` hosts, plus the hosts that got a snapshot from them, stay below the `gg` confirmation threshold.
   A guardian is the source for at most one host that is not checked yet (rule 3).
3. Add the new host at `Observation`.
   After its ACS commitments agree with each other `gg` host for one full commitment interval, change it to `Confirmation`.
   This second change needs the threshold of owner keys and the consent of the new host.
   The new host proposes the promotion, and the owners sign it.
   Canton requires the authorization of the party and of the participant for a permission increase.
   A promotion that only the owner keys sign does not take effect.
4. Make sure that the confirmation threshold is a majority of the hosts at `Confirmation`.
   Change the threshold in a separate change if necessary.
5. If an ACS commitment mismatch stays for one full interval after the onboarding flag is clear, remove the new host.
   The new host agrees with its source host and disagrees with the other hosts. Report the source guardian.

Do each change of rules 3 to 5 for `gg`, `feeCustody` and `nttCustody` together.
After each change, do the custody hosting comparison (Section 2).

WARNING: If your participant logs "Mediator approved a request that has been locally rejected",
or stops with "with failed activeness check is approved", do not import a contract to stop the restart loop.
Such an import makes your watcher observe the transaction.
Stop the participant and report a security incident to the other guardians.

**Open item:** the procedure to recover an honest host after this error.

**Open item:** a signed comparison of the snapshot digests of several guardians before the import.

An `Observation` host during onboarding is an exception to the `Confirmation` rule of Section 2.
It is also an exception to the hosting checks of the owner key rotation (Section 7.4).
Do not rotate owner keys while a host is at `Observation`.

LocalNet rehearsal on 2026-11-19 ([`gg_observation_onboarding_check.sh`](../canton/localnet/gg_observation_onboarding_check.sh)):

- The rehearsal onboarded `guardian4` at `Confirmation`, changed it to `Observation`, then promoted it again.
  It did not onboard a host at `Observation` with the onboarding flag. It did not wait for a commitment interval.
- With `guardian4` at `Observation` and `sv` disconnected, a `PublishMessage` failed with `MEDIATOR_SAYS_TX_TIMED_OUT`.
- After the promotion of `guardian4` to `Confirmation`, the same `PublishMessage` committed.

**Open item:** a LocalNet rehearsal of rule 3 as written: onboard at `Observation` with the onboarding flag, wait for one commitment interval, then promote.

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
- The signer must finish in 30 seconds. The ceremony script stops a signer that runs longer, and the ceremony fails.
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
| Genesis acceptance (`AcceptGenesis`): the core roots and the NTT roots | Once for each deployment | 7.2 |
| Onboarding fee admin replacement | Emergency only | 7.9 |
| Owner key rotation (add or remove a guardian) | On guardian set change | 7.4 |
| Fee withdrawal | On request | 7.6 |
| Live-signed NTT registration or factory change | On request | 7.1 |

A governance VAA on Canton is not a `gg` signing ceremony.
Any party submits the governance VAA. The `gg` and `feeCustody` hosts confirm it (Section 7.3).

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
5. Vet no package whose code runs with `gg` authority and can create a `ReplayNode` or exercise `ClaimReplayRoot` with `consumer = gg`.
   The `gg` claimed-roots tree binds only `ClaimReplayRoot`.
   Such code can create a `ReplayNode` with `consumer = gg` in the namespace of a live deployment.
   That opens a second trie, so one inbound VAA can mint or release two times.
   Such code can also claim the pair of a future deployment first. Then that registration fails.

### 6.2 Package classes

| Class | Packages | Approval |
| - | - | - |
| Wormhole | `wormhole-core`, `ntt`, `token-cip0056` | Check the reproducible build. Then vet the exact package ID. |
| Splice | Amulet and other Splice DARs | Follow the DSO schedule. The super validators set the minimum DAR versions. |
| Splice token-standard interfaces | `splice-api-token-*` | Interface packages only. Vet the IDs that the Wormhole packages depend on. |
| Third-party token registry | The registry of a lock/unlock deployment that `gg` takes custody for | Blocked by decision U1 (Section 10). |

Splice Amulet code runs with the authority of `feeCustody`.
The core bridge charges message fees in Amulet, and `feeCustody` receives the fees.
Thus, when your participant vets an Amulet version, you trust that version with the collected fees.
A vetted Amulet version cannot move `gg` holdings. It cannot publish on an `Emitter` that `gg` owns.
It can publish on an `Emitter` that the fee payer or `feeCustody` owns (Section 2).

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
Example: the change from version 0.8.3 to 0.8.4 gives a new package ID.
The build makes sure that each new version is a valid Daml upgrade of the previous release.

The procedure of Section 6.7 applies only to an upgrade that keeps the signatories and observers of each template.
Version 0.8.0 adds `gg` as a signatory of `EmitterRegistry` and `Emitter`.
Version 0.8.3 adds `feeCustody` as a signatory of `CoreState`.
Thus a 0.7.0 `CoreState`, `EmitterRegistry` or `Emitter` cannot run under 0.8.3.
Version 0.8.3 starts from a fresh genesis (Section 7.2).
Versions 0.8.0, 0.8.1 and 0.8.2 are withdrawn: their fee registry code runs with the authority of `gg`.
The approved list of `guardiand` does not contain 0.8.0, 0.8.1 or 0.8.2.

The watcher accepts a `PublishMessage` only from the package ID in `--cantonPackageID`.
An `ExercisedEvent` carries the package ID of the package that executes the choice.
For an `Emitter` from an earlier transaction, the package that created the `Emitter` has no effect on this match.
For an `Emitter` from the same transaction, the watcher accepts the publish only if the pinned package created the `Emitter`, with `gg` as a signatory.
The rule is `readPartySignsExercisedEmitter` in `node/pkg/cantonclient/read_party_stakeholder.go`.
Thus a watcher on one package ID drops each message that a different package version executes.
A transaction that creates an `Emitter` under the old package and publishes on it under the new package gives a message that no pin accepts.
A dropped message uses its emitter sequence number on the ledger.

Each informee participant must vet the package that executes the choice.
`gg` is an informee of each `PublishMessage`.
Thus a package that no guardian participant vets cannot execute `PublishMessage`.

A dependent package calls `wormhole-core` by package ID.
Example: `ntt` 0.3.2 calls `wormhole-core` 0.8.0. Thus `ntt` needs a new build on 0.8.3.
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
This is the one-id procedure. It is the default.
The watcher accepts one core package ID during the procedure.

NOTE: Section 7.5 of `canton/README.md` gives the same one-id procedure for the upgrade from 0.6.0 to 0.7.0.
That section also gives the reasons for each gate and the results of the LocalNet rehearsal.
It also gives the two-id window, an alternative that needs decision U3 (Section 10.1).

Before step 1, install a `guardiand` release whose approved list contains the old and the new core package ID.
The approved list is `node/pkg/cantonclient/approved_core_packages.json`. It gives the version of each package ID.
Outside unsafe dev mode, the watcher does not start with a package ID outside this list.
Each step that sets `--cantonPackageID` also sets `--cantonPackageVersion` to the version of that package ID.
Do not set the retiring flags (Section 8) in this procedure.

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
13. Set `--cantonPackageID` to the new core package ID and `--cantonPackageVersion` to the new version.
14. Restart `guardiand`.
15. Get the dropped update IDs (Section 6.7.1).
16. Send a reobservation request for each dropped update ID (Section 8.1).
17. Make sure that a VAA exists for each dropped update ID.

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
The two-id window removes this outage. It needs decision U3 (Section 10.1).

Step 16 reads each update from your participant.
Participant pruning removes old updates, and then the reobservation request fails.
Make sure that the retention of your participant is longer than the time from step 2 to step 16.

After the unvet, contracts that the old version created work under the new version.
The LocalNet rehearsal of the upgrade from 0.6.0 to 0.7.0 shows this for `CoreState` and `Emitter`.
`canton/localnet/README.md` gives the rehearsal and its limits.
The rehearsal needs a LocalNet from before the 0.8.0 genesis.

**Open item:** a LocalNet run of the abort procedure (Section 6.7.4).

#### 6.7.1 Get the dropped update IDs

1. Wait for the control message of the coordinator under the new core package ID.
   The coordinator publishes it after each guardian completes step 14.
2. Make sure that your watcher observes the control message.
3. Record the offset of the control message on your participant. This is the restart offset.
4. Do the canary scan (Section 6.7.2) from the start offset to the restart offset.
   Give the control message as the required update ID.
5. Keep each `PublishMessage` with the new core package ID as the executing package ID.
6. Compare the update IDs with the WARN logs of your watcher.
   The log text is `canton: dropped PublishMessage executed by a package other than the pinned package`.
   The `updateId` field of each log is a dropped update ID.
7. Make sure that the scan list contains each update ID of the WARN logs.
   The other update IDs of the scan list are the control message and the messages of the restart period.
8. If a WARN log update ID is not in the scan list, tell the coordinator.
9. Scan from the gate offset of Section 6.7.3 to the restart offset, with the control message as the required update ID.
   Make sure that no `PublishMessage` has the old core package ID.

The scan lists each `PublishMessage` that `gg` can see.
The watcher also applies the stakeholder rule of Section 6.4.
A scan line that gets no VAA after a reobservation request can be a publish that this rule drops.
The watcher then writes a WARN log with its update ID.
The log text is `canton: dropped PublishMessage on an Emitter of which the read-as party is not a stakeholder`.

After a stream error, the watcher continues from the offset of its last observation.
Thus it reads the drops after that offset again.
The WARN log and the counter `wormhole_canton_unpinned_publish_messages_total` then count such a drop two times.
Compare the sets of update IDs, not the counter values.

#### 6.7.2 Canary scan and canary gate

The canary scan is a read of the ledger with a Ledger API user that has no right to submit.
The tool is [`publish_message_scan.sh`](../canton/tools/publish_message_scan.sh). It needs only `curl` and `jq`.

1. Make a Ledger API user with only the right `CanReadAs` for `guardianGovernance`.
2. Get a JWT for this user. Write the JWT to a file with mode 0600.
3. Run the scan:

   ```
   publish_message_scan.sh <json-api-url> <token-file> <gg-party> <start-offset> <end-offset> <required-update-id>
   ```

4. Read the output. Each line is one `PublishMessage`, with its offset, update ID and executing package ID.

The end offset is the offset of a control message.
The required update ID is the update ID of that control message.
The scan fails unless its output contains that update at the end offset.
A Ledger API response can end before the end offset, and the offsets alone do not show this.

The scan reads pages of `SCAN_PAGE_SIZE` updates (default 200).
Set `SCAN_PAGE_SIZE` to the node limit of your participant or less.
Each `curl` call stops after `SCAN_CURL_MAX_SECONDS` (default 60).

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
   Get the list with a scan from the start offset to the control message.
   Give the control message as the required update ID.
   Tell the coordinator about each publish under a package ID that is not the old or the new core package ID.

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
9. Scan from the start offset to the gate offset, with the control message as the required update ID.
   Keep each `PublishMessage` with the old core package ID.
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
7. Scan from the start offset to the abort offset, with the control message as the required update ID.
   Keep each `PublishMessage` with the new core package ID.
8. With a quorum of guardians, set `--cantonPackageID` to the new core package ID and restart `guardiand`.
9. Send a reobservation request for each update ID of step 7.
10. When each VAA exists, set `--cantonPackageID` to the old core package ID and restart `guardiand`.
11. Wait for the second control message of the coordinator under the old core package ID.
12. Make sure that your watcher observes it. Record its offset on your participant.
13. Scan from the abort offset to this offset, with the second control message as the required update ID.
14. Send a reobservation request for each `PublishMessage` with the old core package ID that has no VAA.

Steps 8 to 10 cause a second outage for messages under the old core package ID.
From the restart of step 8 to the restart of step 10, the watchers of a quorum accept only the new core package ID.
A new message under the old core package ID gets no VAA in this period.
Steps 13 and 14 recover each of these messages. Thus the guardians lose no message.

A reobservation reads a committed update. Thus step 8 needs no vetting of the new core package ID.
After step 1, a submission under the new core package ID fails at prepare.
The coordinator tells the integrators that the upgrade stopped.

**Your guardian is late or fails after step 10.** A new message gets a VAA only when a quorum observes it.
After each restart of a guardian, send the reobservation requests of step 16 again.

**A message under the old core package ID has no VAA after step 14:**

1. Wait for the control message of the coordinator under the new core package ID.
   Make sure that your watcher observes it. Record its offset on your participant. This is the recovery offset.
2. A quorum of guardians sets `--cantonPackageID` to the old core package ID again and restarts.
   No vetting change is necessary. A reobservation reads a committed update.
3. Send a reobservation request for each of these update IDs.
4. When each VAA exists, set `--cantonPackageID` to the new core package ID again and restart.
5. Wait for the second control message of the coordinator under the new core package ID.
   Make sure that your watcher observes it. Record its offset on your participant.
6. Scan from the recovery offset to this offset, with the second control message as the required update ID.
7. Send a reobservation request for each `PublishMessage` with the new core package ID that has no VAA.

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

### 7.2 Genesis acceptance

Each deployment of `wormhole-core` 0.9.0 starts from a fresh genesis.
Contracts of an earlier version do not move to 0.9.0.
For the reason, refer to [`A14_EMITTER_ID_UNIQUENESS.md`](../canton/A14_EMITTER_ID_UNIQUENESS.md).

The operator does the deployment work.
It uploads the DARs to its participant and allocates its party.
Then it creates one `WormholeGenesisOffer` of the package `wormhole-genesis`. The offer holds each genesis parameter.

The guardians sign one prepared transaction: `AcceptGenesis` on the offer.
Its actors are `gg`, `feeCustody` and `nttCustody`. Each actor needs its owner key threshold.
The transaction creates `CoreState`, `EmitterRegistry`, `ReplayRootRegistry`, `NttGovernance` and the `gg` `CoinFactory`.
It archives the offer.
The operator signs the offer, so the operator participant confirms the acceptance. This is the last operator confirmation.

Before the ceremony:

1. Make sure that each guardian participant hosts `gg` (Section 4.1).
2. Make sure that `feeCustody` and `nttCustody` exist in the `gg` namespace.
3. Make sure that each of the two parties has the hosts, the confirmation threshold and the signing keys of `gg`.
4. Make sure that `feeCustody` is not `nttCustody`.
5. Make sure that each guardian participant vets the agreed package IDs of `wormhole-core`, `ntt`, `wormhole-genesis` and `token-cip0056`.
6. Make sure that your participant vets no other `wormhole-core` package.
7. Get the agreed parameter file from the genesis coordinator. Compare each value with your own record.
8. For a first genesis, make sure that your participant shows no active `CoreState`, `EmitterRegistry`, `NttGovernance` or claimed-roots `ReplayNode` of your `gg`.
   The ledger cannot detect a second first genesis. One `gg` has one `FirstGenesis`: this is a guardian rule.
   Each first genesis creates a claimed-roots tree. Two first geneses of one `gg` make two trees, and a registration then registers one time in each tree.

Check the offer:

1. Make sure that your participant shows exactly one `WormholeGenesisOffer` of the operator.
2. Compare the `parameters` of the offer with the agreed parameter file. Each field must be equal.
3. Make sure that `genesisGuardianSet` has the agreed guardian set index, the agreed guardian addresses and the public key of each guardian.
4. Make sure that `governanceChainId` is 1 and `governanceContract` is the agreed governance contract.
5. Make sure that `feeCustody` is the `feeCustody` party and `nttCustody` is the `nttCustody` party.
6. Make sure that `onboardingFeeAdmin` is the agreed admin.
7. Make sure that `feeInstrument` is the agreed instrument. Its admin is not `gg` and not `feeCustody`.
8. Make sure that `initialMessageFee`, `registrationFee` and `replayRootClaimFee` are 0 (Section 7.8).
9. For a first genesis, make sure that `genesisKind` is `FirstGenesis`.
10. For a first genesis, make sure that `firstEmitterId` and `firstNttManagerId` are 0.
11. For a first genesis, make sure that `coreGovernanceReplayRecord` and `nttGovernanceReplayRecord` are empty.
    Each `consumedGovernance` map has no entry. No record has `evictedGovernanceMaxTimestampSeconds`.
    The `CoreState` precondition requires a key for each guardian only without governance history.

Check the prepared transaction:

1. Make sure that the transaction has exactly one command: `AcceptGenesis` on the offer.
2. Make sure that `agreedParameters` is equal to your agreed parameter file. Do not copy the parameters from the offer.
3. Make sure that each template package ID is an agreed package ID.
4. Make sure that the prepared transaction has exactly three signer sets: `gg`, `feeCustody` and `nttCustody`.
5. Sign the prepared hash for each of the three parties (Section 7.1).

`AcceptGenesis` compares `agreedParameters` with the offer again. The template preconditions also reject bad values.
These checks do not replace your checks.

After genesis:

1. Make sure that your participant shows exactly one `CoreState`. Its signatories are `gg` and `feeCustody`.
2. Make sure that your participant shows exactly one `EmitterRegistry` with `nextId` 0. Its only signatory is `gg`.
3. Make sure that your participant shows exactly one `ReplayRootRegistry`. Its only signatory is `gg`.
4. Make sure that your participant shows exactly one `NttGovernance`. Its signatories are `gg` and `nttCustody`.
5. Make sure that your participant shows exactly one `CoinFactory` with the admin `gg`.
   Make sure that your participant shows exactly one `ReplayNode` with the consumer `gg` and the namespace `wormhole-core:replay-roots`.
   This node is the root of the `gg` claimed-roots tree. `AcceptGenesis` returns it as `claimedRootsNodeCid`.
   Record the `rootIdentity` of the `NttGovernance`. Compare it with the value that each other guardian records.
6. Make sure that the operator is a signatory or an observer of none of these contracts.
7. Run [`emitter_id_scan.sh`](../canton/tools/emitter_id_scan.sh) with begin offset 0 and `EXPECT_ACTIVE_REGISTRIES=1`.
   Set `EXPECTED_CORE_PACKAGE_ID` to the agreed `wormhole-core` package ID.
   Use a Ledger API user that has only the right `CanReadAs` for `guardianGovernance`.
8. Make sure that the scan passes with `firstId` 0.
9. Unvet `wormhole-genesis` on your participant. No contract of that package stays active.

If `AcceptGenesis` fails, no root exists and the offer stays active. Do the ceremony again on the same offer.
If the parameters are incorrect, ask the operator to withdraw the offer (`WithdrawGenesisOffer`) and to make a new offer.

A re-genesis replaces the roots of an earlier genesis (`genesisKind` `ReGenesisReplacing`).
Its parameters carry the earlier governance replay records and the earlier next IDs.
The new `NttGovernance` keeps the `rootIdentity` of the earlier root.
Use the genesis procedure of the NTT repository (`canton/README.md`) for a re-genesis.

An emitter ID is an integer from 0 to 2^53 - 1 (`maxEmitterId`).

LocalNet rehearsal: [`gg_genesis.sh`](../canton/localnet/gg_genesis.sh).
Before `gg_genesis.sh`, run `custody_host.sh` for each custody party.
Then run `ntt_upload_dars.sh`.
The local party `LabsOperator` on `app-provider` creates the offer. It has no signing key.
The script compares the offer on `sv` with the agreed parameters that it builds itself.
These acceptances fail:

- `gg` with one of three owner keys: `FAILED_TO_EXECUTE_TRANSACTION`.
- No `nttCustody` signature. The error names `nttCustody`.
- `agreedParameters` with another guardian set index: `genesis: agreed parameters differ from the offer`.

Then two owner keys for each of the three parties sign `AcceptGenesis`. The rehearsal counts one `gg` ceremony.
On a rerun, the script checks the `feeRecipient` and the signatories of the active `CoreState`.

### 7.3 Guardian set upgrade on Canton

The `GuardianSetUpgrade` governance VAA changes the guardian set in `CoreState`.
Any party submits it with `SubmitGovernanceVAA`, for example a relayer. The submitter needs a disclosure of `CoreState` (Section 7.10).
The current guardian set must sign the VAA.
The `gg` hosts confirm the transaction. The owner keys do not sign it.

The old guardian set stays valid until its expiry time.
The expiry time is the ledger time of the upgrade plus 86400 seconds.
The expiry check compares whole seconds: the old set is valid while `now <= expiry`.
Thus the old set is valid for all of the expiry second, as on EVM.
The old set is not valid one second after the expiry second.
A governance VAA must have the signatures of the current set.

1. Sign the `GuardianSetUpgrade` VAA through the usual guardian procedure.
2. Keep your participant online until `CoreState` shows the result of the VAA.
3. Make sure that `CoreState` shows the new guardian set index.
4. Make sure that `CoreState` shows the expiry time of the old set.
5. Immediately after the upgrade, submit `AttachGuardianPublicKeys` with the key of each new guardian.
   This step is mandatory. The upgrade keeps only the keys of guardians that were in a stored set.
   Any party can submit the attach.
6. Make sure that each member of the new set has a public key.
   A signature of a guardian without a key fails the VAA, also when the keyed signers reach the quorum.
   Until the attach, a relayer can remove those signatures. The digest stays the same.

LocalNet rehearsal: [`gg_guardian_set_rotation.sh`](../canton/localnet/gg_guardian_set_rotation.sh).
The rehearsal checks these results:

- A VAA of the new set passes `ParseAndVerifyVAA`.
- A VAA of the old set passes until the expiry time.
- A governance VAA of the old set fails with `governance: not signed by current guardian set`.
- A VAA of the new set with an old key fails with `invalid signature for guardian index 0`.
- With a set of three guardians, a VAA with two signatures fails with `no quorum: have 2, need 3`.

The rotation from a one-guardian set to a three-guardian set passed live on 2026-11-17.
All five results above passed in that run.

LocalNet uses the clock time, thus it does not reach the expiry time.
The Daml Script test `Test.TestCore:testGuardianSetUpgradeExpiry` checks the expiry boundary.

#### 7.3.1 Timestamp of a governance VAA

This rule applies to each governance VAA for Canton except `GuardianSetUpgrade`.
`CoreState` keeps the digests of at most 32 governance VAAs.
When it removes a digest, it sets `evictedGovernanceMaxTimestampSeconds` to the timestamp of that digest.
A `SetMessageFee`, `TransferFees` or `ContractUpgrade` VAA with a timestamp at or below this value fails.
A `GuardianSetUpgrade` VAA at or below this value applies, and `CoreState` does not record its digest.
The current-set signature and the rule `newIndex == current + 1` prevent its replay.
Thus a `GuardianSetUpgrade` with an old timestamp, for example 0, does not stop Canton on the old set.
The error is `governance: VAA timestamp at or below the evicted governance timestamp floor`.

Before you sign a governance VAA for Canton, do these steps:

1. Read the active `CoreState` from a `gg` host through the Ledger API endpoint `/v2/state/active-contracts`.
2. Read `evictedGovernanceMaxTimestampSeconds` in the `createArgument` of the `CoreState`.
3. Set the proposal timestamp to the current time in Unix seconds.
4. Make sure that the proposal timestamp is more than the value of step 2.
   The value `null` means that no floor applies.

On LocalNet, this command shows the value:

```bash
active_contracts "$VALIDATOR_NAME" "$GG_PARTY" :Wormhole.Core.State:CoreState \
  | jq -e 'if length == 1 then .[0].createArgument.evictedGovernanceMaxTimestampSeconds else error("expected one CoreState") end'
```

All messages in one `InjectGovernanceVAA` request share one timestamp.
An eviction can raise the floor to that timestamp before you submit each message of the request.
Then the remaining messages fail, and you must issue them again.

If a governance VAA fails with the floor error, do these steps:

1. Make a new proposal with a new timestamp above the floor.
2. Sign the new VAA through the usual guardian procedure.
3. Submit the new VAA.

> **NOTE:** EVM, Sui and Solana do not have this floor.
> A wildcard (chain 0) VAA with an old timestamp can pass on those chains and fail on Canton.
> For Canton, issue a new VAA with a new timestamp.

### 7.4 Owner key rotation

An owner key rotation removes one owner key and adds a new owner key.
The namespace threshold and the signing key threshold of each party do not change.

The owner keys are the namespace owners and the signing keys of each party in the namespace.
Thus the rotation changes the decentralized namespace and the `PartyToParticipant` mapping of each party.
The namespace has `gg` and `feeCustody`. Thus the rotation has three topology changes.
If the namespace also has the `nttCustody` party of an NTT custody deployment, the rotation has four topology changes.

The rotation has three phases:

1. Safety stop: the coordinator reads the namespace and each `PartyToParticipant` mapping in it on each host.
   The rotation stops before any change if two hosts show different mappings or serials.
   It also stops if a party in the namespace has a `PartyToKeyMapping`.
   It also stops if the hosts of a party are not exactly the guardian hosts.
   It also stops if the rotation list and the parties in the namespace differ.
   It also stops if the keys of the namespace or of a party are not the old key set or the new key set.
2. Sign phase: each owner signs each topology change. No change goes to the synchronizer.
   Before the first signature, the coordinator compares each generated change with the change that it calculates.
   The coordinator calculates each change from the views of the safety stop, which all hosts agree on.
   Each calculated mapping must have the guardian policy before the first signature.
   The coordinator keeps the signed changes in a file that only the coordinator can read.
3. Load phase: one load call submits all the changes.
   The load order is `nttCustody`, `feeCustody`, `gg`, then the namespace.

If the new owner has no root namespace delegation, the load phase loads it first.
This delegation alone gives no authority over the `gg` namespace.
A signer failure in the sign phase stops the rotation before the first load.
The topology then does not change.

Procedure:

1. Make sure that the time is not in an LSU freeze window.
2. Get the public key of the new owner in DER SubjectPublicKeyInfo format.
3. Get the list of the parties in the namespace from the coordinator.
   Make sure that the list has `feeCustody`.
   Make sure that the list has `nttCustody` if an NTT custody deployment exists.
4. Before the signature, decode each topology change of the rotation.
   Read the current mappings and serials from the participant that you operate, or from all hosts.
   Do not use the values of the coordinator participant alone.
   Compare each change with the rotation plan:
   - The operation is `Replace`.
   - The serial is the current serial plus 1. The root namespace delegation of the new owner is the only exception.
   - The root namespace delegation of the new owner has the fingerprint of the new key as the namespace.
     Its target is the new key, and its serial is 1. Only the new owner signs it.
   - A `PartyToParticipant` change has the same party, hosts, host permissions and confirmation threshold as the current mapping.
   - A `PartyToParticipant` change has the new signing keys and the current signing key threshold.
   - The namespace change has the new owners and the current namespace threshold.

   Do not sign a change that is different from the plan.
   Section 5.3 has the same rule for a prepared transaction.
5. Sign each topology change of the rotation with your owner key.
6. After the load, make sure that the namespace contains the new owner key.
7. Make sure that the namespace does not contain the removed owner key.
8. For each party in the namespace, make sure that the signing keys contain the new owner key.
9. For each party in the namespace, make sure that the signing keys do not contain the removed owner key.
10. Make sure that the namespace threshold and the signing key threshold of each party did not change.
11. Make sure that the effective times of the changes are in the load order: `nttCustody`, `feeCustody`, `gg`, then the namespace.
12. Make sure that the time from the first effective time to the last effective time is 30 seconds or less.
13. Do the custody hosting comparison (Section 2).

Canton gives each change of one load call its own effective time.
On LocalNet with Canton 3.7.3, the spread was about 1 s.
In that window, `gg` and the namespace accept the removed key after the custody party changes.
This window is an input to decision U4 (Section 10.1).

After the rotation, a signature from the removed key is not valid.
A `gg` transaction with the removed key fails with `FAILED_TO_EXECUTE_TRANSACTION`.
After the load, the LocalNet script reads all hosts.
It makes sure that the namespace owners are exactly the new owner set.
It also makes sure that no `PartyToParticipant` mapping of the namespace has the removed key.
Without a key set, the removed key has no authority over the namespace or a party.

WARNING: The participant rejects a complete load call if one change fails its local check.
After that check, the synchronizer applies each change alone, at its own effective time.
Thus a load can apply only a part of the changes. The rotation is then half-applied.
In that state, the signing keys of a party can still contain the removed owner key.
The removed key then still counts for the signing threshold of that party.
If that party is `nttCustody`, the removed key and one kept key can move the NTT reserves.
If that party is `feeCustody`, the removed key and one kept key can move the collected fees.
If the `ROTATION HALF-APPLIED` line names `namespace`, the removed key and one kept key can change the mapping of each party in the namespace.
Complete the rotation with `--resume` immediately.
Do not start a different ceremony before the rotation is complete.

A signer failure and a load that the participant rejects completely leave the removed key valid for each party.
The added key is then not valid for any party. Thus no party has a mixed key state.

WARNING: The signed changes are an authorization.
Each signed change stays valid until a different change takes its serial.
The coordinator must keep the file of the signed changes secret until the load.
After a load attempt, the participant of the coordinator also has each signed change. The secrecy of the file does not remove that copy.
After a stopped rotation, the file stays valid. The next change of each mapping makes it not valid.
After a complete rotation, the coordinator removes the file.

Measured behavior on LocalNet with Canton 3.7.3:

- A load without the `nttCustody` change applies the other changes. The rotation is then half-applied.
- `gg_rotation_fault_check.sh` case R7 does the same test without the `feeCustody` change.
- A load in which one change has a missing owner signature fails completely. No mapping changes.

#### Recovery from a half-applied rotation

The rotation script shows `ROTATION HALF-APPLIED`.
It also shows the parties that still accept the removed key.

1. Do not generate a new key for the new owner.
2. Run the rotation again with `--resume`.
   `--resume` loads the changes that the owners signed in the sign phase. No owner signs again.
3. Do steps 6 to 10 of the procedure again.
   The changes of a `--resume` are in more than one load call. Thus steps 11 and 12 do not apply.

If you start a new rotation instead of `--resume`, the safety stop stops it before any change.

LocalNet rehearsal: [`gg_rotate_owner_key.sh`](../canton/localnet/gg_rotate_owner_key.sh),
[`gg_rotation_fault_check.sh`](../canton/localnet/gg_rotation_fault_check.sh) and
[`gg_rotation_safety_stop_check.sh`](../canton/localnet/gg_rotation_safety_stop_check.sh).
The LocalNet script follows the three phases of this section.
It keeps the signed changes in a file for `--resume`.
On 2026-11-17, a live run of the earlier script passed. That script loaded each change in a separate load call.
On 2026-11-18, `gg_rotation_fault_check.sh` and `gg_rotation_safety_stop_check.sh` passed on LocalNet with Canton 3.7.3.

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

**Open item:** run these rehearsals again with `wormhole-core` 0.8.3.
Include `gg_rotation_fault_check.sh` and `gg_rotation_safety_stop_check.sh` (Section 7.4).

Limits of the rehearsal:

- One container runs all three participants and the synchronizer.
- The owner keys are files on the host, not keys in guardian custody.
- The checks disconnect participants. They do not stop the participant processes.

### 7.6 Fee withdrawal

A `TransferFees` governance VAA creates a `FeeWithdrawalAuthorization`.
`gg` is its only signatory. It has no observer.
The contract has no choice. Thus only a transaction that `gg` signs can archive it.
The fees are holdings of `feeCustody`.
The `amount` field is in units of 10^-10 of the fee instrument.

The prepare endpoint accepts one command in each transaction (Section 7.2).
Thus the withdrawal is two prepared transactions:

| Order | Transaction | `actAs` | Signer sets |
| - | - | - | - |
| 1 | Archive of the `FeeWithdrawalAuthorization` | `gg` only | `gg` |
| 2 | `TransferFactory_Transfer` with `sender` set to `feeCustody` | `feeCustody` only | `feeCustody` |

Do the archive first. The reasons are:

- No Daml rule connects the transfer to the authorization. The transfer does not read the authorization.
- The ledger accepts one archive of a contract. It rejects each other archive of that contract.
- Thus, with the archive first, each authorization gives a maximum of one transfer.
- With the transfer first, the authorization stays active after the transfer. A second transfer is then possible.
- If the archive commits and the transfer fails, the fees stay with `feeCustody`. This is safe.

`gg` is not a submitter of the transfer. Thus the registry code of the transfer gets only the authority of `feeCustody`.
The coordinator keeps a withdrawal record. The record has one row for each authorization.
Each row has the update ID of the archive and the update ID of the transfer.

Transaction 1, the archive:

1. Make sure that exactly one active `FeeWithdrawalAuthorization` has the `vaaHash` of the VAA.
2. Make sure that its `amount` is equal to the amount of the VAA.
3. Make sure that its `recipient` is equal to the recipient of the VAA.
4. Make sure that the transaction archives only this contract.
5. Make sure that `actAs` is `gg` only.
6. Make sure that the prepared transaction has exactly one signer set: `gg`.
7. Sign the prepared hash for `gg` (Section 7.1).
8. Record the update ID of the archive in the withdrawal record.

Transaction 2, the transfer:

1. Make sure that the archive of transaction 1 committed.
2. Make sure that the withdrawal record has no transfer for this update ID.
3. Make sure that the transfer amount is equal to the `amount` of the archived authorization.
4. Make sure that the Canton receiver is the agreed party for the 32-byte `recipient`.
5. Make sure that `sender` is `feeCustody`.
6. Make sure that the instrument is the fee instrument.
7. Make sure that `actAs` is `feeCustody` only.
8. Make sure that the prepared transaction has exactly one signer set: `feeCustody`.
9. Sign the prepared hash for `feeCustody` (Section 7.1).
10. Record the update ID of the transfer in the withdrawal record.

If transaction 2 fails:

1. Read the transactions of `feeCustody` after the archive.
2. Make sure that no transfer of transaction 2 committed.
3. Prepare transaction 2 again with the same command ID.

Canton rejects a second command with the same command ID in the deduplication period.
If no attempt commits, the guardians make a new `TransferFees` VAA. The archived authorization gives no other transfer.

`feeCustody` must hold only holdings of the fee instrument.
A third party can send other holdings to `feeCustody`. Do not accept them.
Monitor the holdings of `feeCustody` for other instruments.

**Open item:** the mapping from the 32-byte `recipient` to a Canton party.

### 7.7 Key compromise and guardian removal

**Owner key compromise.** Do the owner key rotation (Section 7.4) immediately to remove the compromised key.

**Owner key loss.** If fewer than the threshold number of owner keys stay, no `gg` transaction or topology change is possible.
[`gg_show_topology.sh`](../canton/localnet/gg_show_topology.sh) shows the namespace threshold.

**Participant key or OAuth2 client secret compromise.**
**Open item:** the procedure for each of these cases.

**Operator compromise.** The operator is a signatory of no contract after genesis.
A compromised onboarding fee admin can set the registration fee and the replay root claim fee.
Each fee stays in the range 0 to `maxOnboardingFee`.
It cannot change the message fee, a guardian set, an `Emitter`, a replay node or a governance result.
Replace the admin with the emergency ceremony (Section 7.9).

**Guardian removal:**

1. Remove the owner key of the guardian with an owner key rotation (Section 7.4).
2. Remove the `gg`, `feeCustody` and `nttCustody` hosting from the participant of the guardian.
   That participant can remove its own hosting. Other changes to the hosting need the `gg` threshold.
3. Make sure that the confirmation threshold of each of the three parties is a majority of the hosts that stay.
   If a change is necessary, change the threshold of the three parties together.
4. Remove the Ledger API user of the watcher of the guardian.
5. Do the custody hosting comparison (Section 2).

CAUTION: A participant that stops to host a party keeps its copy of the contracts of that party.
The copy does not change after the removal.
If the participant hosts another stakeholder of these contracts, it rejects valid transactions with `LOCAL_VERDICT_INACTIVE_CONTRACTS`.
LocalNet showed this on 2026-11-19: a removed host rejected an NTT `Release` because of an archived custody pot.
Do not add a removed participant again without a new replication (Section 4.3).

**Open item:** the procedure to remove the stale contracts of a party from a participant that no longer hosts it.

### 7.8 Fee increase gate

NTT paths where `gg` pays a fee fail when the fee is above 0 (decision U5).
Keep each fee at 0 until each NTT path has a fee payer that is not `gg`.

1. Do not sign a `SetMessageFee` VAA for chain 75 with a fee above 0.
2. Do not accept a genesis offer with a `registrationFee` or a `replayRootClaimFee` above 0.
3. Make sure that the onboarding fee admin sets no registration fee and no claim fee above 0.
4. Make sure that the fee payer of each NTT path is not `gg`. Record the result before a fee goes above 0.

### 7.9 Onboarding fee admin replacement

Use this ceremony only in an emergency, for example a compromised or lost onboarding fee admin.
The core governance VAA set has no admin action. Thus `gg` replaces the admin directly.
The ceremony is two prepared transactions, one for each registry:

| Order | Command | `actAs` | Signer sets |
| - | - | - | - |
| 1 | `ReplaceEmitterRegistryFeeAdmin` on the active `EmitterRegistry` | `gg` only | `gg` |
| 2 | `ReplaceReplayRootRegistryFeeAdmin` on the active `ReplayRootRegistry` | `gg` only | `gg` |

For each transaction:

1. Make sure that the registry is the one active registry of your `gg`.
2. Make sure that `newOnboardingFeeAdmin` is the agreed new admin.
3. Make sure that the new admin is not `gg`, `feeCustody` or `nttCustody`.
4. Make sure that the prepared transaction has exactly one signer set: `gg`.
5. Sign the prepared hash for `gg` (Section 7.1).

After the ceremony:

1. Make sure that each registry shows the new admin.
2. Make sure that the fee of each registry did not change.
3. Make sure that the `nextId` of the `EmitterRegistry` did not change.

### 7.10 Disclosures for submitters

`gg` is a signatory of each core contract and of each NTT root.
Thus each guardian participant can read the created event blob of each of these contracts as `gg`.
A submitter that is not a stakeholder needs these disclosures. For example: `CoreState` for a publish, a VAA check or a governance VAA.
The operator participant is not necessary for a disclosure.

To serve disclosures from a guardian participant:

1. Use a Ledger API user that has only the right `CanReadAs` for `guardianGovernance`.
2. Read the active contracts as `gg` with `includeCreatedEventBlob` set to true.
3. Give the `templateId`, `contractId`, `createdEventBlob` and `synchronizerId` of each contract to the submitter.
4. Do not give a submitter a token of this user.

The command `guardiand canton-disclosure-service` does steps 2 and 3 over HTTP (Section 7.12).

A disclosure gives visibility only. It gives no authority.
A stale disclosure fails the transaction. It cannot make a wrong transaction commit.

LocalNet rehearsal: [`gg_operator_independence.sh`](../canton/localnet/gg_operator_independence.sh).
With `app-provider` (the operator participant) disconnected, local parties on `sv` and `app-user` submit each Wormhole function.
They use disclosures that `sv` reads as `gg`.

### 7.11 Registration VAA of a `gg`-minted NTT deployment

`RegisterManagerByVaa` registers a burn/mint deployment of a `gg`-minted token.
It needs a governance VAA with the action `RegisterBurnMintManager`. The guardians sign this VAA.
The VAA holds a registration binding (`wormhole:ntt-registration:v2`, `ntt` 0.6.0 and later).
The binding holds the `rootIdentity` of one `NttGovernance` root.
Thus a root with another identity rejects the VAA.

A first genesis sets `rootIdentity` from `gg` and the ledger time of `AcceptGenesis`.
Two first geneses at one ledger time make two roots with one identity. Each root then accepts the VAAs of the other root.
One root for each `gg` is a guardian rule.

Before you sign a registration VAA:

1. Read the active `NttGovernance` of `gg` on your own participant. Use a user that has only `CanReadAs` for `gg`.
2. Make sure that your participant shows exactly one `NttGovernance` of `gg`.
3. Make sure that its `rootIdentity` is equal to the value that you recorded at genesis (Section 7.2).
4. Calculate the binding yourself: `keccak256(tag ‖ rootIdentity ‖ lp(utf8(gg)) ‖ lp(utf8(admin)) ‖ uint64(instrumentNonce))`.
   The tag is the UTF-8 text `wormhole:ntt-registration:v2`. `lp` is a 4-byte big-endian length prefix. `uint64` is big-endian.
   The NTT repository has the reference function `nttRegistrationBindingFor` (`canton/ntt/daml/Wormhole/Ntt/Manager.daml`).
5. Compare your binding with the binding in the VAA payload. Sign the VAA only if the two values are equal.

Do not copy `rootIdentity` from the request. Read it from your own participant.

### 7.12 Disclosure service

The command `guardiand canton-disclosure-service` serves the core disclosures of Section 7.10 over HTTP.
It reads your participant as `gg`, keeps an in-memory index, and serves the routes of `canton/README.md` §4.7.
The Guardians choose who runs the public instance.
Each guardian can also run an instance for its own submitters.

The ledger checks each disclosure. A wrong or stale disclosure makes the transaction fail.
Thus the service affects liveness only.

#### 7.12.1 Flags

| Flag | Value |
| - | - |
| `--cantonRPC` | Mandatory. gRPC endpoint of the Ledger API v2 of your participant (`host:port`). |
| `--cantonReadAsParty` | Mandatory. The `guardianGovernance` party ID. |
| `--cantonAuthTokenURL` | OAuth2 token endpoint. Use an absolute `https` URL. |
| `--cantonAuthClientID` | OAuth2 client ID. Put the secret in the `GUARDIAND_CANTONAUTHCLIENTSECRET` environment variable. |
| `--cantonAuthAudience` | Optional. The same value as for the watcher (Section 8). |
| `--unsafePlaintext` | Local sandbox only. OAuth2 cannot use it. |
| `--listenAddress` | Mandatory. HTTP listen address of the routes (`host:port`). |
| `--statusAddr` | Listen address of `/metrics`. The default is `127.0.0.1:6061`. An empty value disables it. |
| `--indexByteCapBytes` | Byte cap of the index. The default is 4 GiB. |
| `--ledgerLagBound` | Largest ledger lag for a ready service, from 10 seconds to 1 hour. The default is 1 minute. |
| `--resnapshotInterval` | Interval of the snapshot comparison, from 1 minute to 168 hours. The default is 1 hour. |
| `--logLevel` | Log level: `debug`, `info`, `warn` or `error`. The default is `info`. |

The command refuses to start if a flag is not valid.
It uses the same checks as the watcher for TLS, OAuth2 and the party ID.
It accepts only the core package IDs of the approved list in `guardiand`.
It stops on `SIGINT` or `SIGTERM`, and completes the requests in progress for up to 10 seconds.

Use a Ledger API user that has only the right `CanReadAs` for `gg` (Section 7.10).

#### 7.12.2 Edge proxy

The service serves plain HTTP. Its request limit applies to all clients together. Put an edge proxy in front of it.

1. End the TLS connections at the proxy.
2. Set a request rate limit for each client address.
3. Send only `GET` requests for paths that start with `/v1/` to `--listenAddress`.
4. Set a request timeout of 10 seconds or less.
5. Do not send requests to `--statusAddr` through the proxy.
6. Use `GET /v1/readyz` for the health check of the proxy.

The service also has its own limits:

- At most 256 lookups at the same time. Each lookup above this limit gets 503.
- A request URI of at most 4096 bytes, and headers of at most 16 KiB.
- Read timeout 10 seconds, write timeout 10 seconds, idle timeout 60 seconds.

#### 7.12.3 Readiness and memory

`GET /v1/readyz` gives 200 when the service can serve current disclosures. The `status` field shows the cause of a 503:

| Status | Meaning |
| - | - |
| `bootstrapping` | The first snapshot is not complete. |
| `stream_disconnected` | The update stream is not connected. |
| `ledger_lag_above_bound` | The newest ledger end read that the index reached is older than `--ledgerLagBound`. |
| `index_singleton_missing` | A singleton has no live instance. |
| `index_singleton_conflict` | A singleton has two or more live instances. |
| `index_inconsistent` | The index refused an update. The service builds a new index. |
| `index_resnapshot_difference` | A periodic snapshot found a stale or missing contract. The service builds a new index. |

The status `ready_byte_cap_reached` gives 200. In this state, the service serves the singletons only.
`/v1/replay-node` and `/v1/claimed-roots-node` give 503.
At each `--resnapshotInterval`, the service builds a new index and tries again.
To serve the nodes again, increase `--indexByteCapBytes`.

The service builds a new index in the background after a byte cap retry or a snapshot difference.
During this build, the old index continues to follow the ledger, and `/v1/readyz` shows its state.
The new index replaces the old index when it reaches the same offset.
After other rebuild causes, the status is `stream_disconnected` until the new index is complete.
The service reads the ledger end every 5 seconds, also during a build.

The index uses the blob bytes plus 1536 bytes for each contract.
A `ReplayNode` blob has about 66 bytes for each digest, plus the fixed bytes of the node.
From 8.4 M to 134 M digests in one trie, the trie has about 1.05 M live nodes (`canton/README.md` §4.6).
Thus the index uses about 1.05 M x (1536 + F) bytes plus 66 bytes for each digest. F is the fixed blob bytes of a node.
With F = 1 KiB, the default cap of 4 GiB holds about 24 M digests.
Measure F on your participant, and set `--indexByteCapBytes` from the result.

During a rebuild, the old index, the snapshot and the new index are in memory together.
Thus plan for a peak memory of about 3 times `--indexByteCapBytes`. For the default cap, this is 12 GiB.

A lookup that gets 503 because of the byte cap has no `Retry-After` header. The cap continues until the next `--resnapshotInterval`.
Other 503 answers have `Retry-After: 1`.
The service writes at most one warning log each second for 503 answers. The log field `suppressedUnavailableLogs` gives the number of skipped logs.

#### 7.12.4 Monitoring

| Metric | Alert condition |
| - | - |
| `wormhole_canton_disclosure_service_readiness{status="ready"}` | The value is 0 for 1 minute. The `status` label with the value 1 shows the `/v1/readyz` status. |
| `wormhole_canton_disclosure_index_readiness{readiness="byte_cap_reached"}` | The value is 1. |
| `wormhole_canton_disclosure_index_used_bytes` | The value is more than 80 % of `wormhole_canton_disclosure_index_byte_cap_bytes`. |
| `wormhole_canton_disclosure_ledger_lag_seconds` | The value is more than `--ledgerLagBound`. |
| `wormhole_canton_disclosure_stream_connected` | The value is 0 for 1 minute. |
| `wormhole_canton_disclosure_index_rebuilds_total` | The value increases. The `reason` label shows the cause. |
| `wormhole_canton_disclosure_resnapshot_differences_total` | The value increases. The `kind` label shows the difference. |
| `wormhole_canton_disclosure_index_ignored_unknown_archives` | The value increases. |

The service calculates each gauge when Prometheus reads `/metrics`. Thus the ledger lag increases also while the index writer waits.

## 8. Watcher configuration

The Canton watcher in `guardiand` reads the Ledger API of your participant.

| Flag | Value |
| - | - |
| `--cantonRPC` | gRPC endpoint of the Ledger API v2 of your participant. |
| `--cantonPackageID` | Package ID of `wormhole-core`. Mandatory outside unsafe dev mode. Change it on each new core version (Section 6.7). |
| `--cantonPackageVersion` | Version of `--cantonPackageID` (`MAJOR.MINOR.PATCH`). Mandatory with it outside unsafe dev mode. |
| `--cantonRetiringPackageID` | Retiring core package ID of the two-id window (Section 10.1, U3). Empty outside the window. |
| `--cantonRetiringPackageVersion` | Version of `--cantonRetiringPackageID`. Mandatory with it outside unsafe dev mode. |
| `--cantonRetiringPackageAcceptedUntil` | Exclusive record-time deadline of the retiring package ID (RFC 3339, UTC). Mandatory with it. |
| `--cantonReadAsParty` | The `guardianGovernance` party ID, `<hint>::1220` and 64 lowercase hexadecimal characters. Mandatory outside unsafe dev mode. |
| `--cantonAuthTokenURL` | OAuth2 token endpoint for the Ledger API. Use an absolute `https` URL with no user information and no fragment. |
| `--cantonAuthClientID` | OAuth2 client ID (client-credentials grant). |
| `--cantonAuthClientSecret` | OAuth2 client secret. Use the `GUARDIAND_CANTONAUTHCLIENTSECRET` environment variable. |
| `--cantonAuthAudience` | Optional. The `audience` parameter of the OAuth2 token request. Set it to the Ledger API target audience of your participant. |

Outside unsafe dev mode, the watcher does not start without `--cantonPackageID`, `--cantonPackageVersion` and `--cantonReadAsParty`.
Each package ID has exactly 64 lowercase hexadecimal characters.
Outside unsafe dev mode, each pair of package ID and version must be in the approved list of `guardiand`.
Set the three OAuth2 flags together, or set none of them.
`--cantonAuthAudience` needs the three OAuth2 flags.
OAuth2 needs TLS, thus unsafe dev mode cannot use it.

Canton 3.7 requires an `aud` claim in each Ledger API token.
The participant compares the `aud` claim with its target audience.
Some identity providers (for example Auth0) add `aud` only when the token request has an `audience` parameter.
For these providers, set `--cantonAuthAudience`.
If your identity provider adds `aud` itself (for example a Keycloak audience mapper), you can leave the flag empty.

Use the target audience of your participant as the value:

| Participant configuration | Value |
| - | - |
| Splice validator | The value of `auth.targetAudience` (Helm) or `LEDGER_API_AUTH_AUDIENCE` (Docker Compose), for example `https://canton.network.global`. |
| Canton participant with `target-audience` in its Ledger API `auth-services` | That `target-audience` value. |
| Canton participant without `target-audience` | `https://daml.com/jwt/aud/participant/<participantId>`. |

The value must not contain whitespace or control characters.
A wrong value causes the error `Audience doesn't match the target value` on the participant.
References:

- Audience-based tokens: <https://docs.canton.network/appdev/deep-dives/authorization.md>
- Target audience of a participant: <https://docs.canton.network/global-synchronizer/reference/security-configuration.md>
- Target audience of a Splice validator: <https://docs.canton.network/global-synchronizer/deployment/validator-kubernetes.md>

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
| `wormhole_canton_foreign_registrar_messages_dropped_total` | The value increases. Treat it as a security incident: `gg` authority signed an `Emitter` with another registrar. |
| Traffic balance of the participant | **Open item** |
| Number of `gg` hosts online | **Open item** |
| `ACS_COMMITMENT_MISMATCH` in the participant log | Each occurrence after the onboarding flag of a host is clear (Section 4.4). |
| `SYNC_SERVICE_ALARM` "Mediator approved a request that has been locally rejected" | Each occurrence. Treat it as a security incident (Section 4.4). |
| "with failed activeness check is approved" in the participant log | Each occurrence. Treat it as a security incident (Section 4.4). |

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
It can also move other holdings that `gg` owns.
Message fees are not `gg` holdings: `feeCustody` holds them (decision U5).
The proposed fixes are:

- A dedicated custody party, separate from the `gg` mint authority.
- A separate mint authority party, or a `CoinFactory` that a second party also signs.
- An allowlist of registry templates for quorum custody.

Until a decision, do not vet a third-party registry package for `gg` custody.

The LocalNet rehearsal of the dedicated custody party:

- [`custody_host.sh`](../canton/localnet/custody_host.sh) allocates `nttCustody` in the `gg` namespace.
  `nttCustody` has the hosts, the confirmation threshold and the owner keys of `gg`.
- [`custody_authority_check.sh`](../canton/localnet/custody_authority_check.sh) shows that `gg` and `nttCustody`
  have separate Daml authority.
- [`gg_rotate_owner_key.sh`](../canton/localnet/gg_rotate_owner_key.sh) rotates the owner keys of the two parties
  in one run (Section 7.4).

The custody party separates Daml authority. It does not separate key authority.
Two owner keys and two confirming hosts can move each holding of `nttCustody` without a VAA.

**U4: owner key threshold of `nttCustody` and `feeCustody`.**
Each custody party uses the owner keys and the threshold of `gg`: 2 of 3 on LocalNet.
The guardian quorum for a VAA is 13 of 19.
Decide the relation of the owner key threshold to the guardian quorum for each custody party before MainNet.
In a complete owner key rotation, `gg` and the namespace accept the removed key for up to about 1 s
after the custody party changes (Section 7.4).

**U5: fee custody.**
`wormhole-core` 0.8.3 adds `feeCustody` as the fee recipient and a `CoreState` signatory.
The fee registry code runs on each paid charge with the authority of the allocation controllers.
In 0.8.0 to 0.8.2, `gg` was the fee recipient. Thus the fee registry code ran with the authority of `gg`.
With that authority, it can publish a message on each `gg`-owned `Emitter`, and the guardians sign it.
In 0.8.3, the fee registry code runs only with the authority of the payer and `feeCustody`.
When a fee is due, `chargeFee` rejects `gg` as payer or receiver, and `gg` or `feeCustody` as fee instrument admin.
Thus an NTT path where `gg` pays a fee fails when the fee is above 0.
Such a path must have a separate fee payer before any fee goes above 0 (Section 7.8).
The custody party separates Daml authority. It does not separate key authority.
Two owner keys and two confirming hosts can move each holding of `feeCustody` without a VAA.
Follow-up: a transfer with a receiver preapproval gives no custody authority at charge time (Token Standard V2).

**U6: hosting of `feeCustody`.**
Decision on 2026-11-23: `feeCustody` stays a `CoreState` signatory.
Thus the hosts of `feeCustody` confirm each `PublishMessage`, each VAA check and each governance transaction.
Each custody party always has the hosts, the confirmation threshold and the owner keys of `gg` (Section 2).
Then `feeCustody` adds no liveness dependency to the `gg` hosts.
A hosting procedure or a key procedure changes the three parties together (Sections 4.3, 4.4, 7.4 and 7.7).
The last step of each procedure is the custody hosting comparison with `gg_show_topology.sh`.
The alternative is a pull model, in which `feeCustody` is not a `CoreState` signatory.
That alternative stays an option if the hosting of `feeCustody` must be different from `gg`.

**U3: the two-id window for a core package upgrade.**
The one-id procedure (Section 6.7) has a signing outage from step 5 to step 14 and a coordinated restart.
The watcher accepts an optional retiring package ID with a record-time deadline.
The flags are `--cantonRetiringPackageID`, `--cantonRetiringPackageVersion` and `--cantonRetiringPackageAcceptedUntil`.
With these flags, the watcher accepts two package IDs until the deadline.
Then each restart is a rolling restart, and messages under the new package ID get VAAs without reobservation.
Section 7.5 of `canton/README.md` gives the two-id window procedure.
The two-id window applies only to an upgrade that keeps the signatories and observers of each template.
The decision is when the guardians can use the two-id window in place of the one-id procedure.
Until a decision, use the one-id procedure in Section 6.7, and do not set the retiring flags.

### 10.2 Open items

| Item | Section |
| - | - |
| Sponsor super validator for each guardian | 3.1 |
| KMS provider for guardian participants | 3.4 |
| Production commands for party hosting | 4.1 |
| Confirmation threshold of `gg` for MainNet | 4.1 |
| Recovery of an honest host after a forged approval | 4.4 |
| Signed comparison of snapshot digests before an import | 4.4 |
| LocalNet rehearsal of `Observation`-first onboarding with the onboarding flag | 4.4 |
| Removal of stale party contracts from a former host | 7.7 |
| Threshold k of n of the owner keys for MainNet | 5.1 |
| Production tool to decode and hash prepared transactions | 5.3 |
| Location, writer and authentication of each trust source | 6.0 |
| `ContractUpgrade` governance VAA before a core package upgrade | 6.0 |
| Automatic vetting of Amulet on guardian participants | 6.2 |
| LocalNet run of the core upgrade abort procedure | 6.7 |
| Rehearsals with `wormhole-core` 0.8.3 | 7.5 |
| Mapping from the 32-byte `recipient` to a Canton party | 7.6 |
| Procedure for a participant key or OAuth2 client secret compromise | 7.7 |
| Reobservation period and participant pruning | 8.1 |
| Alert values for the traffic balance and the number of `gg` hosts | 8.3 |
| Persistent offset for the watcher | 8.4 |
| Check of the inbound rate limits of each peer chain | 9.1 |
| F4: expiry of idle Canton Coin custody pots | 9.2 |
| U1: `gg` custody of third-party token registries | 10.1 |
| U4: owner key threshold of `nttCustody` and `feeCustody` | 10.1 |
| U5: fee payer of NTT paths where `gg` pays, before any fee above 0 | 7.8, 10.1 |
| Owner key rotation of a party that holds reserves | 7.4 |
| U3: a set of package IDs in the watcher for the upgrade period | 10.1 |

### 10.3 Resolved items

| Item | Resolution |
| - | - |
| An `Observation` host and the confirmation threshold | An `Observation` host does not count. After the promotion to `Confirmation`, it counts. LocalNet, 2026-11-19 (Section 4.4). |
| Multi-participant hosting of `gg` | Passed live on 2026-11-17 with `wormhole-core` 0.6.0 on three LocalNet participants at `Confirmation` with threshold 2 (Section 2.3). |
| Vetting on a subset of hosts | Rejected at prepare. Each host must vet. Passed live on 2026-11-17 with `wormhole-core` 0.6.0 (Section 6.3). |
| Mandatory `gg` party in the watcher | `--cantonReadAsParty` is mandatory outside unsafe dev mode (Section 8). |
| Owner key rotation | Passed live on 2026-11-17 with `wormhole-core` 0.6.0, with recovery from a half-applied rotation (Section 7.4). |
| Guardian set rotation to three guardians | Passed live on 2026-11-17 with `wormhole-core` 0.6.0, with the below-quorum rejection (Section 7.3). |
| Guardian set expiry boundary | The check compares whole seconds, `now <= expiry`, as on EVM (Section 7.3). |
| `TxID` on different participants | The `TxID` is the update ID, the same on each participant (Section 8.1). |
| U2: emitter address uniqueness against a compromised operator | `gg` co-signs `EmitterRegistry` and `Emitter` from `wormhole-core` 0.8.0. From 0.9.0, `gg` is the only signatory of `EmitterRegistry` (Section 7.2). |
| Operator dependency | From `wormhole-core` 0.9.0, each Wormhole function runs with the operator participant disconnected. LocalNet, `gg_operator_independence.sh` (Section 7.10). |
| Confirmation threshold of a multi-host `gg` | The devnet setup script requires a majority of the hosts (Section 4.1). |
| Contracts of the old core version after the unvet | They work under the new version. LocalNet rehearsal of 0.6.0 to 0.7.0 at commit `395103f5e` (Section 6.7). |
| LocalNet rehearsal of the core package upgrade | Passed for 0.6.0 to 0.7.0 at commit `395103f5e`, before the maximum canary time and the abort procedure (Section 6.7). |
