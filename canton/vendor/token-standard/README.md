# Vendored CIP-56 (Canton Network Token Standard) interface DARs

These are the `splice-api-token-*` **interface** packages (v1, 1.0.0) that the
NTT deployment integrates against so it can custody/mint Canton Coin (Amulet) or
any conforming token with no per-token code.

## Provenance

Vendored from `github.com/digital-asset/cn-quickstart`,
`quickstart/daml/dars/` (the same DARs that repo's `licensing` app pins as
`data-dependencies`). Fetched from the raw GitHub URLs, e.g.:

```
https://raw.githubusercontent.com/digital-asset/cn-quickstart/main/quickstart/daml/dars/splice-api-token-holding-v1-1.0.0.dar
```

Upstream source: `github.com/canton-network/splice`, `token-standard/`.

Committing the DARs (rather than fetching at build time) keeps the build
hermetic and matches cn-quickstart's own approach; that repo notes a TODO to
fetch them via a gradle-daml plugin once available.

## Files and who uses them

| DAR | Provides | Used by |
| --- | --- | --- |
| `splice-api-token-metadata-v1` | `Metadata`, `ChoiceContext`, `ExtraArgs`, `AnyContract` | `ntt-token` seam, `ntt`, `ntt-cip56`, tests |
| `splice-api-token-holding-v1` | `Holding`, `HoldingView`, `InstrumentId`, `Lock` | `ntt-token` seam, `ntt`, `ntt-cip56`, tests |
| `splice-api-token-transfer-instruction-v1` | `Transfer`, `TransferFactory` (`TransferFactory_Transfer`), `TransferInstruction` | `ntt-cip56` (lock/unlock) |
| `splice-api-token-burn-mint-v1` | `BurnMintFactory` (`BurnMintFactory_BurnMint`), `BurnMintOutput` | `ntt-cip56` (burn/mint) |

(`allocation`/`allocation-request`/`allocation-instruction` DARs are also
vendored here for future use — e.g. two-legged/locked settlement — but are not
yet imported.)

## Refreshing

Re-download the files from the URLs above (or rebuild them from the Splice
source), keeping the version in the filename, then `dpm build --all`.
