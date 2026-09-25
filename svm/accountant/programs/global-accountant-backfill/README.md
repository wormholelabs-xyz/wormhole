# Global Accountant Backfill

## Purpose

This program is the one-shot migration image for the Global Accountant (WTT).
It seeds the operational program's state from a wormchain `query_all_accounts`
snapshot. It has one instruction per state kind: NoReplay bits, `BalanceAccount`
PDAs, `ModifyBalance` records, and the Token Bridge `ChainRegistration` state.

The image occupies the operational program account
`US517G5965aydkZ46HS38QLi7UQiSojurfbQfKCELFx`. `declare_id!` carries that
address, and a `const _` assert pins it to `GLOBAL_ACCOUNTANT_PROGRAM_ID`. The
operator ends the migration with `solana program upgrade`, which installs
`global_accountant.so` in the same account.

Every handler comes from `accountant-backfill-core`. Each handler writes through
the `operational-core` PDA helpers that the operational program itself uses. The
bytes this program writes therefore equal the bytes the operational program
reads.

## Program layout

```
global-accountant-definitions
  <- accountant-operational-core
    <- accountant-backfill-core
      <- global-accountant-backfill
```

An operational program never depends on `accountant-backfill-core`. A backfill
handler therefore cannot enter a live dispatch table and still compile.

The shell crate holds the Anchor boundary only:

- `declare_id!` and the program-id assert, in `src/lib.rs`.
- `BACKFILL_AUTHORITY`, in `src/lib.rs`.
- The `flatten_accounts!` macro, in `src/lib.rs`.
- Four `#[derive(Accounts)]` contexts, in `src/contexts.rs`.
- `RawIxData`, the raw instruction-data newtype, in `src/raw_ix_data.rs`.

An arm parses no payload itself. It flattens its context plus
`ctx.remaining_accounts` into one positional `Vec<AccountInfo>`. It then calls
its handler in `accountant_backfill_core::instructions`.

## Instructions

| # | Name | Handler | PDAs per entry |
|---|------|---------|----------------|
| 0 | `backfill_no_replay` | `backfill_noreplay` | One bucket per bucket, not per entry |
| 1 | `backfill_balance` | `backfill_balance` | One `BalanceAccount` |
| 2 | `backfill_modify_balance` | `backfill_modify_balance` | One `ModifyBalance` |
| 3 | `backfill_chain_registration` | `backfill_chain_registration` | Two: `ChainRegistration`, then `RegisterChain` |

The sibling `ntt-global-accountant-backfill` program shares these four handlers
at the same discriminators. It adds two NTT-only arms at 4 and 5.

### PDAs

| PDA | Tag | Seeds | Bytes |
|---|---|---|---|
| `BalanceAccount` | 2 | `b"account"`, chain, token_chain, token_address | 70 |
| `ChainRegistration` | 3 | `b"chain_registration"`, chain | 64 |
| `ModifyBalance` | 4 | `b"modify_balance"`, sequence | 112 |
| `RegisterChain` | 5 | `b"register_chain"`, sequence | 48 |

Tag is the 1-byte `AccountTag` at offset 0. Numeric seed fields are big-endian.
Creation goes through `create_pda_allow_prefund`, so a prefunded PDA still
works. A PDA that already holds data raises `InvalidPda`, which makes a
resubmitted batch fail.

`backfill_no_replay` instead flips bits in the NoReplay program's bitmap
buckets. A bucket address derives from the NoReplay authority PDA, the
namespace seed chunks of `(chain, emitter)`, and the little-endian bucket index.

### Account lists

**W** marks a writable account. **S** marks a required signer. The variadic PDAs
ride in `ctx.remaining_accounts`, in wire order.

`backfill_no_replay`:

| # | Account | W | S | Purpose |
|---|---------|---|---|---------|
| 0 | payer | W | S | Must equal `BACKFILL_AUTHORITY`. |
| 1 | NoReplay program | | | The runtime loads it; the CPI target is the constant `NOREPLAY_PROGRAM_ID`. |
| 2 | NoReplay authority PDA | | | This program's `[b"noreplay_authority"]` PDA. |
| 3 | system program | | | |
| 4.. | NoReplay bitmap bucket | W | | One per bucket, in walk order. |

The other three instructions:

| # | Account | W | S | Purpose |
|---|---------|---|---|---------|
| 0 | payer | W | S | Must equal `BACKFILL_AUTHORITY`. |
| 1 | system program | | | `create_pda_allow_prefund` needs it for its CPI. |
| 2.. | target PDA | W | | One or two per entry; see the instruction table. |

A PDA account count that differs from the entry count raises
`InvalidInstructionData`. Each handler checks the authority first, before it
parses the batch and before it writes anything.

## Wire formats

Instruction data is `discriminator(1) ‖ payload`. A payload starts with a 1-byte
count. The parser is the only constructor, and it rejects five shapes with
`InvalidInstructionData`: a count of 0, a count above `MAX_BATCH_ENTRIES`, a
length that the count does not explain, a pair of entries out of order, and a
duplicate sort key. Sort order is strictly ascending on the key named below.

`MAX_BATCH_ENTRIES` is 63. Each entry costs one System Program CPI, and Solana
caps an instruction trace at 64 entries, so a larger batch would abort mid-write
with `MaxInstructionTraceLengthExceeded`. The transaction packet gives a lower
practical ceiling. The cost probes measure that ceiling per arm against a real
validator.

`BackfillBalance` entry, 68 bytes. Sort key `(chain, token_chain,
token_address)`:

| offset | size | field |
|---|---|---|
| 0 | 2 | chain |
| 2 | 2 | token_chain |
| 4 | 32 | token_address |
| 36 | 32 | balance |

`BackfillModifyBalance` entry, 109 bytes. Sort key `sequence`:

| offset | size | field |
|---|---|---|
| 0 | 1 | kind |
| 1 | 2 | chain_id |
| 3 | 2 | token_chain |
| 5 | 8 | sequence |
| 13 | 32 | token_address |
| 45 | 32 | amount |
| 77 | 32 | reason |

`kind` is 1 for `Add` and 2 for `Subtract`. Any other value raises
`InvalidModificationKind`.

`reason` is the audit text zero-left-padded to 32 bytes, which is how the
governance wire carries it: a 3-byte reason occupies bytes 29 to 31 and bytes 0
to 28 are zero. Copy the 32 bytes out of the original VAA payload.

`BackfillChainRegistration` entry, 42 bytes. Sort key `chain`:

| offset | size | field |
|---|---|---|
| 0 | 2 | chain |
| 2 | 8 | sequence |
| 10 | 32 | emitter |

`emitter` is the Token Bridge emitter address on `chain`. The handler writes
both the `ChainRegistration` PDA and the `RegisterChain` record for one entry.

### NoReplay batches

`BackfillNoReplay` groups its entries by emitter. The payload is
`group_count(1) ‖ group_count × (header ‖ entry_count × entry)`.

Group header, 35 bytes:

| offset | size | field |
|---|---|---|
| 0 | 2 | chain |
| 2 | 32 | emitter |
| 34 | 1 | entry_count |

Entry, 40 bytes:

| offset | size | field |
|---|---|---|
| 0 | 8 | sequence |
| 8 | 32 | digest |

Groups ascend strictly by `(chain, emitter)`. Sequences ascend strictly inside a
group. `MAX_BATCH_ENTRIES` does not apply here: the cost is one CPI per bucket,
plus a second nested CPI for each bucket the NoReplay program still has to
create, so the ceiling depends on which buckets already exist on chain. The
bucket account list is the operator's bound. A bucket holds 1024 bits: the
bucket index is `sequence / 1024`, and the bit index is `sequence % 1024`. The
handler ORs the bits of consecutive entries that share one bucket into one
128-byte mask. It then sends one `MarkUsedBulk` CPI per bucket.

The caller passes one bucket account per bucket, in walk order. A short or a
long bucket list raises `InvalidInstructionData`.

The handler also emits one `ACCDGST\0` commit-log record per entry, with
`UNPINNED_GUARDIAN_SET_INDEX` (0) in the guardian-set field. The wormchain
snapshot does not record the signing set, so an auditor checks such a record
against the VAA archive.

## Authority and build

`BACKFILL_AUTHORITY` is a `[u8; 32]` that
`const_crypto::bs58::decode_pubkey(env!("BACKFILL_AUTHORITY"))` decodes at
compile time. A missing variable is a build error, so every artifact names one
operator key. The NTT backfill reads a separate variable,
`NTT_BACKFILL_AUTHORITY`, so the two migrations run under their own keys.

`GLOBAL_ACCOUNTANT_PROGRAM_ID` is the second compile-time pin. The assert after
`declare_id!` fails the build when the two disagree, so one artifact can target
one program account only.

Run the recipes below from `svm/accountant`.

| Recipe | Result |
|---|---|
| `just build-devnet` | Test artifact. The `justfile` supplies the test values. |
| `just build` | Deploy artifact. The caller's environment supplies every name in `DEPLOY_VARS`. |

`just build` prints the values it compiles in. It aborts with the list of
missing names when the caller sets none. `just build-prod` is an alias of
`just build`.

Check a deploy artifact against the intended operator key:

```
just verify-authority target/deploy/global_accountant_backfill.so <pubkey> global-accountant-backfill
```

The recipe runs the artifact's own authority gate inside a mollusk-hosted SBF
VM. The third argument names the crate that the `.so` came from, because the
check loads the artifact at that crate's `declare_id!` address.

## Security model

**One authority, pinned at compile time.** `require_authority` is the only gate
on every write. The shell crate decodes `BACKFILL_AUTHORITY` from its `env!`
variable at compile time, so the build sets the key that the artifact enforces.
A missing variable is a build error. `just verify-authority` replays the gate of a built `.so` against
an operator key.

**No governance proof on a write.** The operational program takes a
guardian-signed VAA for each state change. This program takes none. The operator
key replaces the guardian signature for the length of the migration, and the
off-chain parity check is what holds the operator to the snapshot.

**Every write re-derives its address.** Each handler builds the layout from the
entry, and `pda::check` derives the PDA address from that layout's own key. A
substituted account raises `InvalidPda` before the write. The NoReplay path
re-derives the authority PDA and the bucket PDA from the entry's own namespace.

**The address check binds the address, and the address alone.** Only the key
fields of a layout are seeds. The operator picks the value fields, and the
snapshot is what holds them to Wormchain. The off-chain parity check is where
the values get their check.

**Creation is create-only.** `pda::create` goes through
`create_pda_allow_prefund`, which raises `InvalidPda` when the account already
holds data or carries another owner. A replayed batch therefore fails at its
first entry instead of overwriting state.

**Check order.** Each handler checks the authority before it parses the batch.
An unauthorised caller cannot reach the parser, and a rejected instruction
writes nothing.

**No on-chain finalize marker.** A Solana program cannot iterate its own PDAs,
so no on-chain check can state that the snapshot is complete. The off-chain
parity check is the gate. See the cutover procedure below.

**Account layout.** Every account carries a 1-byte `AccountTag` at offset 0.
Handlers read state with `UncheckedAccount` plus `bytemuck`, not Anchor's
`#[account(zero_copy)]`.

## Snapshot tooling contract

The snapshot tooling lives outside this repository. It reads the wormchain dump
and emits the four batch formats above. The wire formats are the contract
between that tooling and this program. The rules below complete that contract.
Each rule is an item for the off-chain parity check.

**One `RegisterChain` VAA per chain.** Exactly one `RegisterChain` VAA per chain
must have succeeded on wormchain. Check this against the VAA archive before the
backfill. The backfill arms one `RegisterChain` record per chain. An earlier
accepted registration for the same chain would stay replayable, because
`register_chain` overwrites the `ChainRegistration` PDA on any unused sequence.

**`BackfillNoReplay` source.** The entries come from the wormchain `DIGESTS`
map. Exclude every row whose key is `(chain 1, GOVERNANCE_EMITTER)`. A
governance sequence is a random number, not a counter. A marked governance
sequence pre-burns a bit that `upgrade_contract` consults. Each such sequence
also costs one sparse bucket account.

**`RegisterChain` record sequence.** The `sequence` field is the sequence of the
governance VAA that registered the emitter. The snapshot holds the emitter only,
so the value comes from the VAA archive.

**`ModifyBalance` record sequence.** The `sequence` field is the modification
sequence in the governance payload (`AllModifications` on wormchain). It is not
the VAA sequence.

**`PendingObservations` is not migrated.** An in-flight partial quorum on
wormchain restarts from zero on Solana. The guardians re-observe the message
after the cutover.

**`UNPINNED_GUARDIAN_SET_INDEX` is 0.** A backfilled `ACCDGST\0` record carries 0
in its guardian-set field, because the snapshot does not record the signing set.
Mainnet guardian set 0 expired long ago, and the program rejects an expired set.
No operational record can therefore carry 0. A consumer of the `ACCDGST` log
reads 0 as "backfilled; audit against the VAA archive".

**Upstream check order.** The operational program changed its observation check
order after the wire formats became final. No layout, seed, tag or arithmetic
changed, so a snapshot from the earlier order stays valid.

## Cutover

The migration has no on-chain finalize marker, by design. The off-chain parity
check is the gate. The operator runs it first, and then each guardian runs it
independently against its own copy of the snapshot.

The operational program accepts Solana-targeted governance only. A governance VAA
issued against Wormchain after the snapshot cut therefore has no path onto
Solana. The freeze in step 1 is what stops such a VAA from existing. Keep the
freeze until the operational image is live.

CAUTION: `solana program upgrade` fails when the program-data account is too
small for the new image. Size the account at the first deploy.

Procedure:

1. Freeze accountant governance on Wormchain.
2. Take the Wormchain snapshot.
3. Put every governance VAA that landed before the freeze into the batches below.
4. Build the deploy artifact with `just build`.
5. Check the artifact with `just verify-authority`, against the operator key.
6. Measure both images with `ls -l target/deploy/*.so`.
7. Deploy the backfill artifact at the WTT program id. Give `--max-len` at least
   the size of the operational image.
8. Send the `BackfillNoReplay` batches.
9. Send the `BackfillBalance` batches.
10. Send the `BackfillModifyBalance` batches.
11. Send the `BackfillChainRegistration` batches.
12. Run the off-chain parity check as the operator.
13. Collect a parity result from every guardian.
14. Upgrade the account to `global_accountant.so` with `solana program upgrade`.
15. Move upgrade authority to the program's `[b"upgrade"]` PDA with
    `solana program set-upgrade-authority`.
16. Lift the governance freeze. A new VAA targets Solana.
17. Tell the guardians to start signing.

After step 15 the deploy key can no longer upgrade the program. Each later
upgrade needs a guardian-signed `UpgradeContract` VAA, through the program's own
`upgrade_contract` instruction.

At this commit the two images measure 146,312 bytes (backfill) and 236,088 bytes
(operational). Both numbers move with every code change, so step 6 measures them
again.

Steps 8 to 11 are independent of each other. The order above is the order the
surfpool lifecycle test uses.

## Scale

The cost probes read a staged catalogue of the mainnet wormchain snapshot at
height 18,669,029. That snapshot holds 5,516,669 transfer rows, 17,367 account
rows, 40 chain registrations, and 6 balance modifications. The probes measure the
fee, the compute units and the rent per transaction. They then extrapolate to
those totals. They are operator tooling, not a regression gate.

## Errors

The program returns `GlobalAccountantError` codes as
`ProgramError::Custom(code)`. It declares no `#[error_code]` enum of its own, so
Anchor's `+6000` offset does not apply.

| Code | Name | Cause |
|---|---|---|
| 1 | `InvalidInstructionData` | A malformed batch, or a PDA account count that the entry count does not match. |
| 2 | `InvalidPda` | The PDA address is not the derived address, or the account already holds data. |
| 25 | `InvalidModificationKind` | A `ModifyBalance` entry carries a `kind` byte other than 1 or 2. |
| 48 | `UnauthorizedCaller` | The payer is not `BACKFILL_AUTHORITY`. |

Anchor raises three more codes before a handler runs:

| Code | Name | Cause |
|---|---|---|
| 101 | `InstructionFallbackNotFound` | Empty instruction data, or a discriminator above 3. |
| 3010 | `AccountNotSigner` | The payer account carries no signature. |
| 4100 | `DeclaredProgramIdMismatch` | The runtime executes the image at another address. |

## Testing

- `just test` — the mollusk suites and the unit tests. The suites cover the wire
  parsers, all four handlers, and the program-id pin. The artifact check runs
  the built `.so`'s authority gate in an SBF VM: the artifact accepts the
  compiled-in key and rejects every other key.
- `just e2e-backfill` — the surfpool lifecycle. All four instructions run
  against a real validator and the real NoReplay program. The test checks every
  write for owner, length, rent and layout. It also checks that a stranger's
  instruction rejects with `UnauthorizedCaller` and writes nothing.
- `just e2e-backfill-probe` — the cost probes. They need the snapshot catalogue
  staged, and they print a skip message when it is absent.
- `just e2e` — every surfpool suite of the workspace, including the two above.

## See also

- [`global-accountant`](../global-accountant/README.md) — the operational
  program this image seeds, and the instructions that read the state written
  here.
- [`ntt-global-accountant-backfill`](../ntt-global-accountant-backfill/README.md)
  — the sibling migration image for the NTT Global Accountant. It shares these
  four handlers and adds two NTT-only arms.
