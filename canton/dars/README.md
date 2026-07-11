# Vendored Canton Network token-standard interface DARs

CIP-0056 (Token Standard V1) interface packages consumed by `wormhole-core`
for fee payments (see the main README §4.2). These are **interface-only**
packages — small, stable (frozen at 1.0.0), and network-vetted: the
package-ids must match what participants on the Canton Network have vetted,
so these DARs are taken verbatim from an official Splice release bundle and
must never be rebuilt from source (a different compiler produces different
package-ids). All three MUST come from the same bundle (damlc dedupes
transitive DALFs by package-id; mixing bundles risks conflicting copies).

Provenance: `0.6.12_splice-node.tar.gz` from
https://github.com/digital-asset/decentralized-canton-sync/releases/tag/v0.6.12
(path `splice-node/dars/` inside the bundle).

| DAR | sha256 |
| --- | --- |
| `splice-api-token-metadata-v1-1.0.0.dar` | `455eb160cb5abd4ae9918a6fbb9dad471f721adda39f0e5c76feef08d05637fc` |
| `splice-api-token-holding-v1-1.0.0.dar` | `ef75f8eb41a65810221784fdb78bb9dfac7cb22245aba14fa7cb7f69c34e0175` |
| `splice-api-token-allocation-v1-1.0.0.dar` | `c3f3b447142577ea4fa7d912ca11cd6821de7588e324e8877425932a02fccaa1` |

They are built `--target=2.1`; our packages target LF 2.3, which may
data-depend on lower LF 2.x versions. Token Standard V2 (CIP-0112) ships as a
parallel `-v2` interface family — adopting it is additive, not a replacement
(see README Open Questions).

NOTE: adding these dependencies (plus the new fee fields/args) makes
`wormhole-core` 0.2.0 SCU-incompatible with 0.1.0 — a participant holding
0.1.0 will reject the upload. Fine pre-mainnet: devnet re-bootstraps, and the
Go watcher's configured packageID changes with the new build.
