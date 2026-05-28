# EVM RPC Guardian Compliance Tester

A standalone tool that checks whether an EVM RPC endpoint satisfies the assumptions the Wormhole guardian makes about EVM endpoints.

It reuses the same `node/pkg/watchers/evm/connectors` package the watcher uses, so it exercises the real code path (the `BatchPollConnector` / `PollConnector` the guardian would construct) rather than re-implementing the JSON-RPC spec.

## What it checks

| Check | What it validates | Guardian code it mirrors |
|---|---|---|
| Connectivity / chain ID | `eth_chainId`, optional match against `--evmChainId`, `web3_clientVersion` | `watcher.logVersion` |
| Finalized block tag | `eth_getBlockByNumber("finalized")` returns a real block and `finalized ≤ latest` | `connectors.GetBlockByFinality` |
| Safe block tag (with `--safe`) | `eth_getBlockByNumber("safe")` returns a real block and `finalized ≤ safe ≤ latest` | `connectors.GetBlockByFinality` |
| Block subscription | The `newHeads` subscription (WS) or poller (HTTP) delivers latest blocks during the run | `BatchPollConnector.SubscribeForBlocks` / `PollConnector` |
| Finalized advancement | A finalized block higher than the first one observed is delivered during the run | `BatchPollConnector.pollBlocks` |
| Generic block-hash match | For arbitrary transactions in observed blocks, `receipt.BlockHash` equals the block hash seen on the subscription | The invariant at `watcher.go` `tx.BlockHash != key.BlockHash` |
| Message block-hash match | For `LogMessagePublished` events from the configured core bridge, `receipt.BlockHash == ev.Raw.BlockHash` and `Status == 1` | Same invariant, Wormhole-specific path |
| Reorg replay (observational) | If a reorg happens during the run, the `newHeads` subscription replays affected heights (and logs come back with `Removed: true`) | `BatchPollConnector.SubscribeForBlocks` comment on rollback replay |

Each check is reported as `PASS`, `FAIL`, `INCONCLUSIVE`, or `SKIP`. The process exits non-zero if any check is `FAIL`.

`INCONCLUSIVE` means the endpoint did not exhibit the condition during the run (e.g. no `LogMessagePublished` events arrived, no reorgs occurred). Re-run with a longer `--duration` for more confidence.

Reorgs cannot be forced, so the reorg replay check is inherently observational. A clean run does not prove the endpoint replays — it just means no reorg happened in the window. A `FAIL` (height regression observed without replay) would be conclusive.

## Usage

```sh
go run . --rpc <url> [--contract <coreBridgeAddr>] [--evmChainId <id>] [--safe] [--duration 3m]
```

### Flags

| Flag | Default | Description |
|---|---|---|
| `--rpc` | (required) | RPC URL (`ws://`, `wss://`, `http://`, `https://`). HTTP picks the `PollConnector` path; WebSocket picks `BatchPollConnector`. |
| `--contract` | (none) | Wormhole core bridge address. Enables the `LogMessagePublished` block-hash match check. Without it, that check reports `SKIP`. |
| `--evmChainId` | `0` | Expected EVM chain ID. `0` skips the comparison. |
| `--safe` | `false` | Require the `safe` block tag in addition to `finalized`. |
| `--duration` | `3m` | How long to monitor the subscription for liveness and reorgs. |
| `--pollDelay` | `1s` | Delay between finalized/safe polls, matching the guardian default. |

`SIGINT` / `SIGTERM` stop the run early and still print the report for what was observed up to that point.

### Examples

Sepolia, with the Wormhole core bridge:

```sh
go run . \
  --rpc wss://ethereum-sepolia-rpc.publicnode.com \
  --contract 0x4a8bc80Ed5a4067f1CCf107057b8270E0cC11A78 \
  --evmChainId 11155111 \
  --safe \
  --duration 3m
```

HTTP-only endpoint (the tool will use the polling code path and `SKIP` the reorg replay check, since HTTP has no subscription to observe):

```sh
go run . --rpc https://… --evmChainId 1 --safe --duration 5m
```

Tag support only, no Wormhole traffic, short window:

```sh
go run . --rpc wss://… --duration 30s
```

## Output

```
================ EVM RPC Guardian Compliance Report ================
[PASS        ] Connectivity / chain ID
               chainId=11155111, node=Geth/v1.14.0/linux-amd64/go1.22.2
[PASS        ] Latest block tag
[PASS        ] Finalized block tag
               finalized=5824112, latest=5824144 (lag 32 blocks)
[PASS        ] Safe block tag
               safe=5824128 (finalized <= safe <= latest holds)
[PASS        ] Block subscription (newHeads)
               received 24 latest heads, high=5824168
[PASS        ] Finalized advancement
               finalized advanced 5824112 -> 5824120 during the run
[PASS        ] Generic block-hash match
               5 transaction(s): receipt block hash matched the observed block hash
[INCONCLUSIVE] Message block-hash match
               no LogMessagePublished events observed during the run
[INCONCLUSIVE] Reorg replay (observational)
               no reorgs occurred during the run, so replay could not be confirmed …
===================================================================
RESULT: OK — no failing checks (review any INCONCLUSIVE items)
```

## Notes

- The tool runs inside the `node` Go module and imports the watcher's connectors package; build it from `node/`, not from the repo root.
- This is a one-off operator tool living under `node/hack/`, alongside `wstest.go`. It is not imported by production code (see `node/hack/README.md`).
