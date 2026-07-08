# cantonclient

A thin, watcher-oriented client for the [Canton](https://www.canton.network/)
**Ledger API v2** (gRPC). It mirrors `node/pkg/suiclient`: a small `CantonClient`
interface over the chain RPC plus plain domain types, so
`node/pkg/watchers/canton` does not depend on the generated protobuf stubs
directly.

See [`canton/README.md`](../../../canton/README.md) for the full design.

## Layout

| Path | Build | Purpose |
| --- | --- | --- |
| `cantonclient.go` | `CantonClient` interface, domain types, offset⇄TxID codec |
| `cantonclient_test.go` | unit tests for the offset codec |
| `vectorgen_test.go` | regenerates the Daml signed-VAA test vector (`GEN_CANTON_VECTORS=1`; skipped otherwise) |
| `cantongrpc.go` | the gRPC client against the generated stubs |
| `cantonacs.go` | ACS read path: `ActiveEmitterRequests` (crank work queue) + `ActiveEmitters` (`StateService.GetActiveContracts`) |
| `cantonacs_test.go` | unit tests for the `EmitterRequest` `CreatedEvent` decode |
| `cantoncmd.go` | **write path**: `SubmitApproveEmitter` (`CommandService`) + error classification |
| `cantoncmd_test.go` | **write path** unit tests: error classification, `Commands` construction |
| `proto/com/daml/ledger/api/v2/*.proto` | vendored Ledger API v2 protos (the State+Update+Command closure) from **Canton 3.5.1** |
| `proto/gen/...` | generated Go stubs (committed; see below) |

The gRPC client builds by default — there is **no build tag**. (It depends only
on the committed, CI-verified generated stubs below; the watcher itself talks to
the `CantonClient` interface, and the Canton watcher only starts when
`--cantonRPC` is configured, so there is nothing to gate at compile time.)

## Vendored protos & regeneration

`proto/com/daml/ledger/api/v2/` contains the **real** Ledger API v2 protos
(extracted from the `canton-open-source-3.5.1` jar): the State + Update service
closure (`state_service`, `update_service`, `transaction`, `transaction_filter`,
`event`, `value`, `reassignment`, `topology_transaction`, `offset_checkpoint`,
`trace_context`) plus the CommandService write-path closure (`command_service`,
`commands`, `reassignment_commands`). `google/protobuf/*` come from buf's
well-known types and `google/rpc/status` is mapped to `genproto` (see
`buf.gen.yaml`) to avoid a duplicate proto-registration panic.

Managed mode sets `go_package` to
`github.com/certusone/wormhole/node/pkg/cantonclient/proto/gen/com/daml/ledger/api/v2`
(package alias `apiv2`), which is what `cantongrpc.go` imports.

### Committed + CI-verified

Following the repo convention for generated protos (e.g. `node/pkg/proto`), the
generated stubs under `proto/gen/` are **committed** so the build and the
integration test work without a codegen step. To regenerate after changing the
vendored protos, run from the repo root:

```
make generate-canton-proto      # builds tools/bin/buf, then `buf generate --path com/daml/ledger/api/v2`
```

(or directly: `cd proto && buf generate --path com/daml/ledger/api/v2`). It is
also part of `make generate`. CI (`.github/workflows/build.yml`, the `node-lint`
job) re-runs `make generate-canton-proto` and fails on `git diff` — exactly like
the existing "Generated proto matches committed proto" check — so the committed
stubs can never drift from the vendored protos.

## Write path (emitter-approval crank)

`cantoncmd.go` is the Ledger API **write** path used by the emitter-approval
crank (`node/pkg/cantoncrank`): `SubmitApproveEmitter` exercises the consuming
`ApproveEmitter` choice on an `EmitterRequest` via
`CommandService.SubmitAndWaitForTransaction`, pinning the three command-dedup
change-id components (`act_as = [operator]`, a fixed `user_id`, and a
deterministic `command_id`) and classifying the outcome (`ErrDuplicateCommand` /
`ErrContractInactive` / `ErrSubmissionInFlight` / transient).

It builds by default against the committed stubs generated from the vendored
`CommandService` closure (`command_service.proto`, `commands.proto`,
`reassignment_commands.proto`; the other imports — `reassignment`,
`transaction`, `transaction_filter`, `value` — are already present for the read
path, and `google/protobuf/*` are buf well-known types).

`classifySubmitError` keys on the Canton self-service error-id token in the gRPC
status **message** (confirmed against Canton 3.5.1: e.g.
`DUPLICATE_COMMAND(10,...)`, `CONTRACT_NOT_FOUND(11,...)`), not on the bare gRPC
status code. Note that command deduplication only pre-empts interpretation when
the target contract is still active; a resubmission whose target `EmitterRequest`
has already been consumed surfaces as `CONTRACT_NOT_FOUND` (the consuming-choice
backstop), because Canton resolves the archived contract during interpretation
before the dedup check runs.

Run the write-path unit tests with:

```
go test ./pkg/cantonclient -run 'TestClassifySubmitError|TestBuildApproveCommands' -v
```

## Integration test

`node/pkg/watchers/canton/watcher_integration_test.go`
(`//go:build integration`) drives a live Canton sandbox via `dpm` and observes a
published message through this client end-to-end:

```
go test -tags integration -run TestCantonWatcherIntegration ./pkg/watchers/canton -v
```

It is skipped unless `dpm` (+ a JDK) is available, and is excluded from the
default build.
