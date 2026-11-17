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
| `canton_console.sh` | Runs a Canton console script against the three participants. Stops it after `CANTON_CONSOLE_TIMEOUT_SECONDS` (default 600). |
| `bounded_run.sh` | Runs a command with a timeout. `canton_console.sh` uses it. |
| `test/` | Script tests. They do not need Docker. |
| `console/` | Console configuration and console scripts. |
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
| `gg_rotate_owner_key.sh` | Replaces one `guardianGovernance` owner key. |
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
| `publish_message_scan.sh` | Lists each `PublishMessage` of a participant in an offset range. The canary scan of a production guardian. |

## Script tests

The script tests do not need Docker or LocalNet. Run each test from the repository root:

```bash
canton/localnet/test/bounded_run_test.sh
canton/localnet/test/image_lock_test.sh
canton/localnet/test/owner_signer_block_test.sh
canton/localnet/test/ntt_lib_test.sh
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
     The Admin API does not show this error code to the client. It shows a trace id.
     The script finds the error code with this trace id in the log of the `canton` container.
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

4. To rehearse the recovery, set `GG_ROTATION_FAIL_BEFORE_SIGNING_KEYS=true` for one rotation:

   ```bash
   GG_ROTATION_FAIL_BEFORE_SIGNING_KEYS=true \
     canton/localnet/gg_rotate_owner_key.sh owner3 owner1 owner2 owner4
   ```

   The rotation stops after the namespace change and shows `ROTATION HALF-APPLIED`.
   Then do step 3 without the variable.

### Limits of the rehearsal

- One container runs all three participants and the synchronizer.
  The check does not show a fault of a separate host or network.
- The checks disconnect participants from the synchronizer.
  They do not stop the participant processes.
- The vetting check fails at the submitting participant.
  It does not show the check on the other hosts for a submission that skips this check.
- Each script changes the ledger state. Run each script in the order of this section.

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
It makes the watcher with `WatcherConfig.Create` in the TestNet environment.
Thus `Create` requires the pinned `wormhole-core` package id and the read-as party.
The harness also checks that `Create` rejects a configuration without one of them.
It reads the update stream of `guardianGovernance` on `app-provider`.
`run_harness_on` starts a harness on the Ledger API of a different participant.

The harness authenticates as the Ledger API user `wormhole-watcher`.
`watcher_harness_lib.sh` makes this user on each participant with only the right `CanReadAs(guardianGovernance)`.
The script stops if the user has a different set of rights.
The script removes the token file when the harness streams.

LocalNet uses a static JWT on plaintext gRPC.
The harness sets this transport through `WatcherConfig.ledgerTransportForTest`.
Only tests of the `canton` package can set this field.

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
It follows the procedure of section 7.5 of `canton/README.md`.
The unvet of the old versions occurs immediately before the switch of the watcher pin.
The three `guardianGovernance` hosts are `app-provider`, `sv` and `app-user`.
They are also the only informee participants.

| Package | Package id | DAR |
| --- | --- | --- |
| `wormhole-core` 0.6.0 | `701a57c867e02d962f231bd9f0ff5186e16462a77289aa1b7aaba7fe95074c15` | `canton/dars/wormhole-core-0.6.0.dar` |
| `wormhole-core` 0.7.0 | `7a22a1c4026b2a037b021063c4ff2bd5506be2e748aa9de8b7a6057c90c356eb` | Build output of `canton/core` |
| `ntt` 0.2.0 | `8b9c3ee1c93b7039b39421b790c283f507c2b6a01171aa839873f220dc5ff84a` | Uploaded earlier. NTT commit `1db4b3c`. |
| `ntt` 0.3.0 | `291fcf920b971d50956845c2690723fb321f0a5b141a44caa5a16910f6ffa1a6` | `ntt/canton/ntt/.daml/dist/ntt-0.3.0.dar` |

The script stops if a core DAR has a different package id.

### Prerequisites

- The `guardianGovernance` rehearsal ran. The `CoreState` message fee is 0.
- The `ntt` 0.3.0 DAR exists. It calls `wormhole-core` 0.7.0.
- LocalNet has the `ntt` 0.2.0 package and a burn/mint `NttManager` that `ntt` 0.2.0 created. `ntt-keys` holds its admin key.
- Guardian set 2 with devnet keys 2, 3 and 4 is the current guardian set.
  Set `CURRENT_GUARDIAN_SET_INDEX` and `CURRENT_GUARDIAN_KEY_INDICES` for a different set.
- The 0.7.0 DAR exists. Run `dpm build --all` in `canton`.

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

| Step | Name | Result |
| --- | --- | --- |
| 0 | `prepare` | Each host vets core 0.6.0 and `ntt` 0.2.0. No host vets a different core or `ntt` version. |
| 1 | `observe-v6` | The guardian watchers `v6-<host>`, pinned to 0.6.0, observe a 0.6.0 `PublishMessage`. |
| 2 | `vet-v7` | The script records the start offset of each host. Each host vets core 0.7.0, then `ntt` 0.3.0. |
| 3 | `publish-v7` | One 0.7.0 `PublishMessage` and one 0.6.0 `PublishMessage` commit. Each `v6-<host>` observes the 0.6.0 message and drops the 0.7.0 message with a WARN log. The `canary`, pinned to 0.7.0, does the opposite. |
| 4 | `ntt` | `ntt_e2e.sh` passes with `ntt` 0.3.0. Each NTT `PublishMessage` runs under 0.7.0 on each host. |
| 5 | `gate` | Canary gate. The `canary` counter sum over all labels does not change in a quiet window of `QUIET_WINDOW_SECONDS` (20). The `canary` observes a control publish after the window. A scan of each host finds only the 0.6.0 publish of step 3. |
| 6 | `unvet-v6` | Each host unvets core 0.6.0 and `ntt` 0.2.0 in one topology transaction. A 0.6.0 publish and an `ntt` 0.2.0 exercise fail at prepare. Drain gate: the script reads the synchronizer timeouts and waits. Each `v6-<host>` drops a 0.7.0 control publish. A scan of each host finds no 0.6.0 publish that its `v6` watcher did not observe. |
| 7 | `switch-v7` | Each `v6-<host>` stops. On each host, the WARN logs, a scan from its start offset and the counter give the same dropped update ids. Each `v7-<host>` reobserves each of them. The three `v7` watchers agree. |
| 8 | `enforce-v7` | 0.7.0 rejects a nonce above `uint32`. 0.6.0 accepts it while vetted, and fails after the unvet. The step can run again. |
| 9 | `post-unvet` | Contracts from core 0.6.0 work under core 0.7.0. Contracts from `ntt` 0.2.0 work under `ntt` 0.3.0. `ntt_e2e.sh` passes. |
| 10 | `rollback` | A watcher pinned to 0.6.0 reobserves a 0.6.0 update while no host vets 0.6.0. Then each host vets 0.6.0 and `ntt` 0.2.0 again. 0.6.0 exercises an `Emitter` from 0.7.0. `ntt` 0.2.0 exercises an `NttManager` from `ntt` 0.3.0. |
| 11 | `restore` | Each host unvets core 0.6.0 and `ntt` 0.2.0 again. A 0.6.0 publish fails at prepare. |

The `vet-v7` step vets `ntt` 0.3.0 `291fcf92` by package id when each host has it.
Otherwise it uploads the NTT DAR, and stops if the DAR is a different package.

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
