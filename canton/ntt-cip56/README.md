# wormhole-ntt-cip56

Concrete CIP-56 (Canton Network Token Standard) implementations of the
[`NttToken`](../ntt-token/daml/Wormhole/Ntt/Token.daml) seam, so an NTT deployment
can custody/mint **Canton Coin (Amulet) or any conforming token** with no
per-token code. Builds against the vendored token-standard interface DARs
(see [`../vendor/token-standard/README.md`](../vendor/token-standard/README.md))
and is part of the `dpm build --all` workspace.

## Templates (`TokenCip56.daml`)

Both implement `NttToken`; each hook stores its registry **factory** contract-id
(fetched off-ledger from the token's registry / scan API) and the token
`InstrumentId`. The per-transfer holdings and choice context arrive through the
seam's widened `LockOrBurn` / `MintOrUnlock` choices.

- **`Cip56CustodyToken`** (lock/unlock) — drives `TransferFactory_Transfer`:
  lock = transfer `sender → custody`; unlock = transfer `custody → recipient`.
- **`Cip56BurnMintToken`** (burn/mint) — drives `BurnMintFactory_BurnMint`:
  burn = spend `inputHoldingCids` with no outputs; mint = one `BurnMintOutput`
  to the recipient.

`TrimmedAmount` (NTT's integer wire amount) is converted to the CIP-56 `Decimal`
via `trimmedToDecimal` (value = amount / 10^decimals).

## Submit-time authorization & disclosures (important)

The factory choices are real token-standard calls, so at **command submission**
the caller must:
- submit as / co-authorize the party whose holdings move (the sender on a
  lock/burn), and
- attach the registry's **disclosed contracts** referenced by `extraArgs`
  (the factory contract + any context), via `submitWithDisclosures` — exactly as
  cn-quickstart's `RegistryApi.getTransferFactory` + `submitWithDisclosures` do.

So a production NTT send in lock/unlock mode is app-orchestrated (query registry →
submit the manager `Transfer` with the sender's authority + disclosures), not a
bare operator-only command. The on-ledger Daml here is complete; that submission
wiring lives in the (off-ledger) relayer/app.

## Not yet wired

- `recipientAddress` (32-byte NTT address) → Canton `Party` still needs an
  address registry (the `Receive` recipient is relayer-supplied); see
  [`../README.md`](../README.md) §10.
- Two-legged / `Allocation`-based locked settlement (DARs are vendored, unused).
