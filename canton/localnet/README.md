# Canton LocalNet for the Wormhole core bridge

This directory starts a local Canton Network with Docker.
The network uses Splice 0.10.2 on Canton 3.7.3.
DevNet and MainNet use the same versions.

## Software versions

| Item | Value |
| --- | --- |
| Splice release | 0.10.2, commit `e214318836feb7cee420dee33e85c7a6abbc7c04` |
| Compose files | `cluster/compose/localnet` from that Splice commit |
| Canton | 3.7.3 (from `GET /v2/version`) |
| Protocol version | 37 |
| Daml-LF versions | 2.1, 2.2, 2.3, 2.4 |
| Image digests | `images.lock` and `compose.digests.yaml` |

The scripts use the LocalNet files from the Splice release.
The `cn-quickstart` copy of these files is older than Splice 0.10.2.

## Network topology

One `canton` container runs three participants and the global synchronizer.

| Validator | Role | gRPC Ledger API | JSON Ledger API | Validator API |
| --- | --- | --- | --- | --- |
| `sv` | Super Validator, synchronizer owner | 4901 | 4975 | 4903 |
| `app-provider` | Validator | 3901 | 3975 | 3903 |
| `app-user` | Validator | 2901 | 2975 | 2903 |

The ports bind to 127.0.0.1 only.
`env.sh` sets `HOST_BIND_IP` to 127.0.0.1.
The scripts ignore a `HOST_BIND_IP` value from the shell environment.
PostgreSQL uses host port 5440.
Set `DB_PORT` to use a different port.

## Files

| File | Purpose |
| --- | --- |
| `env.sh` | Shared settings. The other scripts read this file. |
| `compose.digests.yaml` | Pins each image to its digest. |
| `images.lock` | Image names, tags and digests. |
| `image_lock_lib.sh` | Compares the images that compose resolves with `images.lock`. `start.sh` uses it. |
| `start.sh` | Starts LocalNet and waits for the Ledger APIs. Stops first if an image is not in `images.lock`. |
| `stop.sh` | Stops LocalNet. |
| `token.sh` | Prints a JWT for the Ledger API. |
| `upload_dars.sh` | Uploads the core DAR and the test DAR. Vets all their packages. |
| `create_script_user.sh` | Creates the user that Daml Script uses. |
| `run_smoke.sh` | Runs the smoke script. |
| `daml/` | The smoke script package `wormhole-localnet-smoke`. |
| `canton_console.sh` | Runs one or more Canton console scripts as one script against the three participants. Stops it after `CANTON_CONSOLE_TIMEOUT_SECONDS` (default 600). |
| `bounded_run.sh` | Runs a command with a timeout. `canton_console.sh` uses it. |
| `console_jar_from_image.sh` | Copies the console jar from the canton image of `images.lock` and checks its SHA-256. |
| `test/` | Script tests. They do not need Docker. |
| `console/` | Console configuration and console scripts. |
| `console/owner_signing_prelude.canton` | Shared console code of the owner-key scripts: external signer, owner keys, guardian policy checks. |
| `console/owner_key_rotation_lib.canton` | Decisions of the owner-key rotation: party order, safety stop, outcome, stored signed changes. |
| `owner_key_input_checks.sh` | Checks owner names and the custody artifacts file. Lists active rotation faults. |
| `interactive_submit.js` | Prepares, signs and executes one external-party submission. LocalNet only. |
| `gg_lib.sh` | Shared functions for the `gg_*.sh` scripts. |
| `gg_host.sh` | Hosts `guardianGovernance`. Allocates `LabsOperator` on `app-provider`. Allocates three local core parties on `sv`. Vets the core DARs on all participants. |
| `gg_offline_lib.sh` | Builds the genesis offer, the `AcceptGenesis` argument and the governance relay command. `test/gg_offline_lib_test.sh` tests it. |
| `gg_operator_independence.sh` | Runs the operator independence rehearsal: genesis by offer and one acceptance, then each Wormhole function with `app-provider` disconnected. |
| `gg_show_topology.sh` | Shows the namespace and the hosting on each participant. |
| `gg_genesis.sh` | Creates the co-signed `CoreState` (fee recipient `feeCustody`) and `EmitterRegistry`, then the `ReplayRootRegistry`. |
| `gg_publish_and_govern.sh` | Publishes one message. Applies one governance VAA. |
| `gg_confirmation_quorum.sh` | Checks the confirmation threshold with two hosts offline. |
| `gg_package_vetting.sh` | Checks the package vetting gate. |
| `vetting_probe/` | Versions 1.0.0 and 2.0.0 of the probe package for the vetting check. |
| `gg_guardian_set_rotation.sh` | Rotates the Wormhole guardian set. |
| `gg_rotate_owner_key.sh` | Replaces one owner key of the guardian namespace, for each party in the namespace. |
| `gg_rotation_fault_check.sh` | Rehearses the rotation faults R1 to R7. |
| `gg_rotation_safety_stop_check.sh` | Rehearses the rotation safety stop SS1 to SS5. |
| `custody_host.sh` | Allocates `feeCustody` or `nttCustody` (`CUSTODY_PARTY_NAME`) under the `guardianGovernance` namespace. |
| `custody_authority_check.sh` | Checks that `guardianGovernance` and one custody party (`--party`) have different authority. |
| `ntt_e2e.sh` | Runs the NTT steps below in sequence. |
| `ntt_upload_dars.sh` | Uploads the NTT DAR to all participants and vets its unvetted packages. |
| `ntt_governance.sh` | Records the `NttGovernance` root and the canonical `CoinFactory` that `AcceptGenesis` created. |
| `ntt_burn_mint.sh` | Runs a burn/mint deployment of a `guardianGovernance` token. |
| `ntt_lock_unlock.sh` | Runs a lock/unlock deployment of Canton Coin. |
| `ntt_lock_unlock_gg.sh` | Registers a lock/unlock deployment of Canton Coin with `guardianGovernance` as the admin. |
| `gateway_t1.sh` | Runs the disclosure service on a separate gateway network behind a mutual-TLS proxy, and checks it. |
| `registry_api_check.sh` | Checks the CIP-0056 registry API of the disclosure service with an offer transfer and a direct transfer. |
| `ntt_watcher.sh` | Runs the guardian watcher harness and checks its observations. |
| `ntt_lib.sh` | Shared functions for the `ntt_*.sh` scripts. |
| `ntt_offline_lib.sh` | NTT functions that `test/ntt_lib_test.sh` tests without LocalNet. |
| `allocate_external_party.js` | Allocates a 1-of-1 Ed25519 external party. |
| `watcher_harness_lib.sh` | Shared functions that start and stop the guardian watcher harness. |
| `guardian4.sh` | Starts or removes the temporary participant `guardian4` and its PostgreSQL container. |
| `party_replication.sh` | Connects `guardian4`. Replicates `guardianGovernance` and `nttCustody` to `guardian4` offline. Removes a host. Sets the confirmation threshold. Sets a host permission. Compares each hosting change with the intended change before the owner keys sign it. |
| `gg_hosting_change_check.sh` | Checks the hosts, the threshold and the ACS of `guardian4` after a hosting change. |
| `gg_hosting_change_live_check.sh` | Checks that a `PublishMessage` commits with `guardian4` as a confirmer, also with `app-provider` disconnected. |
| `gg_hosting_change_negative_check.sh` | Checks that the synchronizer rejects a hosting change with one owner key or without host consent. |
| `gg_observation_onboarding_check.sh` | Checks that an Observation host stays below the threshold, and counts after a consented promotion to Confirmation. |
| `forge_coin_contract.sh` | Makes invented `Coin` contracts with valid contract IDs for the snapshot forgery test. LocalNet only. |
| `snapshot_forgery_poc.sh` | Runs the snapshot forgery test: import, attack, controls, watcher. LocalNet only. |

## Script tests

The script tests do not need Docker or LocalNet.
Run the tests from the repository root.
The loop runs each test in `canton/localnet/test` except the two console tests:

```bash
for script_test in canton/localnet/test/*_test.sh; do
  case "$script_test" in */console_*) continue ;; esac
  "$script_test"
done
canton/tools/publish_message_scan_test.sh
```

The console tests need the Canton console jar in `../vendor/canton-3.7.3/`, or in `$CANTON_VENDOR_DIR/canton-3.7.3/`.
They do not need LocalNet or Docker. They use `console/test/offline-participant.conf`.
If the jar is missing, the tests stop. They do not copy the jar from Docker.
`console_jar_from_image.sh` copies the jar from the canton image of `images.lock` and checks its SHA-256.

```bash
canton/localnet/test/console_scripts_compile_test.sh
canton/localnet/test/console_table_test.sh
```

## Prerequisites

- Docker with the Compose plugin.
- 12 GB of memory for Docker.
- `dpm` with Daml SDK 3.7.3 in `$HOME/.dpm/bin`.
- `curl`, `git` and `openssl`.

## Procedure

1. Build the DARs:

   ```bash
   cd canton
   dpm build --all
   ```

2. Start LocalNet:

   ```bash
   canton/localnet/start.sh
   ```

   The first start clones the Splice compose files into `../vendor/splice-0.10.2`.
   The script stops if the clone is at a different commit.

3. Upload the DARs to the `app-provider` validator:

   ```bash
   canton/localnet/upload_dars.sh app-provider
   ```

4. Run the smoke script:

   ```bash
   canton/localnet/run_smoke.sh app-provider
   ```

   The script creates the core bridge contracts.
   Then it publishes two messages.
   Then it checks the signatures of one guardian-signed VAA.
   The output shows sequences 0 and 1.

5. Stop LocalNet and keep the ledger data:

   ```bash
   canton/localnet/stop.sh
   ```

6. To delete the ledger data, add the flag:

   ```bash
   canton/localnet/stop.sh --delete-data
   ```

## Authentication

LocalNet accepts HS256 tokens that use the development secret `unsafe`.
The audience is `https://canton.network.global`.
The default subject is `ledger-api-user`.
This user is an administrator on each validator.

Print a token:

```bash
canton/localnet/token.sh
```

Print a token for a different user:

```bash
canton/localnet/token.sh wormhole-script
```

NOTE: Use the `unsafe` secret on LocalNet only.

The scripts give the token to `curl` in a header file with mode 0600.
The token is thus not in the process arguments.

## Daml Script user

The administrator `ledger-api-user` cannot submit commands for new parties.
`create_script_user.sh` creates the user `wormhole-script` for Daml Script.
This user has the rights `ParticipantAdmin`, `CanActAsAnyParty` and `CanReadAsAnyParty`.

Run `Test.TestCore:setup` as this user:

```bash
token_file="$(mktemp)"
canton/localnet/token.sh wormhole-script > "$token_file"
dpm script \
  --dar canton/test/.daml/dist/wormhole-core-test-0.1.0.dar \
  --script-name Test.TestCore:setup \
  --ledger-host localhost \
  --ledger-port 3901 \
  --access-token-file "$token_file"
rm -f "$token_file"
```

`mktemp` makes the token file with mode 0600.

`Test.TestCore:setup` gives the same party IDs on each run.
Thus it runs only one time on each participant.
`run_smoke.sh` adds a run label to each party name, so you can run it again.

## Package names

Each package name and version can have only one package ID on a participant.
If you change the smoke script, increase its version in `daml/daml.yaml`.
A new version must be a valid upgrade of the old version.

## guardianGovernance rehearsal on three participants

These scripts rehearse the production topology of `guardianGovernance`.
The party is a decentralized-namespace external party.
Three Ed25519 owner keys control it, with a threshold of 2.
All three participants host it at `Confirmation` permission.
The confirmation threshold is 2.

The scripts keep keys, artifacts and contract ids in `../localnet-state`.
Set `LOCALNET_STATE_DIR` to use a different directory.
The console scripts use the Canton 3.7.3 console jar from the `canton` container.
`canton_console.sh` copies the jar to `../vendor/canton-3.7.3` on the first run.

NOTE: The owner keys are files on the host. Production keeps them in guardian custody.

WARNING: `interactive_submit.js` is not a production signer.
It signs the transaction hash that the participant sends.
It does not calculate the hash from the prepared transaction.
Thus it trusts the participant.
Use it on LocalNet only.

### Evidence logs

Each `gg_*.sh` script that uses `gg_lib.sh` writes its output to a log file.
The log file is `../localnet-state/evidence/<script>-<UTC time>.log`.
The first line shows the arguments and the Git commit.
The scripts write tokens and private keys only to files with mode 0600. Thus you can share the logs.
Keep the logs as the evidence of each run.

### Prerequisites

- LocalNet is running.
- You built the DARs. Refer to the procedure above.
- `node` 18 or later (`interactive_submit.js` uses the built-in `fetch`), `jq`, `java` 21 and `go` are on the `PATH`.

### Host the parties and run genesis

1. Host `guardianGovernance` and allocate the local parties:

   ```bash
   canton/localnet/gg_host.sh
   ```

   The script uploads the core DARs to `app-provider`, `sv` and `app-user`.
   Each participant vets the DARs.
   It hosts `guardianGovernance` on the three participants.
   It allocates the local party `LabsOperator` on `app-provider`. `LabsOperator` is the operator: it offers genesis
   and it is the onboarding fee admin. It has no signing key.
   It allocates the local parties `CorePublisher`, `CoreVerifier` and `GovernanceRelayer` on `sv`.
   The script writes `../localnet-state/local-parties.json`.

2. Allocate `feeCustody` (the `CoreState` fee recipient) and `nttCustody` (the NTT reserve custodian) before genesis:

   ```bash
   CUSTODY_PARTY_NAME=feeCustody canton/localnet/custody_host.sh owner1 owner2 owner3
   CUSTODY_PARTY_NAME=nttCustody canton/localnet/custody_host.sh owner1 owner2 owner3
   ```

   The script writes `../localnet-state/fee-custody-artifacts.json` and `../localnet-state/custody-artifacts.json`.
   Each party has the hosts, the confirmation threshold and the signing keys of `guardianGovernance`.

3. Show the topology on each participant:

   ```bash
   canton/localnet/gg_show_topology.sh
   ```

   Each participant shows `threshold=2`, three hosts at `Confirmation` and `signingKeys=2-of-3`.

4. Upload the `ntt` DAR and the `wormhole-genesis` DAR to each participant. The script vets their packages:

   ```bash
   NTT_REPO_DIR=<NTT checkout> canton/localnet/ntt_upload_dars.sh
   ```

5. Run genesis:

   ```bash
   canton/localnet/gg_genesis.sh
   ```

   `LabsOperator` creates one `WormholeGenesisOffer` on `app-provider`.
   The script builds the agreed parameters itself and compares them with the offer that `sv` shows.
   The genesis guardian set index is `GENESIS_GUARDIAN_SET_INDEX` (default `0`).
   Before the commit, these `AcceptGenesis` transactions must fail:
   - `guardianGovernance` with one owner key.
   - No `nttCustody` signature.
   - `agreedParameters` with another guardian set index (`genesis: agreed parameters differ from the offer`).

   Then two owner keys for each of `guardianGovernance`, `feeCustody` and `nttCustody` sign one `AcceptGenesis`.
   It creates `CoreState`, `EmitterRegistry`, `ReplayRootRegistry`, `NttGovernance` and the `guardianGovernance`
   `CoinFactory`, and archives the offer. `feeRecipient` is `feeCustody`.
   The script writes the root contract IDs to `../localnet-state/core-contracts.json`.
   Then it creates the Ledger API user `wormhole-watcher` on each participant.
   This user has only the right `CanReadAs(guardianGovernance)`.
   `canton/tools/emitter_id_scan.sh` needs a token of this user on each participant that it reads.

   Each committed submission that guardian namespace parties sign adds one line to
   `../localnet-state/gg-ceremonies.jsonl`.

6. Publish one message and apply one governance VAA:

   ```bash
   canton/localnet/gg_publish_and_govern.sh
   ```

   The argument is the key indices of the current guardian set. The default is 0.
   `CorePublisher` registers an `Emitter` and publishes. `GovernanceRelayer` relays the governance VAA.
   Each submits on `sv` as a local party, with the disclosures that `sv` reads as `guardianGovernance`.
   The governance VAA sets the message fee to 1000.
   All three participants show the new `CoreState`.

### Operator independence rehearsal

Run it on a reset LocalNet after steps 1 and 2 above. The script runs steps 4 and 5 itself.

```bash
NTT_REPO_DIR=<NTT checkout> canton/localnet/gg_operator_independence.sh 0 1
```

The arguments are the key indices of the current guardian set and of the next set.
The script prints `PASS step <n>` for each of 21 steps:

| Step | Action | Expected |
| - | - | - |
| 1, 2 | `LabsOperator` is local to `app-provider` only. Each participant vets `wormhole-core`, `ntt`, `wormhole-genesis` and `token-cip0056`. | No `gg` ceremony. |
| 3, 4 | `gg_genesis.sh`. | One `gg` ceremony. `AcceptGenesis` archives the offer. No root names `LabsOperator`. |
| 5 | Disconnect `app-provider`. | Disconnected. |
| 6, 7 | `CorePublisher` registers an `Emitter`. Then it publishes at fee 0. | Signatories `CorePublisher` and `gg`. |
| 8 | The watcher harness on `sv` reads as `gg`. | One observation with the `Emitter` address. |
| 9 to 11 | `CoreIntegrator` claims a replay root and consumes a VAA, then the same VAA again. | Commit, commit, `digest already consumed`. |
| 12, 13 | `GovernanceRelayer` relays `SetMessageFee` 0 and `TransferFees`. | `actingParties` is `GovernanceRelayer`. One `FeeWithdrawalAuthorization` with no observer. |
| 14 to 17 | NTT on `app-user`: `RegisterManager` (lock/unlock of Canton Coin), `SetPeer`, `Transfer`, then `Release` by a relayer on `sv`. | Commit. The watcher observes the `Transfer` message. |
| 18 | `GovernanceRelayer` relays `GuardianSetUpgrade`. | The next guardian set index. |
| 19 | `LabsOperator` submits `SetRegistrationFee`. | No commit: its participant is offline. The fee stays. |
| 20 | Reconnect `app-provider`. Disconnect `sv` and `app-user`. Publish from `app-provider`. | `MEDIATOR_SAYS_TX_TIMED_OUT`. The unresponsive parties are exactly `guardianGovernance` and `feeCustody`. |
| 21 | Count the `gg` submissions since step 1 in two ways. (a) The ceremony log. (b) The transactions on `sv` that show a command ID to `gg`. | (a) 1, (b) 1: the `AcceptGenesis` transaction. |

The EXIT trap stops the watcher harness and connects each participant again.
After the script, the guardian set is the next set and the message fee is 0.
Give the key indices of the next set to the scripts that sign VAAs, for example `gg_publish_and_govern.sh 1`.

### Check the confirmation threshold

1. Run the check:

   ```bash
   canton/localnet/gg_confirmation_quorum.sh
   ```

   The argument is the key indices of the current guardian set. The default is 0.
   After a rotation to a new set, give the key indices of that set, for example `1`.
   The script disconnects `app-provider` and `app-user` from the synchronizer.
   Only `sv` can then confirm for `guardianGovernance`. The local submitters are on `sv`.
   A governance VAA, a VAA check and a publish each fail with `MEDIATOR_SAYS_TX_TIMED_OUT`.
   Each failure must name exactly `guardianGovernance` and `feeCustody` as the unresponsive parties.
   With `app-user` connected again, the governance VAA succeeds.
   It sets the message fee to 0 again.
   At the end, the script connects all participants again.

2. After each connection change, the script reads the connection state of each participant.
   If a participant has the incorrect state, the script stops with an error.
   If a participant stays disconnected at the end, the script shows `ERROR` and a recovery command.

3. If the script shows the recovery command, run it:

   ```bash
   PARTICIPANT_ACTION=reconnect PARTICIPANT_NAMES=app-provider,app-user \
     canton/localnet/canton_console.sh canton/localnet/console/participant_connection.canton
   ```

NOTE: The mediator timeout uses sequencer time.
With two participants offline, sequencer time can stop.
The script requests time proofs from `sv` to move the time.

CAUTION: Without the time proofs, a transaction can wait for many minutes.
A host that connects again in that period can still confirm it.

4. After `custody_host.sh` and `gg_package_vetting.sh`, do the check for `nttCustody`, or for `feeCustody` with `fee-custody-artifacts.json`:

   ```bash
   canton/localnet/gg_confirmation_quorum.sh --party "$(jq -r .partyId ../localnet-state/custody-artifacts.json)"
   ```

   With `app-user` disconnected, an `nttCustody` transaction succeeds with two confirming hosts.
   With `app-provider` and `app-user` disconnected, it fails with `MEDIATOR_SAYS_TX_TIMED_OUT`.
   The failure must name `nttCustody` as the only unresponsive party.
   Give `--signing-owners <owner>,<owner>` after a rotation that removes `owner1` or `owner2`.
   The `--party` check uses the probe package that `gg_package_vetting.sh` uploads.

### Check the package vetting gate

1. Run the check:

   ```bash
   canton/localnet/gg_package_vetting.sh
   ```

   Only `app-provider` vets version 2.0.0 of the probe package.
   A create on 2.0.0 fails with `INVALID_PRESCRIBED_SYNCHRONIZER_ID`.
   The error must name `sv` and `app-user` with `has not vetted`.
   A create by package name selects version 1.0.0.
   Then `sv` also vets 2.0.0.
   Two of three hosts is equal to the confirmation threshold.
   The create on 2.0.0 fails again, and the error must name only `app-user`.
   Thus each host must vet the package, not only the threshold number of hosts.
   With 2.0.0 vetted on all participants, the create on 2.0.0 succeeds.

### Rotate the Wormhole guardian set

1. Rotate from the current set to a set of devnet key 1:

   ```bash
   canton/localnet/gg_guardian_set_rotation.sh 0 1
   ```

   The arguments are the key indices of the current set and of the new set.
   Use commas between the indices of a set with more than one guardian.
   The keys come from `node/pkg/devnet`.
   `node/hack/canton_localnet_vaa` makes and signs the VAAs.

2. Make sure that the output shows these results:
   - Each member of the new set has a public key after `AttachGuardianPublicKeys`.
   - A VAA of the new set passes `ParseAndVerifyVAA`.
   - A VAA of the old set passes until the expiry time.
   - A governance VAA of the old set fails with `governance: not signed by current guardian set`.
   - A VAA of the new set with an old key at index 0 fails with `invalid signature for guardian index 0`.

NOTE: The rotation from set 0 to set 1 uses sets of one guardian.
The quorum of a one-guardian set is 1.
Thus this run does not check the quorum calculation.

3. To check the quorum calculation, rotate from set 1 to a set of three guardians:

   ```bash
   canton/localnet/gg_guardian_set_rotation.sh 1 2,3,4
   ```

   The quorum of a three-guardian set is 3.
   A VAA with signatures from two guardians of the new set fails with `no quorum: have 2, need 3`.
   The script uses the `-signer-positions` flag of `canton_localnet_vaa` for this VAA.

The expiry time of the old set is the ledger time of the upgrade plus 86400 seconds.
The old set is valid at the expiry time and not valid one second after it.
LocalNet uses the clock time, thus the expiry does not occur during the check.
The Daml Script test `Test.TestCore:testGuardianSetUpgradeExpiry` moves the time and checks the expiry.

### Replace an owner key of the guardian namespace

The owner keys are the namespace owners and the signing keys of each party in the namespace.
The parties are `guardianGovernance`, `feeCustody` and, after `custody_host.sh` for it, `nttCustody`.
Thus a rotation changes the namespace and the `PartyToParticipant` mapping of each party in the namespace.

The rotation has these phases:

1. Safety stop. The script reads the namespace and each `PartyToParticipant` mapping in it on each participant.
   It stops before any change if one of these conditions is true:
   - Two participants show a different namespace definition, a different mapping or a different serial.
   - A party in the namespace has a `PartyToKeyMapping`.
   - The hosts of a party in the namespace are not exactly the participants of the console configuration.
   - A party in the namespace is not in the rotation list.
   - A party in the rotation list is not in the namespace.
   - The signing keys of a party are not the old key set or the new key set.
   - The namespace owners are not the old key set or the new key set.

   The script calculates each intended change from the views of the safety stop, which all participants agree on.
   Thus the hosts, the thresholds and the serials come from all participants. This is true in the `--resume` mode also.
   Each intended mapping must have the guardian policy before the first signer call.
2. Sign phase. `app-provider` generates each change.
   Before the first signer call, the script compares each change with the intended change.
   The operation, the serial, the mapping and the hash must be equal.
   Then each owner signs each change. No change goes to the synchronizer.
   The script keeps the signed changes in `../localnet-state/rotation-<new-owner>.signed.bin`, with mode `0600`.
   If a signer fails, the script shows `ROTATION NOT STARTED`. The topology does not change.
   Before the load, in the fresh mode and in the `--resume` mode, the script checks each stored change:
   - Each `PartyToParticipant` change has the guardian policy.
   - Each change is equal to the change that the agreed topology of the safety stop needs, with the next serial.
   - A change that is already effective is equal to the current mapping, with the current serial.
   - The namespace change has the new owners and the current threshold.
3. Load phase. If the new owner has no root namespace delegation, load 1 adds it.
   This delegation alone gives no authority over the guardian namespace.
   Load 2 is one load call with all other changes: `nttCustody`, `feeCustody`, `guardianGovernance`, then the namespace.
   The script tries load 2 a maximum of 3 times. Each try loads only the changes that a participant does not show.
4. Checks. The script reads all participants again. The views must agree. The script then finds one of these results:
   - All changes are effective. The script shows `ROTATION applied`.
     Then it checks the hosts, the thresholds and the key sets on the agreed views.
     It shows `ROTATION effective times` for the changes and `ROTATION done`.
     The script removes the file of the signed changes after these checks.
   - No change is effective. The script shows `ROTATION NOT APPLIED`. Each mapping keeps its serial.
   - Some changes are effective. The script shows `ROTATION HALF-APPLIED` and the parties that accept the removed key.

The signer of each owner must finish in 30 seconds.
The script stops a signer that runs longer.
The signature must be exactly 64 bytes as 128 lowercase hex characters.

1. Replace `owner3` with a new key `owner4`:

   ```bash
   canton/localnet/gg_rotate_owner_key.sh owner3 owner1 owner2 owner4
   ```

   The arguments are the removed owner, the two kept owners and the new owner.
   The thresholds stay at 2 of 3.

2. Make sure that the output shows these results:
   - `ROTATION parties: nttCustody,feeCustody,guardianGovernance`, or `feeCustody,guardianGovernance` without `nttCustody`.
   - `ROTATION protocol version: 37`.
   - `ROTATION effective times (one load call): nttCustody=<time> feeCustody=<time> guardianGovernance=<time> namespace=<time>`.
   - `ROTATION effective times in load order, spread <seconds> s (bound 30 s)`.
     Canton gives each change of one load call its own effective time, in the load order.
     The script stops if a time is not after the time before it.
     The script also stops if the spread is more than 30 s.
   - `ROTATION removed owner key <fingerprint> is in no key set of the namespace`.
     On each participant, the namespace owners are exactly the new owner set.
     No `PartyToParticipant` mapping of the namespace has the removed key.
     Without a key set, the removed key has no authority. Thus these two checks show the removal.
     An earlier version used a probe that only the removed key signed.
     That probe fails in each state, because the namespace threshold is 2. Thus it did not show the removal.
   - For each party, a transaction with one kept key and the new key succeeds.
   - For each party, a transaction with one kept key and the removed key fails with `FAILED_TO_EXECUTE_TRANSACTION`.
     The error shows 1 valid signature and 1 invalid signature.
   - For each party, a transaction with only the removed key fails with `FAILED_TO_EXECUTE_TRANSACTION`.
     The error shows 0 valid signatures and 1 invalid signature.

WARNING: In a half-applied rotation, the removed key and one kept key can sign for each party in the
`ROTATION HALF-APPLIED` line. If the line names `namespace`, the removed key and one kept key can change the mapping of
each party in the namespace. Do step 3 immediately.

3. If the script shows `ROTATION HALF-APPLIED`, run the script again with `--resume` and the same owner names:

   ```bash
   canton/localnet/gg_rotate_owner_key.sh --resume owner3 owner1 owner2 owner4
   ```

   The script loads the stored signed changes. It calls no signer for them.
   It does not load again the changes that are already effective.
   Then it does the checks of step 2.

4. If the script shows `ROTATION NOT STARTED` or `ROTATION NOT APPLIED`, the removed key stays valid.
   Run the script again with a different new owner name.

5. If the script shows `ROTATION applied` but not `ROTATION done`, a check after the load failed.
   Do not use `--resume`. It does the same checks again.
   Examine the topology with `gg_show_topology.sh`.
   Each change in the file of the signed changes is effective. Remove the file after the examination.

WARNING: After `ROTATION NOT APPLIED`, the file of the signed changes stays.
Each change in it stays valid until a different change takes its serial.
Keep the file secret. The next complete rotation takes each serial, and the file then has no effect.
After a load attempt, `app-provider` also has each signed change. The secrecy of the file does not remove that copy.
Only a later change of each mapping makes the signed changes not valid.
To discard the file, run `gg_show_topology.sh` two times, 2 minutes apart.
If the serials did not change, remove the file.

NOTE: The NTT scripts sign with `owner1` and `owner2`.
After a rotation that removes one of them, set `NTT_GG_SIGNING_OWNERS` to two current owner names.

### Host nttCustody under the guardian namespace

The `nttCustody` party is the custody party of the NTT lock/unlock deployment (NTT PR 75).
In this rehearsal, `nttCustody` holds no token.
It has the same decentralized namespace as `guardianGovernance`.
It has the same hosts, the same confirmation threshold and the same 2-of-3 signing keys.

1. Show the current owners:

   ```bash
   canton/localnet/gg_show_topology.sh
   ```

2. Find the owner name of each fingerprint in `extra-key-fingerprints.json` or `gg-artifacts.json`.

3. Allocate `nttCustody`. Use the three current owner names:

   ```bash
   canton/localnet/custody_host.sh owner1 owner2 owner3
   ```

   The script writes `../localnet-state/custody-artifacts.json`.
   Before the load, the script checks that `guardianGovernance` has the guardian policy.
   Before the first signer call, the script compares the change that `app-provider` generates with the change that it calculates.
   The operation must be `Replace`. The serial must be 1. The mapping and the hash must be equal.
   The script also compares the change that each host returns after its signature.
   After the load, it reads the `nttCustody` mapping back and checks the same policy:
   - Exactly 3 hosts, each with the `Confirmation` permission.
   - A confirmation threshold of exactly 2.
   - Exactly 3 signing keys with a threshold of exactly 2.

4. Check the authority of each custody party. Use two current owner names:

   ```bash
   canton/localnet/custody_authority_check.sh --party nttCustody owner1 owner2
   canton/localnet/custody_authority_check.sh --party feeCustody owner1 owner2
   ```

5. Make sure that the output shows these results:
   - A `guardianGovernance` submission cannot create a contract that `nttCustody` signs.
     The error is `DAML_AUTHORIZATION_ERROR`.
   - An `nttCustody` submission cannot create a contract that `guardianGovernance` signs.
     The error is `DAML_AUTHORIZATION_ERROR`.
   - An `nttCustody` submission with only `guardianGovernance` signatures fails.
   - A submission for the two parties with one pair of owner signatures succeeds.
   - A submission for each party with one owner signature fails with `FAILED_TO_EXECUTE_TRANSACTION`.
     The error shows 1 valid signature and 0 invalid signatures.
   - A submission of one custody party cannot create a contract that the other custody party signs.
     The error is `DAML_AUTHORIZATION_ERROR`.

### Rehearse the rotation faults and the safety stop

Do these rehearsals after `custody_host.sh` and `custody_authority_check.sh`.
Each rehearsal uses new owner names with the prefix `rot-` or `stop-`.
The last line of each rehearsal shows the current owner names.

1. Run the fault rehearsal. Use the current owner names:

   ```bash
   canton/localnet/gg_rotation_fault_check.sh owner3 owner1 owner2
   ```

   The rehearsal runs these cases:

   | Case | Fault | Expected result |
   | - | - | - |
   | R2 | The signer of the new owner fails. | `ROTATION NOT STARTED`. The topology does not change. |
   | R1 | None. | `ROTATION done`. The effective times are in the load order, with a spread of 30 s or less. |
   | R3 | The load omits the `nttCustody` change. | `ROTATION HALF-APPLIED` names `nttCustody`. |
   | R6 | None. Ceremony probes in the R3 state. | `nttCustody` accepts the removed key. `guardianGovernance` accepts the new key. |
   | R3b | `--resume` of R3. The signer of the new owner fails. | `ROTATION done`. `ROTATION signatures in this run: 0`. |
   | R7 | The load omits the `feeCustody` change. Then `--resume`. | `ROTATION HALF-APPLIED` names `feeCustody`. Then `ROTATION done`. |
   | R4 | The `nttCustody` change has no signature of the new owner. | Measurement. |
   | R5 | The root delegation is in load 2. | Measurement. |

2. Record the `MEASURED` lines of R3, R4 and R5 in "Results of the rotation measurements".

3. Run the safety stop rehearsal. Use the current owner names from the last line of step 1:

   ```bash
   canton/localnet/gg_rotation_safety_stop_check.sh rot-r5-<time> owner1 owner2
   ```

   Each stop case must show `SAFETY STOP:`, and the topology must not change:

   | Case | State | Expected result |
   | - | - | - |
   | SS1 | The rotation list has only `guardianGovernance`. | Stop: the parties differ. |
   | SS5 | The rotation list has no `feeCustody`. | Stop: the parties differ. |
   | SS3 | The rotation list has an unknown party. | Stop: the parties differ. |
   | SS2 | A half-applied rotation A. Rotation B starts without `--resume`. | Stop: the `nttCustody` keys are not the old or the new set. |
   | SS4 | `--resume` of rotation A. | `ROTATION safety stop passed`, then `ROTATION done`. |

#### Results of the rotation measurements

Canton 3.7.3 on LocalNet, 2026-11-18, four runs, before `feeCustody` existed:

- R3: on each participant, the namespace and `guardianGovernance` changed. `nttCustody` did not change.
- R4: the participant rejects the complete load call. No mapping changes.
  The root delegation of load 1 stays effective. This delegation alone gives no authority.
- R5: Canton accepts the root delegation and the namespace change in one load call. All mappings change.
- R1 and R5: each change of one load call has its own effective time, in the load order.

| Run | Case | `nttCustody` | `guardianGovernance` | namespace | Spread |
| - | - | - | - | - | - |
| 1 | R1 | 11:52:49.327797Z | 11:52:49.743385Z | 11:52:50.331548Z | 1.004 s |
| 1 | R5 | 11:54:55.628159Z | 11:54:56.097210Z | 11:54:56.458812Z | 0.831 s |
| 2 | R1 | 12:03:54.931426Z | 12:03:55.348410Z | 12:03:55.941646Z | 1.010 s |
| 2 | R5 | 12:05:08.695948Z | 12:05:09.171924Z | 12:05:09.775166Z | 1.079 s |
| 3 | R1 | 13:21:59.218974Z | 13:21:59.640074Z | 13:22:00.231191Z | 1.012 s |
| 3 | R5 | 13:23:15.607646Z | 13:23:16.088348Z | 13:23:16.688899Z | 1.081 s |
| 4 | R1 | 13:53:14.739979Z | 13:53:15.302642Z | 13:53:15.782525Z | 1.043 s |
| 4 | R5 | 13:54:20.798877Z | 13:54:21.274043Z | 13:54:21.874240Z | 1.075 s |

WARNING: In a complete rotation, `guardianGovernance` and the namespace accept the removed key for up to about 1 s
after the `nttCustody` change.

### Limits of the rehearsal

- One container runs all three participants and the synchronizer.
  The check does not show a fault of a separate host or network.
- The checks disconnect participants from the synchronizer.
  They do not stop the participant processes.
- The vetting check fails at the submitting participant.
  It does not show the check on the other hosts for a submission that skips this check.
- Each script changes the ledger state. Run each script in the order of this section.
- Two owner keys and two confirming hosts can create any contract that has `nttCustody` as the only signatory.
  Thus they can move each holding of `nttCustody` through the registry of that holding, without a VAA.
  The VAA check is only in the NTT choices. Case 1 of `custody_authority_check.sh` shows this.
- One owner key pair signs for the two parties in one submission.
  The parties have separate Daml authority, but not separate key authority.
  Case 5 of `custody_authority_check.sh` shows this.
- The rotation safety stop reads the participants of the console configuration only. On LocalNet, these are the three hosts.
  It reads the `global` synchronizer only.
- The console calculates the intended values from the agreed views of all participants of the console configuration.
  A change from `app-provider` that differs from these values fails the comparison before the signature.
  The owners still trust the console for the calculation. A production owner decodes each change (operations document, Section 7.4).

## NTT end-to-end run

These scripts run the Wormhole NTT layer on LocalNet.
The NTT packages come from the NTT repository, branch `canton/migrate-ntt-layer`, commit `b2c355d`.
Set `NTT_REPO_DIR` to the root of that checkout.
The default is `../ntt`, next to this repository.
`ntt_lib.sh` pins the package IDs of `wormhole-core` 0.12.0, `ntt` 0.5.0, `wormhole-genesis` 0.1.0 and `token-cip0056` 0.2.0.
The pinned `ntt` and `wormhole-genesis` DARs call `wormhole-core` 0.9.0. Thus `ntt_upload_dars.sh` rejects them until they are rebuilt on 0.12.0 and pinned (CORE-4095).
`ntt_upload_dars.sh` stops if a DAR has a different package ID.
`ntt_pinned_package_id` in `ntt_offline_lib.sh` holds the package ID of each `ntt` version.
`nttCustody` is a signatory of `NttGovernance` and `NttManager`.
`ntt_lib.sh` reads the `nttCustody` party from `custody-artifacts.json`.
The `guardianGovernance` owner keys of `NTT_GG_SIGNING_OWNERS` also sign for `nttCustody`.
`AcceptGenesis` creates the `NttGovernance` root and the `CoinFactory` (`gg_genesis.sh`). `ntt_governance.sh` records them.

### Prerequisites

- The `guardianGovernance` rehearsal of the previous section ran.
- The NTT DARs exist. Run `dpm build --all` in `canton` of the NTT checkout.
- Guardian set 1 with devnet key 1 is the current guardian set.
  Set `NTT_GUARDIAN_SET_INDEX` and `NTT_GUARDIAN_KEY_INDICES` for a different set.
- The current owner keys include `owner1` and `owner2`.
  Set `NTT_GG_SIGNING_OWNERS` for different owner keys.

### Procedure

1. Run all steps:

   ```bash
   canton/localnet/ntt_e2e.sh
   ```

2. Make sure that the last line of the output is `NTT end-to-end run passed`.

To run one step only, use the step script. Run the steps in the sequence of the table.

| Step | Script | Result |
| --- | --- | --- |
| 1 | `ntt_upload_dars.sh` | Each participant vets each package of the `ntt` DAR, and no `wormhole-core` newer than 0.12.0. |
| 2 | `ntt_governance.sh` | One `NttGovernance` root and one `CoinFactory` exist. |
| 3 | `ntt_watcher.sh start` | The watcher harness reads `app-provider`. |
| 4 | `ntt_burn_mint.sh` | A relayer mints 2.5 tokens to a user. The user burns 1.0 token. |
| 5 | `ntt_lock_unlock.sh` | A user locks 5 CC. A relayer releases 2 CC to a recipient. |
| 6 | `ntt_watcher.sh stop`, `check` and `reobserve` | The watcher observed the two outbound transfers. |

### Burn/mint deployment

- The deployment admin is a new external party on `app-provider`.
- `guardianGovernance` administers the token. The instrument id is bound to the admin.
- The admin and two `guardianGovernance` owner keys sign `RegisterManager`.
- `RegisterManager` publishes `TransceiverInit`. `SetPeer` publishes `TransceiverRegistration`.
- The user on `app-user` creates a `DepositPreapproval`.
- The relayer on `sv` submits `Mint` with a VAA from the current guardian set.
- The script makes sure that `guardianGovernance` is the instrument admin of each minted `Coin`.
- The relayer submits the same `Mint` VAA again. The ledger rejects it with `digest already consumed`.
- The relayer submits a `Mint` VAA that devnet key 9 signs for guardian 0.
  The ledger rejects it with `invalid signature for guardian index 0`.
- The user submits `Transfer`. The canonical `CoinFactory` burns the token.

### Lock/unlock deployment of Canton Coin

- The deployment admin is the custodian. It is a wallet user on `app-provider`.
  `guardianGovernance` is not the custodian.
- The custodian, the locker and the recipient get Canton Coin from the wallet tap.
- The custodian and the recipient create Amulet `TransferPreapproval` contracts with the wallet API.
- The factory and the choice context come from the Amulet registry API of Scan.
- The locker submits `Transfer`. The custodian receives the Canton Coin.
- The relayer on `sv` submits `Release`. The recipient receives Canton Coin through its `TransferPreapproval`.
- The relayer submits the same `Release` VAA again. The ledger rejects it with `digest already consumed`.
- The relayer is not a stakeholder of the recipient holdings.
  `Release` checks the amount on the custodian side.

### Guardian watcher harness

The harness is `node/pkg/watchers/canton/watcher_localnet_test.go`, with the build tag `localnet`.
It makes the watcher with `WatcherConfig.buildWatcher` in the TestNet environment.
`buildWatcher` runs the checks of `Create`.
Thus the harness requires the pinned `wormhole-core` package id, its version and the read-as party.
The harness also checks that `buildWatcher` rejects a configuration without one of them.
The pinned id and version must be in the approved list.
By default, this is `node/pkg/cantonclient/approved_core_packages.json`, and the harness also runs `Create`.
`WATCHER_APPROVED_CORE_PACKAGES_FILE` replaces the approved list of the harness. The file has the same format.
It reads the update stream of `guardianGovernance` on `app-provider`.
`run_harness_on` starts a harness on the Ledger API of a different participant.

The harness authenticates as the Ledger API user `wormhole-watcher`.
`watcher_harness_lib.sh` makes this user on each participant with only the right `CanReadAs(guardianGovernance)`.
The script stops if the user has a different set of rights.
The script removes the token file when the harness streams.

LocalNet uses a static JWT on plaintext gRPC.
The harness injects a client with this transport through `Watcher.cantonClient`.
`WatcherConfig` has no transport field.

Each 500 ms and at the stop, the harness writes the counts of `wormhole_canton_unpinned_publish_messages_total` to `<name>.metrics.json`.
Thus a script can read the counter of a harness that runs.
A script that fails stops each harness that it started.

`ntt_watcher.sh check` compares each live observation with the `WormholeMessage` of the transaction.
Each observation has chain 75.
The emitter address that the watcher calculates is equal to the `emitterAddress` of the transceiver `Emitter`.

`ntt_watcher.sh reobserve` sends a reobservation request for each outbound transfer.
It sends the requests to a harness on `app-provider`, then to a harness on `sv`.

NOTE: The transaction id of the watcher is the update id.
The update id is the same on each participant.

### Limits of the NTT run

- The peer on chain 2 is not a real deployment. The devnet guardian key signs the inbound VAAs.
- By default, the lock and the release occur in the same mining round.
  Set `NTT_RELEASE_AFTER_ROUNDS` (0 to 24) to release from a custody pot that is that number of rounds old.
  Token-standard transfers use the round of `ExternalPartyConfigState`.
  The DSO updates this round one time each 24 hours.
  Thus the custody pot can be from round 0, and then the option has no effect.
- The message fee is 0. The `Transfer` commands do not include a fee allocation.
- Each run makes new deployments. The `NttGovernance` root and the `CoinFactory` stay the same.

## Disclosure service on a separate gateway (T1)

`gateway_t1.sh` runs the NTT disclosure service in its own container.
The service reads the JSON Ledger API of `app-provider` through a mutual-TLS proxy.
A relayer on `app-user` uses only the sets of the service.
The relayer is not a stakeholder of `NttManager`, `CoreState` or `LockedLedger`.

| Container | Networks | Function |
| --- | --- | --- |
| `t1-disclosure-gateway` | `gateway-net` | Disclosure service. Reads as `guardianGovernance` and `nttCustody`. |
| `t1-ledger-proxy` | `localnet`, `gateway-net` | nginx. Requires a client certificate of the T1 CA. Forwards to `canton:3975`. |
| `t1-gateway-edge` | `gateway-edge-net`, `gateway-net` | nginx. Publishes the service on `127.0.0.1:7599`. |

`gateway-net` is an internal network.
On Docker Desktop, a container on a normal bridge network can connect to `host.docker.internal`.
It then reaches the LocalNet ports that bind to 127.0.0.1.

The ledger user `disclosure-gateway` has only `CanReadAs` for `guardianGovernance` and `nttCustody`.
The CA, the certificates and the token go to `$LOCALNET_STATE_DIR/gateway-t1`.

### Prerequisites

- `ntt_governance.sh`, `ntt_burn_mint.sh` and `ntt_lock_unlock_gg.sh` ran.
- A disclosure service image from `canton/disclosure/service/Dockerfile` of the NTT repository.
  The image must have the role flags (`--guardian-governance-party`, `--ntt-custody-party`) and `--behind-edge-proxy`.
  The image must not serve `/v1/disclosures` (NTT branch `canton/ntt-disclosure-sets`, commit `ab36968` or later).

### Procedure

1. Start the gateway:

   ```bash
   DISCLOSURE_SERVICE_IMAGE=<image> canton/localnet/gateway_t1.sh up
   ```

2. Run the checks:

   ```bash
   canton/localnet/gateway_t1.sh check
   ```

3. Make sure that the last line of the output is `gateway_t1.sh check: PASS`.

4. Remove the containers and the networks:

   ```bash
   canton/localnet/gateway_t1.sh down
   ```

### Checks

| Check | Result |
| --- | --- |
| C1 | The proxy accepts the gateway client certificate. It refuses a request with no client certificate, and a certificate of a different CA, with HTTP 400. |
| C2 | From `gateway-net`, a direct connection to the participant ports fails. A connection to the proxy succeeds. |
| C3 | `/v1/healthz` and `/v1/readyz` give only a status. The index lists five routes: `/v1`, `/v1/healthz`, `/v1/readyz`, `/v1/managers`, `/v1/flows/{flow}`. Other paths get HTTP 404, for example `/v1/disclosures`. `/v1/managers` lists the deployments. The `register` flow returns the four genesis roots of `guardianGovernance`: `NttGovernance`, `EmitterRegistry`, `ReplayRootRegistry` and `CoreState`, of the pinned packages. |
| C4 | The relayer sees no `NttManager`, `CoreState` or `LockedLedger`. |
| C5 | The relayer submits `Mint` on the burn/mint deployment. |
| C6 | A user on `app-user` locks Canton Coin with `Transfer` on the deployment of `ntt_lock_unlock_gg.sh`. |
| C7 | The relayer submits `Release`. The recipient receives Canton Coin from the `nttCustody` reserve. |
| C8 | A `Release` with the set from before C7 fails with `LOCAL_VERDICT_INACTIVE_CONTRACTS`. A new set succeeds. |

For Canton Coin, the service names the committed `TransferFactory` and the `TransferPreapproval` as `missing`.
The relayer gets these contracts and the choice context from the Amulet registry API of Scan.

NOTE: The service keeps ledger reads for `--cache-ttl` (default 5 seconds).
After a rejection for an inactive contract, get the set again after this time.

### CIP-0056 registry API check

`registry_api_check.sh` starts one disclosure service container on the `localnet` network.
The service serves the CIP-0056 registry API (v1) for the instruments of `guardianGovernance`.
The script removes the container when it stops.

Prerequisites:

- `ntt_burn_mint.sh` ran.
- `gateway_t1.sh up` ran. The service reads the participant through the `t1-ledger-proxy` container with mutual TLS.
  The service sends a token only over HTTPS or to a loopback address.
- `DISCLOSURE_SERVICE_IMAGE` names an image of the disclosure service with the registry API.
- `NTT_GUARDIAN_SET_INDEX` and `NTT_GUARDIAN_KEY_INDICES` give the current guardian set of `CoreState`.

Procedure:

1. Run the check:

   ```bash
   DISCLOSURE_SERVICE_IMAGE=<image> canton/localnet/registry_api_check.sh
   ```

2. Make sure that the last line of the output starts with `registry_api_check.sh: PASS`.

The script does these steps:

1. A relayer mints the bridged token to party A on `app-user` with the mint flow of the service.
2. A is not a stakeholder of the `CoinFactory`.
   A sends tokens to party B with only the responses of the registry API.
3. The first transfer is an offer. B accepts it with the accept choice context.
4. B creates a `TransferPreapproval`. The second transfer is direct.
5. The script checks `totalSupply` before the mint, after the mint and after the transfers.

## Core package upgrade rehearsal

This directory no longer has the rehearsal of the `wormhole-core` upgrade from 0.6.0 to 0.7.0.
It needs a LocalNet with a 0.6.0 genesis, and no current LocalNet has one.
Section 7.5 of `canton/README.md` gives the results of that rehearsal.
The tag `canton/core-0.6-0.8.2-dars` (commit `1e5bf52f07`) has the script `core_upgrade_rehearsal.sh` and this section.
`watcher_harness_lib.sh` keeps the canary gate of the rehearsal.
