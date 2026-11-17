# Canton NTT Accountant Configuration

## Scope

This document tells operations how to put a Canton NTT deployment under the NTT Global Accountant.
It covers the values to supply, how to derive them, and the procedure for each environment.

## Background

The guardian sends an NTT transfer to the NTT accountant only when the emitter is in its config.
The config is the function `nttGetEmitters` in `node/pkg/accountant/ntt_config.go`.
Each entry is a pair of a chain id and a 32-byte emitter address.

Canton entries go in the function `cantonNttTransceiverEmitters` in the same file.
There is one list for each environment: mainnet, testnet, and devnet.
All three lists are empty at this time.
Thus the NTT accountant does not limit transfers out of Canton.

The guardian checks each entry when it starts:

- The chain id must be a known chain id. Canton is chain id 75.
- Each entry in a Canton list must have chain id 75.
- The address must have exactly 64 hex characters, without a `0x` prefix.
- Each pair of chain id and address must occur only one time.

If one check fails and the guardian has the `--accountantNttContract` flag, `guardiand` stops at startup.
The accountant start returns an error, and `node.go` logs it as fatal.

## Values to supply after each Canton NTT deployment

Get these values for each deployment from the ledger:

| Value | Source |
|---|---|
| `operator` | The party id of the core bridge operator. It is the `operator` field of `NttManager`. |
| `guardianGovernance` | The party id of the guardian threshold party (`gg`). It is the `guardianGovernance` field of `NttManager`. |
| `transceiverEmitterId` | The `transceiverEmitterId` field of `NttManager`. |
| mode | The `tokenConfig` field of `NttManager` gives lock/unlock or burn/mint. In `TransceiverInit`, `0` is lock/unlock and `1` is burn/mint. |
| `managerAddress` | The `managerAddress` field of `NttManager`. Peer chains use it. The guardian config does not use it. |

Use the full party id text, which includes the `::` namespace part.

## Derive the transceiver emitter address

The transceiver emitter address is the address that the guardian config needs.
The Daml function `emitterAddressFor` computes it as follows:

```
keccak256( "wormhole:emitter:v1" ‖ lp(operator) ‖ lp(guardianGovernance) ‖ uint64be(transceiverEmitterId) )
```

The `lp` prefix is the byte length of the party id text, as a 4-byte big-endian integer.

The Go function `NttTransceiverEmitterAddress` in `node/pkg/watchers/canton/watcher.go` computes the same value.
The test `TestNttTransceiverEmitterAddressMatchesVector` pins it to the shared test vector.
`Test.TestCore:testAddressVector` pins the Daml side to the same vector.

Do not use the manager address in the guardian config.
The manager address has a different domain tag, `wormhole:ntt-manager:v1`.

## Hub and spoke rule

The NTT accountant keeps one hub for each NTT network.
Only a lock/unlock manager registers a hub.

1. A lock/unlock manager publishes `TransceiverInit` at sequence 0 when `RegisterManager` runs.
2. The accountant records a hub from a `TransceiverInit` VAA only when the mode is lock/unlock.
3. The accountant rejects a burn/mint `TransceiverInit` with the error `ignoring non-locking NTT initialization`.
4. A burn/mint Canton spoke gets its hub through peer registrations. The hub registers the spoke as a peer first.
5. Then the spoke registers the hub as a peer. The accountant then records the hub of the spoke.
6. The accountant counts each spoke transfer against the balance of its hub.

The accountant rejects a transfer that is larger than the source balance.
The error is `InsufficientBalance`.
Thus a Canton spoke cannot send out more than the hub network locked.

The accountant keeps each peer entry permanently.
A Canton `SetPeer` can replace a peer, but the accountant keeps the first value.
Thus a peer change on Canton needs a new deployment, with a new transceiver emitter.

## Enable procedure for each environment

Do these steps for each Canton NTT deployment, one environment at a time.

1. Get the values in the table above from the ledger of that environment.
2. Derive the transceiver emitter address with `NttTransceiverEmitterAddress`.
3. Compare the address with the emitter address on a VAA from that deployment.
4. If the deployment is a lock/unlock hub, submit its `TransceiverInit` VAA to the NTT accountant contract.
5. Submit the peer registration VAAs from the hub to the NTT accountant contract.
6. Submit the peer registration VAAs from each spoke to the NTT accountant contract.
7. Query `AllTransceiverHubs` on the NTT accountant contract. Make sure that the Canton emitter has the correct hub.
8. Query `AllTransceiverPeers`. Make sure that each peer entry is in both directions.
9. Add an entry to the list for that environment in `cantonNttTransceiverEmitters`.
10. Set `logOnly: true` on the new entry for the first release.
11. Run `go test ./pkg/accountant/...` in `node/`. This step is mandatory. Do not release the build if a test fails.
12. Release the guardian build to the guardians of that environment.
13. Monitor the NTT accountant logs for the Canton emitter. Make sure that no transfer fails.
14. Remove `logOnly: true` in a subsequent release to enforce the limits.

Use this format for the entry:

```go
{chainId: vaa.ChainIDCanton, addr: "<64 hex characters of the transceiver emitter address>"},
```

> **WARNING:** Do not add an entry before step 8 is complete. An enforced entry without a hub registration stops all transfers from that emitter.

> **WARNING:** Do not skip step 11. A malformed entry stops `guardiand` at startup on each guardian that enables the NTT accountant.

> **CAUTION:** Do not copy the addresses from `ntt_canton_test.go`. These addresses are for tests only.

> **NOTE:** The guardian needs the `--accountantNttContract` flag. Without it, the guardian does not start the NTT accountant.

## Known risk

The design note `A14_EMITTER_ID_UNIQUENESS.md` describes a fault where two emitters get the same address.
If this occurs, the accountant sees two messages with one message id.
The accountant then rejects the second message with a digest mismatch.
