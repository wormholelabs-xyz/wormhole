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
| `transfer_fees_vectorgen_test.go` | generates the signed TransferFees fixtures of `Test.TestTransferFees` |
| `cantongrpc.go` | the gRPC client against the generated stubs |
| `cantongrpc_updateid_test.go` | unit tests for update ids on the stream and `GetUpdateById` |
| `proto/com/daml/ledger/api/v2/*.proto` | vendored Ledger API v2 protos (the State+Update closure) from **Canton 3.7.3** |
| `proto/gen/...` | generated Go stubs (committed; see below) |

The gRPC client builds by default — there is **no build tag**. (It depends only
on the committed, CI-verified generated stubs below; the watcher itself talks to
the `CantonClient` interface, and the Canton watcher only starts when
`--cantonRPC` is configured, so there is nothing to gate at compile time.)

## Vendored protos & regeneration

`proto/com/daml/ledger/api/v2/` contains the **real** Ledger API v2 protos: the
State + Update service closure (`state_service`, `update_service`, `transaction`,
`transaction_filter`, `event`, `value`, `reassignment`, `topology_transaction`,
`offset_checkpoint`, `trace_context`), plus `package_service` and
`package_reference` for `ListVettedPackages`, and `version_service` and
`experimental_features` for the server maximum of the `ListVettedPackages` page size.

Provenance of the fourteen files:

| Item | Value |
| --- | --- |
| Source | `lib/canton-open-source-3.7.3.jar` of the Daml SDK 3.7.3 `canton-open-source` component |
| Fetch | `dpm install 3.7.3` from `europe-docker.pkg.dev/da-images/public` |
| Component manifest digest | `sha256:c9b2511b45c55f411d602140e616f276497f407335a5657a08d5c616ead8ca8a` |
| Jar SHA-256 | `3824a19207b7be4aed3120006e0d48bbd5452196ef1d8b77b2d5ec2f086a9528` |

Extract each file with `unzip -p <jar> com/daml/ledger/api/v2/<file>.proto`.
The 3.7.3 protos add fields and RPCs to the 3.5.6 protos. Two server behaviors changed with them.
`GetActiveContracts` excludes incomplete reassignments by default; the watcher does not call it.
A `ListVettedPackages` page can be empty and still carry a next-page token, because the server applies
the package filter after it selects the page. `ListVettedPackageMetadata` follows the token.
`google/protobuf/*` come from the buf well-known types and
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

(or directly: `cd proto && buf generate --path com/daml/ledger/api/v2`).
Use the Go version of the CI `node-lint` job (1.25.10, for example
`GOTOOLCHAIN=go1.25.10`). `protoc-gen-go` formats its output with the `go/format`
package of the Go version that builds it, so another Go version changes the comments. It is
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

The test runs only when `dpm` and a Java 21 JDK are available. The default build
excludes it.
