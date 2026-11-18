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
| `cantonclient.go` | `CantonClient` interface, domain types |
| `updateid.go` | `UpdateID`: Ledger API update id ⇄ 34-byte `TxID` codec (canton/README.md §7.2) |
| `updateid_test.go` | unit tests for the update id codec |
| `vectorgen_test.go` | asserts the Daml hex fixtures match the generator; `CANTON_VECTORGEN=write` rewrites them |
| `cantongrpc.go` | the gRPC client against the generated stubs |
| `cantongrpc_updateid_test.go` | unit tests for update ids on the stream and `GetUpdateById` |
| `proto/com/daml/ledger/api/v2/*.proto` | vendored Ledger API v2 protos (the State+Update closure) from **Canton 3.5.5** |
| `proto/gen/...` | generated Go stubs (committed; see below) |

The gRPC client builds by default — there is **no build tag**. (It depends only
on the committed, CI-verified generated stubs below; the watcher itself talks to
the `CantonClient` interface, and the Canton watcher only starts when
`--cantonRPC` is configured, so there is nothing to gate at compile time.)

## Vendored protos & regeneration

`proto/com/daml/ledger/api/v2/` contains the **real** Ledger API v2 protos
(extracted from the `canton-open-source-3.5.5` jar): the State + Update service
closure (`state_service`, `update_service`, `transaction`, `transaction_filter`,
`event`, `value`, `reassignment`, `topology_transaction`, `offset_checkpoint`,
`trace_context`). `package_service` and `package_reference` (for
`ListVettedPackages`) come from the `canton-open-source-3.5.6` jar. The ten
files above are byte-identical in 3.5.5 and 3.5.6. `google/protobuf/*` come from the buf well-known types and
`google/rpc/status` is mapped to `genproto` (see `buf.gen.yaml`) to avoid a
duplicate proto-registration panic.

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

## Integration test

`node/pkg/watchers/canton/watcher_integration_test.go`
(`//go:build integration`) drives a live Canton sandbox via `dpm` and observes a
published message through this client end-to-end:

```
go test -tags integration -run TestCantonWatcherIntegration ./pkg/watchers/canton -v
```

It is skipped unless `dpm` (+ a JDK) is available, and is excluded from the
default build.
