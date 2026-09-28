# Global Accountant

The Global Accountant is a Solana Anchor port of the wormchain Global
Accountant CosmWasm contract. It tracks a balance per `(chain, token_chain,
token_address)` triple. Each Token Bridge transfer changes one or two of
these balances. The program never lets a chain move more value than the
guardian network has attested for it.

The program accepts two kinds of input: signed VAAs and pre-quorum guardian
observations. It also accepts governance actions, each through its own
instruction, gated on a fixed governance emitter address.

## Instructions

| # | Name | Purpose |
|---|------|---------|
| 0 | `submit_observations` | Accumulate guardian signatures before a VAA exists. |
| 1 | `close_pending` | Reclaim rent from a pending-observations PDA. |
| 2 | `submit_vaas` | Apply a fully signed Token Bridge VAA. |
| 3 | `register_chain` | Governance: set the Token Bridge emitter for a foreign chain. |
| 4 | `modify_balance` | Governance: apply a manual balance correction. |
| 5 | `upgrade_contract` | Governance: replace this program's code. |

Each account table below lists write and signer flags as they apply on
Solana: **W** marks a writable account, **S** marks a required signer.

### 0. `submit_observations`

Guardians gossip observations of one message before a VAA exists. Each
observation adds one signature to a pending PDA keyed on `(chain, emitter,
sequence, guardian_set_index, digest)`. The observation that reaches quorum
commits the transfer, marks NoReplay, and closes the pending PDA.
`submit_vaas` shares this NoReplay state: each `(chain, emitter, sequence)`
commits once through either path.

| # | Account | W | S | Purpose |
|---|---------|---|---|---------|
| 0 | submitter | W | S | Pays rent for the pending PDA. |
| 1 | pending PDA | W | | Accumulates guardian signatures. |
| 2 | Core Bridge `GuardianSet` PDA | | | Checks the guardian signature. |
| 3 | NoReplay bitmap PDA | W | | Marked on quorum. |
| 4 | system program | | | |
| 5 | NoReplay program | | | |
| 6 | NoReplay authority PDA | | | This program's NoReplay CPI authority. |
| 7 | source-chain balance PDA | W | | Any account before quorum; checked on quorum. |
| 8 | destination-chain balance PDA | W | | As above. |
| 9 | rent recipient | W | | Must equal the pending PDA's recorded payer. |
| 10 | `ChainRegistration` PDA | | | Must match the VAA's emitter. |

### 1. `close_pending`

A permissionless cleanup instruction. It closes a pending-observations PDA
and refunds its rent once one of two conditions holds: the recorded guardian
set has expired, or NoReplay already marked the sequence through the other
path.

| # | Account | W | S | Purpose |
|---|---------|---|---|---------|
| 0 | closer | | S | Any signer can call this instruction. |
| 1 | pending PDA | W | | Closed on success. |
| 2 | rent recipient | W | | Must equal the recorded payer. |
| 3 | Core Bridge `GuardianSet` PDA | | | Checked for expiry. |
| 4 | NoReplay bitmap PDA | | | Checked for a mark. |

### 2. `submit_vaas`

Applies one fully signed VAA. The Verify VAA Shim checks the guardian
signatures. The instruction accepts a Token Bridge `Transfer`,
`TransferWithPayload`, or `AssetMeta` payload; `AssetMeta` is a no-op. A
governance VAA is always rejected here: its emitter is never a registered
Token Bridge contract, so the `ChainRegistration` check fails before any
balance changes.

| # | Account | W | S | Purpose |
|---|---------|---|---|---------|
| 0 | submitter | W | S | Pays rent for a new NoReplay bucket. |
| 1 | Verify VAA Shim program | | | |
| 2 | Core Bridge `GuardianSet` PDA | | | |
| 3 | `GuardianSignatures` PDA | | | Posted through the shim before this call. |
| 4 | NoReplay bitmap PDA | W | | |
| 5 | NoReplay program | | | |
| 6 | NoReplay authority PDA | | | |
| 7 | source-chain balance PDA | W | | Any account for a non-`Transfer` payload. |
| 8 | destination-chain balance PDA | W | | As above. |
| 9 | system program | | | |
| 10 | `ChainRegistration` PDA | | | Must match the VAA's emitter. |

### 3. `register_chain`

Governance action. Writes or overwrites the `ChainRegistration` PDA for one
foreign chain with the emitter address the VAA carries. A per-sequence
`RegisterChain` PDA is the replay guard.

Guardian governance sequence numbers are not issued in order. This
instruction accepts any unused sequence and overwrites the current
registration outright. A stale registration never returns on its own: only
one valid `RegisterChain` VAA per rotation keeps the registered emitter
current.

| # | Account | W | S | Purpose |
|---|---------|---|---|---------|
| 0 | payer | W | S | Pays rent for both PDAs below. |
| 1 | Verify VAA Shim program | | | |
| 2 | Core Bridge `GuardianSet` PDA | | | |
| 3 | `GuardianSignatures` PDA | | | |
| 4 | `ChainRegistration` PDA | W | | Written or overwritten. |
| 5 | system program | | | |
| 6 | `RegisterChain` PDA | W | | Replay guard, keyed on sequence. |

### 4. `modify_balance`

Governance action. Applies an `Add` or `Subtract` delta to one balance PDA.
`Add` on an absent PDA creates it with `balance = amount`. `Subtract` on an
absent PDA, or past the current balance, fails. A per-sequence
`ModifyBalance` PDA is the replay guard and the audit record.

| # | Account | W | S | Purpose |
|---|---------|---|---|---------|
| 0 | payer | W | S | Pays rent for both PDAs below. |
| 1 | Verify VAA Shim program | | | |
| 2 | Core Bridge `GuardianSet` PDA | | | |
| 3 | `GuardianSignatures` PDA | | | |
| 4 | `BalanceAccount` PDA | W | | Adjusted by the delta. |
| 5 | system program | | | |
| 6 | `ModifyBalance` PDA | W | | Replay guard and audit record, keyed on sequence. |

### 5. `upgrade_contract`

Governance action. Replaces this program's code through the BPF upgradeable
loader, using a buffer the VAA names. The shared NoReplay bitmap is the
replay guard here, the same one `submit_vaas` and `submit_observations` use,
keyed on the fixed governance emitter's address and chain.

| # | Account | W | S | Purpose |
|---|---------|---|---|---------|
| 0 | payer | W | S | |
| 1 | Verify VAA Shim program | | | |
| 2 | Core Bridge `GuardianSet` PDA | | | |
| 3 | `GuardianSignatures` PDA | | | |
| 4 | NoReplay bitmap PDA | W | | |
| 5 | NoReplay program | | | |
| 6 | NoReplay authority PDA | | | |
| 7 | system program | | | |
| 8 | upgrade authority PDA | | | This program's `[b"upgrade"]` PDA. |
| 9 | spill | W | | Receives the buffer's leftover lamports. |
| 10 | buffer | W | | Holds the replacement image; named in the VAA as `new_contract`. |
| 11 | program-data account | W | | |
| 12 | this program's account | W | | |
| 13 | rent sysvar | | | |
| 14 | clock sysvar | | | |
| 15 | BPF upgradeable loader | | | |

## Security model

**Governance gating.** `register_chain`, `modify_balance`, and
`upgrade_contract` each require a VAA from Solana chain 1, signed by the
fixed governance emitter address. No other emitter can reach these
instructions; the Verify VAA Shim and this check run before any state
change.

**Two replay-guard shapes.** `register_chain` and `modify_balance` each use
a dedicated per-sequence PDA as both their replay guard and their audit
trail: the PDA's existence blocks a replay, and its contents record what
happened. `upgrade_contract` instead marks the shared NoReplay bitmap, the
same one `submit_vaas` and `submit_observations` use. This blocks a replay
just as well, but leaves no on-chain record of which buffer a given sequence
applied — only program logs carry that.

**No governance path through `submit_vaas`.** A governance VAA's emitter is
never a registered Token Bridge contract for any chain. The
`ChainRegistration` check in `submit_vaas` and `submit_observations` rejects
it before the payload is even parsed.

**Account layout.** Every account carries a 1-byte `AccountTag` at offset 0.
Handlers read state with `UncheckedAccount` plus `bytemuck`, not Anchor's
`#[account(zero_copy)]`. PDA creation goes through
`CreateAccountAllowPrefund`, since `#[account(init)]` fails on a prefunded
PDA.

## Testing

- `just test` — the Mollusk integration suite, plus unit tests.
- `just e2e` — end-to-end tests against a real `surfpool` instance and the
  real Verify VAA Shim, for `submit_observations`, `submit_vaas`,
  `register_chain`, and `modify_balance`.
- `just e2e-upgrade-deploy` / `just e2e-upgrade-submit` / `just e2e-upgrade-stop`
  — the two-step `upgrade_contract` end-to-end test. Each recipe takes the
  cargo package as an optional argument; the default is this program.
- `just bench` — compute-unit regression tracking for `submit_vaas` and
  `submit_observations`, tracked at `benches/compute_units.md`.
