# Canton Guardian Operations

This document gives the operation responsibilities of a Wormhole guardian on the Canton Network.
It is a draft. Items with the label **Open item** or **Decision** need a decision before MainNet.

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
| Configure and monitor the Canton watcher in `guardiand` | 8 | Continuous |

## 2. Parties

Each guardian participant hosts one Wormhole party: `guardianGovernance` (`gg`).
No other guardian party exists.

| Property | Value |
| - | - |
| Hosting permission | `Confirmation` on every guardian participant. Never `Submission`. |
| Confirmation threshold | Greater than 1. One participant cannot confirm for `gg` alone. |
| Owner keys | One Ed25519 key for each guardian. A threshold of owner keys controls the `gg` namespace. |
| Signatory of | `CoreState` and every NTT manager. |
| Observer of | Every `Emitter`. Thus guardians see each `PublishMessage`. |
| Other roles | Fee recipient. Mint authority for Wormhole-administered tokens. |
| Watcher | `guardiand` reads the update stream as `gg` (`--cantonReadAsParty`). |

The `operator` party is not a guardian party. Wormhole Labs operates it.

### 2.1 Why the hosting permission is a MainNet gate

A host at `Submission` permission can submit a transaction for `gg` alone.
Thus a host at `Submission` removes the need for the owner key threshold.
A confirmation threshold of 1 lets one compromised participant confirm for `gg`.
Before MainNet, make sure that each guardian participant hosts `gg` at `Confirmation`.
Also make sure that the confirmation threshold is greater than 1.

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

As of 2026-11-16, MainNet software is Splice 0.10.2 on Canton 3.7.3.
LocalNet uses the same versions.
Refer to the Version Compatibility Dashboard for current versions.

### 3.3 Logical Synchronizer Upgrades

The super validators upgrade the protocol version through a Logical Synchronizer Upgrade (LSU).
An LSU has a topology freeze time and an upgrade time.

- Between the freeze time and the upgrade time, no topology transaction is possible.
  Thus, you cannot vet packages, change party hosting or change owner keys in that period.
- A transaction that you prepare before the upgrade time fails if you submit it after the upgrade time.

1. Find the LSU events on the SV Operations Schedule.
2. Do not plan a `gg` ceremony in a freeze window.
3. Do not plan a vetting change in a freeze window.
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

1. Get the `guardianGovernance` party ID from the Wormhole Canton deployment record.
2. Sign the `PartyToParticipant` proposal for your participant at `Confirmation` permission.
3. Make sure that the proposal does not give `Submission` permission to a participant.
4. Make sure that the proposal sets a confirmation threshold greater than 1.
5. Grant the watcher Ledger API user `CanReadAs` for `guardianGovernance`.
6. Vet each package on the allowlist (Section 6) before genesis.
7. Show the topology on your participant.
8. Make sure that each guardian participant is a host at `Confirmation`.

A change to the `gg` hosting mapping needs the `gg` threshold.
Your participant can remove its own hosting at any time.

LocalNet rehearsal: [`gg_host.sh`](../canton/localnet/gg_host.sh) hosts `gg` on three participants.
[`gg_show_topology.sh`](../canton/localnet/gg_show_topology.sh) shows `threshold=2`, three hosts at `Confirmation` and `signingKeys=2-of-3`.

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
- The signer contract is: hex hash in, hex signature out (64-byte Ed25519 signature).
  [`canton/devnet/guardian_key_tool.js`](../canton/devnet/guardian_key_tool.js) shows this contract for local tests only.

1. Generate the owner key in your HSM, KMS or offline signer.
2. Export the public key in DER SubjectPublicKeyInfo format.
3. Send the public key to the genesis coordinator.
4. Keep the private key in the signer. Do not export it.

**Open item:** the threshold k of n for MainNet.
LocalNet uses 2 of 3.

### 5.2 Ceremonies

A ceremony is a request to sign a prepared hash with the `gg` owner key.

| Ceremony | When | Procedure |
| - | - | - |
| Genesis co-signature of `CoreState` | Once | 7.2 |
| `NttGovernance` root creation | Once | 7.1 |
| Owner key rotation (add or remove a guardian) | On guardian set change | 7.4 |
| Fee withdrawal authorization | On request | 7.1 |
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

### 6.1 Rules

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

### 6.4 New `wormhole-core` versions

Each new version of `wormhole-core` has a new package ID.
Example: the change from version 0.6.0 to 0.7.0 gives a new package ID.
The watcher accepts messages only from the package ID in `--cantonPackageID`.
Thus a new core version needs two coordinated changes on each guardian.

1. Get the new package ID from the Wormhole release notes.
2. Check the package ID (Section 6.5).
3. Vet the package ID on your participant (Section 6.6).
4. Wait until each guardian confirms that it vetted the package ID.
5. Set `--cantonPackageID` to the new package ID at the agreed time.
6. Restart `guardiand`.
7. Make sure that the watcher shows messages from the new package ID.

Do not set `--cantonPackageID` to a package ID that your participant did not vet.
Do not unvet the old package ID until no contract uses it.

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

The `operator` and the `gg` owner key threshold sign the `CoreState` together.
After genesis, the operator creates the `EmitterRegistry` and the `ReplayRootRegistry` alone.

1. Make sure that each guardian participant hosts `gg` (Section 4.1).
2. Make sure that each guardian participant vetted the `wormhole-core` package ID.
3. Get the `CoreState` arguments from the coordinator.
4. Make sure that the arguments contain the agreed guardian set.
5. Make sure that the arguments contain the agreed governance chain and governance contract.
6. Sign the prepared hash (Section 7.1).
7. Make sure that your participant shows exactly one `CoreState`.

LocalNet rehearsal: [`gg_genesis.sh`](../canton/localnet/gg_genesis.sh).
The `operator` and two of three owner keys sign the `CoreState`.

### 7.3 Guardian set upgrade on Canton

The `GuardianSetUpgrade` governance VAA changes the guardian set in `CoreState`.
The operator submits it with `SubmitGovernanceVAA`.
The current guardian set must sign the VAA.
The `gg` hosts confirm the transaction. The owner keys do not sign it.

The old guardian set stays valid until its expiry time.
The expiry time is the ledger time of the upgrade plus 86400 seconds.
The old set is valid at exactly the expiry second, as on EVM.
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

LocalNet uses the clock time, thus it does not reach the expiry time.
The Daml Script test `Test.TestCore:testGuardianSetUpgradeExpiry` checks the expiry boundary.

### 7.4 Owner key rotation

An owner key rotation removes one owner key and adds a new owner key.
The threshold does not change.
The rotation has two topology changes in this sequence:

1. The decentralized namespace change.
2. The `PartyToParticipant` signing key change.

Procedure:

1. Make sure that the time is not in an LSU freeze window.
2. Get the public key of the new owner in DER SubjectPublicKeyInfo format.
3. Sign the namespace change with your owner key.
4. Make sure that the namespace contains the new owner key.
5. Make sure that the namespace does not contain the removed owner key.
6. Sign the party signing key change with your owner key.
7. Make sure that the party signing keys contain the new owner key.
8. Make sure that the party signing keys do not contain the removed owner key.
9. Make sure that the threshold did not change.

After the rotation, a signature from the removed key is not valid.
A `gg` transaction with the removed key fails with `FAILED_TO_EXECUTE_TRANSACTION`.
A namespace change that the removed key signs fails with `TOPOLOGY_UNAUTHORIZED_TRANSACTION`.

#### Recovery from a half-applied rotation

If the second change fails, the rotation is half-applied.
The new owner is then in the namespace, but its key cannot sign for `gg`.

1. Do not generate a new key for the new owner.
2. Use the existing key of the new owner.
3. Do the topology changes that are not yet effective.
4. Do steps 7 to 9 of the procedure again.

LocalNet rehearsal: [`gg_rotate_owner_key.sh`](../canton/localnet/gg_rotate_owner_key.sh).
For a half-applied rotation, the script shows `ROTATION HALF-APPLIED`.
The `--resume` flag finishes the rotation with the existing key of the new owner.

### 7.5 Rehearsal evidence

Each LocalNet `gg_*.sh` script writes an evidence log.
The log is in `localnet-state/evidence/` next to the repository.
The first line of the log shows the arguments and the Git commit.

Limits of the rehearsal:

- One container runs all three participants and the synchronizer.
- The owner keys are files on the host, not keys in guardian custody.
- The checks disconnect participants. They do not stop the participant processes.

## 8. Watcher configuration

The Canton watcher in `guardiand` reads the Ledger API of your participant.

| Flag | Value |
| - | - |
| `--cantonRPC` | gRPC endpoint of the Ledger API v2 of your participant. |
| `--cantonPackageID` | Package ID of `wormhole-core`. Mandatory outside unsafe dev mode. Change it on each new core version (Section 6.4). |
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

The watcher uses `UpdateService.GetUpdates` and continues from the last offset after a restart.

### 8.1 Failure behavior

If the participant stops, the watcher does not see new messages.
The guardian then does not sign Canton messages.
This failure is safe: it delays messages, but it does not cause an incorrect signature.

### 8.2 Monitoring

1. Monitor the health of the participant.
2. Monitor the traffic balance of the participant.
3. Monitor the connection of the participant to the synchronizer.
4. Monitor the watcher for errors.
5. Monitor the watcher for a stopped offset.

## 9. Global accountant

The NTT global accountant limits what one chain can send out.
Canton NTT emitters must be in the accountant configuration before MainNet.

**Status:** `node/pkg/accountant/ntt_config.go` has no Canton entries.
This work is in progress separately.

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

**U2: emitter address uniqueness against a compromised operator.**
The operator is the only signatory of `EmitterRegistry`.
Thus a compromised operator can create a new registry that starts its counter at 0.
An honest owner that registers again can then get the address of its first emitter.
Both emitters start at sequence 0, and the guardians sign both messages.
Two VAAs with one message ID then reach quorum.
For NTT, the effect is a transceiver address collision, not a direct mint.
The core red-team finding is A14-3, with the design note `A14_EMITTER_ID_UNIQUENESS.md` on `canton/overnight-core`.
The recommended fix adds `gg` as a signatory of `EmitterRegistry`.
This fix needs a `gg` ceremony, a new package ID and a new `--cantonPackageID` value.

### 10.2 Open items

| Item | Section |
| - | - |
| Sponsor super validator for each guardian | 3.1 |
| KMS provider for guardian participants | 3.4 |
| Confirmation threshold of `gg` for MainNet | 4.1 |
| Threshold k of n of the owner keys for MainNet | 5.1 |
| Production tool to decode and hash prepared transactions | 5.3 |
| Automatic vetting of Amulet on guardian participants | 6.2 |
| Canton entries in the NTT global accountant | 9 |
| U1: `gg` custody of third-party token registries | 10.1 |
| U2: emitter address uniqueness against a compromised operator | 10.1 |

### 10.3 Resolved items

| Item | Resolution |
| - | - |
| Multi-participant hosting of `gg` | Rehearsed on three LocalNet participants at `Confirmation` with threshold 2 (Section 2.3). |
| Vetting on a subset of hosts | Rejected at prepare. Each host must vet (Section 6.3). |
| Mandatory `gg` party in the watcher | `--cantonReadAsParty` is mandatory outside unsafe dev mode (Section 8). |
| Owner key rotation | Rehearsed, with recovery from a half-applied rotation (Section 7.4). |
| Guardian set expiry boundary | The old set is valid at exactly the expiry second, as on EVM (Section 7.3). |
