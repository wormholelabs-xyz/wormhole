# Vendored Canton Network token-standard interface DARs

CIP-0056 (Token Standard V1) interface packages. The first three are consumed
by `wormhole-core` for fee payments (see the main README §4.2); the last two
(`burn-mint`, `transfer-instruction`) are consumed by the NTT layer's CIP-0056
token implementations (`ntt-cip56`, README §11). These are **interface-only**
packages — small, stable (frozen at 1.0.0), and network-vetted: the package-ids
must match what participants on the Canton Network have vetted, so these DARs
are taken verbatim from an official release and must never be rebuilt from
source (a different compiler produces different package-ids). They MUST all be
the same network-vetted 1.0.0 packages (damlc dedupes transitive DALFs by
package-id; mixing incompatible copies risks conflicts).

Provenance: `0.6.12_splice-node.tar.gz` from
https://github.com/digital-asset/decentralized-canton-sync/releases/tag/v0.6.12
(path `splice-node/dars/` inside the bundle). These 1.0.0 interface DARs are
immutable and distributed identically across releases: the `metadata`,
`holding`, and `allocation` DARs here are **byte-for-byte identical** (verified
by sha256) to the copies shipped in cn-quickstart, confirming they are the
canonical network-vetted packages.

| DAR | consumed by | sha256 |
| --- | --- | --- |
| `splice-api-token-metadata-v1-1.0.0.dar` | core (fees), NTT | `455eb160cb5abd4ae9918a6fbb9dad471f721adda39f0e5c76feef08d05637fc` |
| `splice-api-token-holding-v1-1.0.0.dar` | core (fees), NTT | `ef75f8eb41a65810221784fdb78bb9dfac7cb22245aba14fa7cb7f69c34e0175` |
| `splice-api-token-allocation-v1-1.0.0.dar` | core (fees) | `c3f3b447142577ea4fa7d912ca11cd6821de7588e324e8877425932a02fccaa1` |
| `splice-api-token-burn-mint-v1-1.0.0.dar` | NTT (ntt-cip56) | `a18e85c4841a278bce000df8329c3f0e2fee3b30b55dd6a31492d10a72b4f9c1` |
| `splice-api-token-transfer-instruction-v1-1.0.0.dar` | NTT (ntt-cip56) | `e4c73aa7ae73fb2fc330b938ffb99f568792321640ba4b9472902aa8d742c994` |

They are built `--target=2.1`; our packages target LF 2.3, which may
data-depend on lower LF 2.x versions. Token Standard V2 (CIP-0112) ships as a
parallel `-v2` interface family — adopting it is additive, not a replacement
(see README Open Questions).

NOTE: adding these dependencies (plus the new fee fields/args) makes
`wormhole-core` 0.2.0 SCU-incompatible with 0.1.0 — a participant holding
0.1.0 will reject the upload. Fine pre-mainnet: devnet re-bootstraps, and the
Go watcher's configured packageID changes with the new build.
