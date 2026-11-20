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

## Previous `wormhole-core` releases (upgrade baseline)

`core/daml.yaml` sets `upgrades:` to the DAR of the previous release.
`damlc` then checks that the new version is a valid smart contract upgrade (SCU).

| DAR | Baseline for | Source commit of `canton/core` | Main package id |
| --- | --- | --- | --- |
| `wormhole-core-0.6.0.dar` | 0.7.0 | `926e55310`, SDK 3.5.1 | `701a57c867e02d962f231bd9f0ff5186e16462a77289aa1b7aaba7fe95074c15` |
| `wormhole-core-0.7.0.dar` | 0.8.0 | `cfff39977`, SDK 3.5.1 | `7a22a1c4026b2a037b021063c4ff2bd5506be2e748aa9de8b7a6057c90c356eb` |
| `wormhole-core-0.8.0.dar` | 0.8.1 | `b1ed0c5f6`, SDK 3.5.1 | `03a65dc33bc2e08ad04318e300227e3f4dc6b983be09b650b59f2676f43a22b6` |
| `wormhole-core-0.8.1.dar` | 0.8.2 | `3e9747594`, SDK 3.5.1 | `4a214c8015945441a74dcc2a702f0652b3136e8f7488ab13d8196313c4d47d89` |
| `wormhole-core-0.8.2.dar` | 0.8.3 | `250021718`, SDK 3.5.1 | `132d32bfdba695df16b78b1c2250579d9369b13fe030d951973293f4e23b70c1` |

0.8.0 adds `guardianGovernance` as a signatory of `EmitterRegistry` (A14-3) and of `Emitter` (A14-2).
`damlc` reports these changes as the warning `upgraded-template-expression-changed`.
A 0.7.0 registry or `Emitter` cannot run under 0.8.0.
Thus 0.8.0 starts from a fresh genesis (`../A14_EMITTER_ID_UNIQUENESS.md`).

0.8.1 changes the `ReplayNode` precondition: the namespace must not be empty.
`damlc` reports this change as the warning `upgraded-template-expression-changed`.
A 0.8.0 `ReplayNode` with an empty namespace cannot run under 0.8.1.
No participant outside LocalNet vetted 0.8.0.

0.8.2 adds the `CoreState` precondition on the guardian sets, and genesis takes the guardian set index.
`damlc` reports the precondition change as the warning `upgraded-template-expression-changed`.
It reports the same warning for the `EmitterRegistry` precondition: only a compiler-generated name changed.
Each 0.8.1 `CoreState` of a LocalNet genesis or rotation satisfies the new precondition.
No participant outside LocalNet vetted 0.8.1.

0.8.3 adds `feeRecipient` (`feeCustody`) as a signatory of `CoreState`.
0.8.3 also adds `ensure` rules to `CoreState`: the fee instrument admin is not `guardianGovernance` and not the `operator`.
`damlc` reports these changes as the warning `upgraded-template-expression-changed`.
A 0.8.2 `CoreState` cannot run under 0.8.3.
Thus 0.8.3 starts from a fresh genesis (`../A14_EMITTER_ID_UNIQUENESS.md`).
0.8.0, 0.8.1 and 0.8.2 are withdrawn: their fee registry code runs as `guardianGovernance`.
The 0.8.3 main package id is `9d546b11014d1272be6b09a0ff9f6204a3401333829531d360e20fabd1b4163d`.
The first 0.8.3 build, `b5613c61c1aac18db84a87e2eb13cde6d544dd953d10c40de0440747e92a7e69`, is replaced.
It accepts `guardianGovernance` as the fee instrument admin. Do not deploy it.

0.11.1 parses the SetMessageFee fee and the TransferFees amount only below 2^63.
A larger value aborts with a range error, not with `ArithmeticError`.
The 0.11.1 main package id is `019609a33c112e13fa98642f1d5b4dd75e8ab7bfa00a14fcf57caa510ed1b9d5` (Daml SDK 3.7.3).
0.11.1 replaces 0.11.0 (`3e555cc2975764a29323906d74ab513025a3ba96376210b69daa542b294352bc`). No participant vetted 0.11.0.

0.11.0 sets a capacity on the `CoreState` governance collections.
`consumedGovernance` holds at most 32 digests, with a timestamp floor for evicted digests.
`guardianSets` holds at most 4 sets, and each governance transition removes expired sets.
It changes `CoreState` field types, so it is not an SCU upgrade of 0.10.0, and it starts a new `wormhole-core` line.
The 0.11.0 main package id is `3e555cc2975764a29323906d74ab513025a3ba96376210b69daa542b294352bc` (Daml SDK 3.7.3).
0.10.0 (`bf75fd518ed8f524832ac25c65a353215bb472a531d7ef745bf050848936eddb`) is replaced.
Its governance collections grow without a bound.

0.10.0 replaces the `Text` byte aliases with constrained byte types (`../README.md` §4.8).
It changes template field types, so it is not an SCU upgrade of 0.9.0, and it starts a new `wormhole-core` line.
The 0.10.0 main package id is `bf75fd518ed8f524832ac25c65a353215bb472a531d7ef745bf050848936eddb` (Daml SDK 3.7.3).
The SDK 3.7.3 0.9.0 build, `a562ac78ce2617dda9dc1ccd8c7500b4e523ff7ae1d216e28f8836b1fb264034`, is replaced. Do not deploy it.

0.9.0 removes the `operator` party from each core template and choice.
It removes template fields, so it is not an SCU upgrade of 0.8.x.
Thus `core/daml.yaml` has no `upgrades:` line, and 0.9.0 starts a new `wormhole-core` line.
0.9.0 `ensure` rules on `CoreState`: the fee instrument admin is neither `guardianGovernance` nor `feeCustody`.
The 0.9.0 main package id is `a562ac78ce2617dda9dc1ccd8c7500b4e523ff7ae1d216e28f8836b1fb264034` (Daml SDK 3.7.3).
The third 0.9.0 build, `bba94798a73c0fafadd836ec6754cab75205cb09556d13263813151bd748006a`, is built with Daml SDK 3.5.1. The SDK 3.7.3 build replaces it. Do not deploy it.
The first 0.9.0 build, `dd75f77780f2b93e5a30f12a04f54f9e4577c12ebcaf1720a413f09cc229af1d`, is replaced.
It accepts `guardianGovernance` as the fee instrument admin. Do not deploy it.
The second 0.9.0 build, `ec382591e253d8e5c9e8cd4f1de5fd3c40cb655c9f78da2d9de15af74958d66a`, is replaced.
Its `TransferFees` accepts chain 0, and its fee choices have no explicit bound asserts. Do not deploy it.
0.8.3 is withdrawn: the operator signs its core contracts.


To check the package id, run
`dpm damlc inspect-dar --json dars/<dar>` and read `main_package_id`.
The DAR file hash is not reproducible across builds; the package id is reproducible.
When a new `wormhole-core` version ships, vendor its DAR here and move the `upgrades:` field to it.
