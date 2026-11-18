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
| `gg_host.sh` | Hosts `guardianGovernance` and `Operator`. Vets the core DARs on all participants. |
| `gg_show_topology.sh` | Shows the namespace and the hosting on each participant. |
| `gg_genesis.sh` | Creates the co-signed `CoreState` and `EmitterRegistry`, then the `ReplayRootRegistry`. |
| `gg_publish_and_govern.sh` | Publishes one message. Applies one governance VAA. |
| `gg_confirmation_quorum.sh` | Checks the confirmation threshold with two hosts offline. |
| `gg_package_vetting.sh` | Checks the package vetting gate. |
| `vetting_probe/` | Versions 1.0.0 and 2.0.0 of the probe package for the vetting check. |
| `gg_guardian_set_rotation.sh` | Rotates the Wormhole guardian set. |
| `gg_rotate_owner_key.sh` | Replaces one owner key of the guardian namespace, for each party in the namespace. |
| `gg_rotation_fault_check.sh` | Rehearses the rotation faults R1 to R6. |
| `gg_rotation_safety_stop_check.sh` | Rehearses the rotation safety stop SS1 to SS4. |
| `custody_host.sh` | Allocates `nttCustody` under the `guardianGovernance` namespace. |
| `custody_authority_check.sh` | Checks that `guardianGovernance` and `nttCustody` have different authority. |
| `ntt_e2e.sh` | Runs the NTT steps below in sequence. |
| `ntt_upload_dars.sh` | Uploads the NTT DAR to all participants and vets its unvetted packages. |
| `ntt_governance.sh` | Creates the `NttGovernance` root and the canonical `CoinFactory`. |
| `ntt_burn_mint.sh` | Runs a burn/mint deployment of a `guardianGovernance` token. |
| `ntt_lock_unlock.sh` | Runs a lock/unlock deployment of Canton Coin. |
| `ntt_watcher.sh` | Runs the guardian watcher harness and checks its observations. |
| `ntt_lib.sh` | Shared functions for the `ntt_*.sh` scripts. |
| `ntt_offline_lib.sh` | NTT functions that `test/ntt_lib_test.sh` tests without LocalNet. |
| `allocate_external_party.js` | Allocates a 1-of-1 Ed25519 external party. |
| `watcher_harness_lib.sh` | Shared functions that start and stop the guardian watcher harness. |
| `core_upgrade_rehearsal.sh` | Rehearses the `wormhole-core` upgrade from 0.6.0 to 0.7.0, and the rollback. |
| `core_upgrade_package_ids.sh` | Package ids and DARs of the core upgrade rehearsal. |

## Script tests

The script tests do not need Docker or LocalNet. Run each test from the repository root:

```bash
canton/localnet/test/bounded_run_test.sh
canton/localnet/test/image_lock_test.sh
canton/localnet/test/owner_signer_block_test.sh
canton/localnet/test/ntt_lib_test.sh
canton/localnet/test/core_upgrade_package_ids_test.sh
canton/localnet/test/curl_calls_are_bounded_test.sh
canton/localnet/test/canary_gate_decision_test.sh
canton/localnet/test/owner_key_input_checks_test.sh
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
- `dpm` with Daml SDK 3.5.1 in `$HOME/.dpm/bin`.
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

1. Host `guardianGovernance` and `Operator`:

   ```bash
   canton/localnet/gg_host.sh
   ```

   The script uploads the core DARs to `app-provider`, `sv` and `app-user`.
   Each participant vets the DARs.
   It hosts `guardianGovernance` on the three participants.
   It hosts `Operator` on `app-provider` only.

2. Show the topology on each participant:

   ```bash
   canton/localnet/gg_show_topology.sh
   ```

   Each participant shows `threshold=2`, three hosts at `Confirmation` and `signingKeys=2-of-3`.

3. Create the `CoreState`:

   ```bash
   canton/localnet/gg_genesis.sh
   ```

   `Operator` and two owner keys sign the `CoreState`.
   Then the script creates the Ledger API user `wormhole-watcher` on each participant.
   This user has only the right `CanReadAs(guardianGovernance)`.
   `canton/tools/emitter_id_scan.sh` needs a token of this user on each participant that it reads.

4. Publish one message and apply one governance VAA:

   ```bash
   canton/localnet/gg_publish_and_govern.sh
   ```

   The governance VAA sets the message fee to 1000.
   All three participants show the new `CoreState`.

### Check the confirmation threshold

1. Run the check:

   ```bash
   canton/localnet/gg_confirmation_quorum.sh
   ```

   The argument is the key indices of the current guardian set. The default is 0.
   After a rotation to a new set, give the key indices of that set, for example `1`.
   The script disconnects `sv` and `app-user` from the synchronizer.
   Only `app-provider` can then confirm for `guardianGovernance`.
   A governance VAA, a VAA check and a publish each fail with `MEDIATOR_SAYS_TX_TIMED_OUT`.
   Each failure must name `guardianGovernance` as the only unresponsive party.
   With `app-user` connected again, the governance VAA succeeds.
   It sets the message fee to 0 again.
   At the end, the script connects all participants again.

2. After each connection change, the script reads the connection state of each participant.
   If a participant has the incorrect state, the script stops with an error.
   If a participant stays disconnected at the end, the script shows `ERROR` and a recovery command.

3. If the script shows the recovery command, run it:

   ```bash
   PARTICIPANT_ACTION=reconnect PARTICIPANT_NAMES=sv,app-user \
     canton/localnet/canton_console.sh canton/localnet/console/participant_connection.canton
   ```

NOTE: The mediator timeout uses sequencer time.
With two participants offline, sequencer time can stop.
The script requests time proofs from `app-provider` to move the time.

CAUTION: Without the time proofs, a transaction can wait for many minutes.
A host that connects again in that period can still confirm it.

4. After `custody_host.sh`, do the check for `nttCustody`:

   ```bash
   canton/localnet/gg_confirmation_quorum.sh --party "$(jq -r .partyId ../localnet-state/custody-artifacts.json)"
   ```

   With `app-user` disconnected, an `nttCustody` transaction succeeds with two confirming hosts.
   With `sv` and `app-user` disconnected, it fails with `MEDIATOR_SAYS_TX_TIMED_OUT`.
   The failure must name `nttCustody` as the only unresponsive party.
   Give `--signing-owners <owner>,<owner>` after a rotation that removes `owner1` or `owner2`.

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
   - A VAA of the new set passes `ParseAndVerifyVAA`.
   - A VAA of the old set passes until the expiry time.
   - A governance VAA of the old set fails with `governance: not signed by current guardian set`.
   - A VAA of the new set with an old key at index 0 fails with `public key does not match guardian address at index 0`.

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
The parties are `guardianGovernance` and, after `custody_host.sh`, `nttCustody`.
Thus a rotation changes the namespace and the `PartyToParticipant` mapping of each party in the namespace.

The rotation has these phases:

1. Safety stop. The script reads the namespace and each `PartyToParticipant` mapping in it on each participant.
   It stops before any change if one of these conditions is true:
   - Two participants show a different namespace definition, a different mapping or a different serial.
   - A party in the namespace has a `PartyToKeyMapping`.
   - A party in the namespace is not in the rotation list.
   - A party in the rotation list is not in the namespace.
   - The signing keys of a party are not the old key set or the new key set.
   - The namespace owners are not the old key set or the new key set.
2. Sign phase. `app-provider` generates each change.
   Before the first signer call, the script compares each change with the change that it calculates.
   The operation, the serial, the mapping and the hash must be equal.
   Then each owner signs each change. No change goes to the synchronizer.
   The script keeps the signed changes in `../localnet-state/rotation-<new-owner>.signed.bin`, with mode `0600`.
   If a signer fails, the script shows `ROTATION NOT STARTED`. The topology does not change.
   Before the load, in the fresh mode and in the `--resume` mode, the script checks each stored change:
   - Each `PartyToParticipant` change has the guardian policy.
   - Each change is equal to the change that the current topology needs, with the next serial.
   - A change that is already effective is equal to the current mapping, with the current serial.
   - The namespace change has the new owners and the current threshold.
3. Load phase. If the new owner has no root namespace delegation, load 1 adds it.
   This delegation alone gives no authority over the guardian namespace.
   Load 2 is one load call with all other changes: `nttCustody` first, then `guardianGovernance`, then the namespace.
   The script tries load 2 a maximum of 3 times. Each try loads only the changes that are not effective.
4. Checks. The script finds one of these results:
   - All changes are effective. The script shows `ROTATION effective times` for the changes and `ROTATION done`.
     The script removes the file of the signed changes.
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
   - `ROTATION parties: nttCustody,guardianGovernance`, or `guardianGovernance` alone before `custody_host.sh`.
   - `ROTATION protocol version: 37`.
   - `ROTATION effective times (one load call): nttCustody=<time> guardianGovernance=<time> namespace=<time>`.
   - `ROTATION effective times in load order, spread <seconds> s (bound 30 s)`.
     Canton gives each change of one load call its own effective time, in the load order.
     The script stops if a time is not after the time before it.
     The script also stops if the spread is more than 30 s.
   - The removed owner cannot add itself to the namespace again.
     Only the removed owner signs this probe. Thus the probe has no signature of a current owner.
     The change fails with `TOPOLOGY_UNAUTHORIZED_TRANSACTION`.
     The Admin API does not show this error code to the client. It shows a trace id.
     The script finds the error code with this trace id in the log of the `canton` container.
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

WARNING: After `ROTATION NOT APPLIED`, the file of the signed changes stays.
Each change in it stays valid until a different change takes its serial.
Keep the file secret. The next complete rotation takes each serial, and the file then has no effect.
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

4. Check the authority of the two parties. Use two current owner names:

   ```bash
   canton/localnet/custody_authority_check.sh owner1 owner2
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
   | R3b | `--resume` of R3. The signer of the new owner fails. | `ROTATION done`. 0 signatures before the removed-owner check. |
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
   | SS3 | The rotation list has an unknown party. | Stop: the parties differ. |
   | SS2 | A half-applied rotation A. Rotation B starts without `--resume`. | Stop: the `nttCustody` keys are not the old or the new set. |
   | SS4 | `--resume` of rotation A. | `ROTATION safety stop passed`, then `ROTATION done`. |

#### Results of the rotation measurements

Canton 3.7.3 on LocalNet, 2026-11-18, four runs:

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
- The comparison before the signature finds a changed change from `app-provider`.
  The owners still trust the console for the intended values. A production owner decodes each change (operations document, Section 7.4).

## NTT end-to-end run

These scripts run the Wormhole NTT layer on LocalNet.
The NTT packages come from the NTT repository, branch `canton/overnight-ntt`, commit `79d739b16`.
Set `NTT_REPO_DIR` to the root of that checkout.
The default is `../ntt`, next to this repository.
`ntt_lib.sh` pins the package IDs of `wormhole-core` 0.8.0, `ntt` 0.3.2 and `token-cip0056` 0.2.0.
`ntt_upload_dars.sh` stops if a DAR has a different package ID.

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
| 1 | `ntt_upload_dars.sh` | Each participant vets each package of the `ntt` DAR, and no `wormhole-core` newer than 0.8.0. |
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
  The ledger rejects it with `public key does not match guardian address at index 0`.
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

## Core package upgrade rehearsal

`core_upgrade_rehearsal.sh` does the `wormhole-core` upgrade from 0.6.0 to 0.7.0 on LocalNet.
It follows the one-id procedure of section 7.5 of `canton/README.md`.
The unvet of the old versions occurs immediately before the switch of the watcher pin.
The three `guardianGovernance` hosts are `app-provider`, `sv` and `app-user`.
They are also the only informee participants.

| Package | Package id | DAR |
| --- | --- | --- |
| `wormhole-core` 0.6.0 | `701a57c867e02d962f231bd9f0ff5186e16462a77289aa1b7aaba7fe95074c15` | `canton/dars/wormhole-core-0.6.0.dar` |
| `wormhole-core` 0.7.0 | `7a22a1c4026b2a037b021063c4ff2bd5506be2e748aa9de8b7a6057c90c356eb` | `canton/dars/wormhole-core-0.7.0.dar` |
| `ntt` 0.2.0 | `8b9c3ee1c93b7039b39421b790c283f507c2b6a01171aa839873f220dc5ff84a` | `ntt/canton/dars/ntt-0.2.0.dar`. NTT commit `1db4b3c`. |
| `ntt` 0.3.0 | `291fcf920b971d50956845c2690723fb321f0a5b141a44caa5a16910f6ffa1a6` | `NTT_V3_DAR`. Build NTT commit `a1aa515`. |

`core_upgrade_package_ids.sh` holds these package ids, versions and DAR paths.
guardiand does not approve 0.6.0 or 0.7.0.
Thus the rehearsal writes an approved list with these two versions to `core-upgrade-watcher/approved_core_packages.json`.
Each rehearsal harness uses this list.
The script stops if a DAR has a different package id.
`test/core_upgrade_package_ids_test.sh` checks the DARs without LocalNet.

### Scope with `wormhole-core` 0.8.0

The rehearsal keeps the versions 0.6.0 and 0.7.0.
The procedures of section 7.5 apply only to an upgrade that keeps the signatories of each template.
0.6.0 and 0.7.0 are the only pair of released versions with this property.
0.8.0 adds `guardianGovernance` as a signatory of `EmitterRegistry` and `Emitter`.

The rehearsal cannot run on a LocalNet with the 0.8.0 genesis:

- The 0.8.0 `CoreState`, `EmitterRegistry` and `Emitter` cannot run under 0.6.0 or 0.7.0.
- `ntt_e2e.sh` follows the `ntt` 0.3.2 and `wormhole-core` 0.8.0 pins of `ntt_lib.sh`.
  Thus the steps `ntt` and `post-unvet` fail with `ntt` 0.3.0.

The `prepare` step stops before it changes the vetting state if the Operator `CoreState` is not a 0.6.0 contract.
Use the rehearsal again for the first upgrade from 0.8.0.
Then change the package ids and DARs in `core_upgrade_package_ids.sh`.

### Prerequisites

- The Operator `CoreState` is a 0.6.0 contract. The `CoreState` message fee is 0.
- `NTT_V3_DAR` is the `ntt` 0.3.0 DAR. It calls `wormhole-core` 0.7.0.
- LocalNet has the `ntt` 0.2.0 package and a burn/mint `NttManager` that `ntt` 0.2.0 created. `ntt-keys` holds its admin key.
- Guardian set 2 with devnet keys 2, 3 and 4 is the current guardian set.
  Set `CURRENT_GUARDIAN_SET_INDEX` and `CURRENT_GUARDIAN_KEY_INDICES` for a different set.

### Procedure

1. Run all steps:

   ```bash
   canton/localnet/core_upgrade_rehearsal.sh all
   ```

2. Make sure that the last line of the output is `core_upgrade_rehearsal.sh all: PASS`.

To run one step only, give the step name. Run the steps in the sequence of the table.
The watchers `v6-<host>` and `canary` continue to run between steps.
Each guardian watcher runs as three harnesses, one on each participant.
Each harness reads as `guardianGovernance` on its own participant.

The table gives the expected result of each step.
No LocalNet run of the steps `gate`, `switch-v7` and `abort-v7` exists at this commit.
Section 7.5 of `canton/README.md` gives the results of the run at commit `395103f5e`.

| Step | Name | Expected result |
| --- | --- | --- |
| 0 | `prepare` | Each host vets core 0.6.0 and `ntt` 0.2.0. No host vets a different core or `ntt` version. |
| 1 | `observe-v6` | The guardian watchers `v6-<host>`, pinned to 0.6.0, observe a 0.6.0 `PublishMessage`. |
| 2 | `vet-v7` | The script records the start offset of each host. Each host vets core 0.7.0, then `ntt` 0.3.0. |
| 3 | `publish-v7` | One 0.7.0 `PublishMessage` and one 0.6.0 `PublishMessage` commit. Each `v6-<host>` observes the 0.6.0 message and drops the 0.7.0 message with a WARN log. The `canary`, pinned to 0.7.0, does the opposite. |
| 4 | `ntt` | `ntt_e2e.sh` passes with `ntt` 0.3.0. Each NTT `PublishMessage` runs under 0.7.0 on each host. |
| 5 | `gate` | Canary gate. The `canary` counter sum over all labels does not change in a quiet window of `QUIET_WINDOW_SECONDS` (20). A change starts the window again. The gate times out after `CANARY_GATE_MAX_SECONDS` (120). The `canary` observes a control publish after the window. A scan of each host to the control finds only the 0.6.0 publishes before the window. |
| 6 | `unvet-v6` | Each host unvets core 0.6.0 and `ntt` 0.2.0 in one topology transaction. A 0.6.0 publish and an `ntt` 0.2.0 exercise fail at prepare. Drain gate: the script reads the synchronizer timeouts and waits. Each `v6-<host>` drops a 0.7.0 control publish. A scan of each host finds no 0.6.0 publish that its `v6` watcher did not observe. |
| 7 | `switch-v7` | Each `v6-<host>` stops. A 0.7.0 publish commits while no watcher runs. Each `v7-<host>` starts and observes a 0.7.0 restart control. On each host, a scan to the restart control gives the WARN log update ids and the gap publish. The counter labels are exactly 0.7.0. Each running `v7-<host>` gets these ids in its request file and reobserves each of them. The three `v7` watchers agree. |
| 8 | `enforce-v7` | 0.7.0 rejects the nonces -1 and 4294967296. 0.7.0 commits the nonce 0. It also commits 4294967295. 0.6.0 accepts 4294967296 while vetted. After the unvet, it fails. The step can run again. |
| 9 | `post-unvet` | Contracts from core 0.6.0 work under core 0.7.0. Contracts from `ntt` 0.2.0 work under `ntt` 0.3.0. `ntt_e2e.sh` passes. |
| 10 | `rollback` | A watcher pinned to 0.6.0 reobserves a 0.6.0 update while no host vets 0.6.0. Then each host vets 0.6.0 and `ntt` 0.2.0 again. 0.6.0 exercises an `Emitter` from 0.7.0. `ntt` 0.2.0 exercises an `NttManager` from `ntt` 0.3.0. |
| 11 | `restore` | Each host unvets core 0.6.0 and `ntt` 0.2.0 again. A 0.6.0 publish fails at prepare. |
| A | `abort-v7` | After a gate timeout. Each host unvets core 0.7.0 and `ntt` 0.3.0 in one topology transaction. A 0.7.0 publish and an `ntt` 0.3.0 exercise fail at prepare. After the drain wait, each `v6-<host>` observes a 0.6.0 control. The scan to the control equals the WARN log ids of each `v6-<host>`. Each `v6-<host>` stops. A 0.6.0 publish commits while no watcher pinned to 0.6.0 runs. Each `v7-recovery-<host>` reobserves each 0.7.0 update. Each `v6-after-abort-<host>` observes a second 0.6.0 control. On each host, a scan from the abort offset to the second control lists the gap publish. Each host reobserves the 0.6.0 publishes of this scan. |

The sequence `abort-all` runs `prepare`, `observe-v6`, `vet-v7`, `publish-v7`, `gate` and `abort-v7`.
The gate must time out at most 5 seconds after `CANARY_GATE_MAX_SECONDS`. The `canary` counter must grow.
To get the timeout, set `CANARY_GATE_INJECTED_OLD_PUBLISH_INTERVAL_SECONDS` to a value below `QUIET_WINDOW_SECONDS`:

```bash
CANARY_GATE_INJECTED_OLD_PUBLISH_INTERVAL_SECONDS=10 canton/localnet/core_upgrade_rehearsal.sh abort-all
```

The script then publishes under 0.6.0 every 10 seconds of the gate. This fault is for the rehearsal only.

The `vet-v7` step vets `ntt` 0.3.0 `291fcf92` by package id on each host that has it.
On each other host, it uploads `NTT_V3_DAR`. It stops if the DAR is a different package.

The evidence log is `$LOCALNET_STATE_DIR/evidence/core_upgrade_rehearsal-<time>.log`.
The directory with the same name holds the watcher observations, logs, counters and the dropped update ids.
`$LOCALNET_STATE_DIR/core-upgrade.json` holds the offsets and update ids that later steps use.

### Checks

- Each negative watcher check has a positive control in the same window.
  The same watcher observes a message of its pinned package.
- Each drop has a WARN log with its update id and the executing package id.
- Each watcher log has no `canton_fetch_obvs_req failed`, `invalid txID` or `returned a different update`.
- A rejection for an unvetted package must have `INVALID_PRESCRIBED_SYNCHRONIZER_ID`.
  It must also have `has not vetted <package id>` once for each host.
- A Daml rejection must have `DAML_FAILURE` and the assertion message.
- The script checks the package that created the `CoreState` (0.6.0).
  It also checks the package that executed the choice (0.7.0).

### Results

- The `ExercisedEvent` of `PublishMessage` shows the package that ran the choice.
  It is the same on the three participants. The participant offsets are different.
- A `PublishMessage` re-creates the `Emitter` under the package that runs the choice.
- A submission with the package name selects 0.7.0 when each host vets 0.6.0 and 0.7.0.
- After the unvet, a 0.6.0 submission fails at prepare. The `Emitter` does not change.
- After the unvet, contracts from 0.6.0 work under 0.7.0.
  The script checks `PublishMessage` on an `Emitter` and `ParseAndVerifyVAA` on the `CoreState`.
- After the unvet, `SetPaused` by package name on an `NttManager` from `ntt` 0.2.0 runs under `ntt` 0.3.0.
  Thus `ntt` 0.3.0 is a valid upgrade of `ntt` 0.2.0, and `ntt` 0.2.0 managers need no migration.
- The `prepare` step unvets 0.7.0. Then a 0.6.0 `PublishMessage` on an `Emitter` from 0.7.0 commits.
- The `ntt` 0.2.0 package calls 0.6.0. Thus each `ntt` 0.2.0 `PublishMessage` runs under 0.6.0.
  An earlier run unvetted 0.6.0 but kept `ntt` 0.2.0.
  Then the `ntt` 0.2.0 `RegisterManager` failed with `required dependencies not vetted`.
- The scan as `wormhole-watcher` finds each dropped update. Thus the right `CanReadAs(guardianGovernance)` is enough.

WARNING: Do not unvet 0.6.0 and do not pin the watchers to 0.7.0 while `ntt` 0.2.0 is in use.
The NTT messages then stop. First build NTT on `wormhole-core` 0.7.0.

CAUTION: While each host vets 0.6.0, a submitter can select 0.6.0 and skip the 0.7.0 checks.

Final LocalNet state after `post-unvet`:

| Host | `wormhole-core` | `ntt` | `token-cip0056` |
| --- | --- | --- | --- |
| `app-provider` | 0.7.0 `7a22a1c4` | 0.3.0 `291fcf92` | 0.2.0 `0a624d6a` |
| `sv` | 0.7.0 `7a22a1c4` | 0.3.0 `291fcf92` | 0.2.0 `0a624d6a` |
| `app-user` | 0.7.0 `7a22a1c4` | 0.3.0 `291fcf92` | 0.2.0 `0a624d6a` |
