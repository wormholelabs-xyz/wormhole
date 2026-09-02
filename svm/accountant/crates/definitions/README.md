# Global Accountant Definitions

Shared declarations for the Wormhole Global Accountant SVM programs: wire
formats, on-chain account layouts, instruction discriminants, error codes, and
protocol identifiers (program IDs, chain IDs, governance modules, PDA seeds).

The crate is `no_std` and carries no Solana runtime dependency, so the same
layouts and parsers can be used from on-chain programs, host-side tests, and
off-chain tooling (indexers, explorers, balance reconcilers) without pulling in
the Solana toolchain. The only dependencies are `bytemuck` (zero-copy account
layouts) and `ruint` (256-bit integer math).

## What's here

- `constants` — program IDs, chain IDs, governance modules, PDA seed prefixes,
  no-replay and verify-VAA-shim constants, and commit-log tags.
- `state` — zero-copy (`bytemuck`) account layouts: balance ledger, pending
  observations, modification, chain registration, and the NTT relayer / hub /
  peer registrations.
- `vaa` and `ntt` — payload parsers for Token Bridge and NTT messages.
- `instruction`, `error`, `primitives` — instruction discriminants, the program
  error enum, and the `Uint256` amount type.

Every public item is re-exported at the crate root, so consumers reach them as
`global_accountant_definitions::<Name>`.

## Scope

This is a declarations dictionary, not a behavior crate. Items belong here
because they are canonical declarations — wire formats, account schemas, or
protocol identifiers — not because more than one program consumes them; a parser
or layout used by a single program still lives here. Parsers stay policy-free
(bytes to struct). Logic that decides what a program does with the decoded data
belongs in that program; behavior shared across programs lives in the
operational-core and backfill-core crates.
