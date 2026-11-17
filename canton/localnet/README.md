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
| `compose.digests.yaml` | Pins each Splice image to its digest. |
| `images.lock` | Image names, tags and digests. |
| `start.sh` | Starts LocalNet and waits for the Ledger APIs. |
| `stop.sh` | Stops LocalNet. |
| `token.sh` | Prints a JWT for the Ledger API. |
| `upload_dars.sh` | Uploads the core DAR and the test DAR. Vets all their packages. |
| `create_script_user.sh` | Creates the user that Daml Script uses. |
| `run_smoke.sh` | Runs the smoke script. |
| `daml/` | The smoke script package `wormhole-localnet-smoke`. |
| `canton_console.sh` | Runs a Canton console script against the three participants. |
| `console/` | Console configuration and console scripts. |
| `interactive_submit.js` | Prepares, signs and executes one external-party submission. LocalNet only. |
| `gg_lib.sh` | Shared functions for the `gg_*.sh` scripts. |
| `gg_host.sh` | Hosts `guardianGovernance` and `Operator`. Vets the core DARs on all participants. |
| `gg_show_topology.sh` | Shows the namespace and the hosting on each participant. |
| `gg_genesis.sh` | Creates the `CoreState` and the two registries. |
| `gg_publish_and_govern.sh` | Publishes one message. Applies one governance VAA. |
| `gg_confirmation_quorum.sh` | Checks the confirmation threshold with two hosts offline. |
| `gg_package_vetting.sh` | Checks the package vetting gate. |
| `vetting_probe/` | Versions 1.0.0 and 2.0.0 of the probe package for the vetting check. |
| `gg_guardian_set_rotation.sh` | Rotates the Wormhole guardian set. |
| `gg_rotate_owner_key.sh` | Replaces one `guardianGovernance` owner key. |

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
The logs do not contain tokens or private keys.
Keep the logs as the evidence of each run.

### Prerequisites

- LocalNet is running.
- You built the DARs. Refer to the procedure above.
- `node`, `jq`, `java` 21 and `go` are on the `PATH`.

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

### Replace a guardianGovernance owner key

1. Replace `owner3` with a new key `owner4`:

   ```bash
   canton/localnet/gg_rotate_owner_key.sh owner3 owner1 owner2 owner4
   ```

   The arguments are the removed owner, the two kept owners and the new owner.
   The script changes the decentralized namespace and the party signing keys.
   The threshold stays at 2 of 3.

2. Make sure that the output shows these results:
   - The kept owners and the new owner change the namespace.
   - The removed owner and one kept owner cannot change the namespace.
     The change fails with `TOPOLOGY_UNAUTHORIZED_TRANSACTION`.
   - A `guardianGovernance` transaction with one kept key and the new key succeeds.
   - A transaction with one kept key and the removed key fails with `FAILED_TO_EXECUTE_TRANSACTION`.
     The error shows 1 valid signature and 1 invalid signature.
   - A transaction with only the removed key fails with `FAILED_TO_EXECUTE_TRANSACTION`.
     The error shows 0 valid signatures and 1 invalid signature.

The script changes the namespace first and the party signing keys second.
If the second change fails, the rotation is half-applied.
The new owner is then in the namespace, but its key cannot sign for `guardianGovernance`.
The script shows `ROTATION HALF-APPLIED` and a recovery command.

3. If the script shows `ROTATION HALF-APPLIED`, run the script again with `--resume` and the same owner names:

   ```bash
   canton/localnet/gg_rotate_owner_key.sh --resume owner3 owner1 owner2 owner4
   ```

   The script uses the existing key of the new owner.
   It does not do the changes that are already effective.
   Then it does the checks of step 2.

### Limits of the rehearsal

- One container runs all three participants and the synchronizer.
  The check does not show a fault of a separate host or network.
- The checks disconnect participants from the synchronizer.
  They do not stop the participant processes.
- The vetting check fails at the submitting participant.
  It does not show the check on the other hosts for a submission that skips this check.
- Each script changes the ledger state. Run each script in the order of this section.
