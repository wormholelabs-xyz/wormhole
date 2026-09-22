# NTT Global Accountant Backfill

## Purpose

This program is the one-shot migration image for the NTT Global Accountant. It
seeds the operational program's state from a wormchain `query_all_accounts`
snapshot. It has one instruction per state kind: NoReplay bits, `BalanceAccount`
PDAs, `ModifyBalance` records, the Standard Relayer `ChainRegistration` state,
the `TransceiverHub` map, and the `TransceiverPeer` map.

The image occupies the operational program account
`cGfHiC6Kgg3FpFZvgwGcswsCRtp4aBP2fzuXRQPizuN`. `declare_id!` carries that
address, and a `const _` assert pins it to `NTT_GLOBAL_ACCOUNTANT_PROGRAM_ID`.
The operator ends the migration with `solana program upgrade`, which installs
`ntt_global_accountant.so` in the same account.

Every handler comes from `accountant-backfill-core`. Each handler writes through
the `operational-core` PDA helpers that the operational program itself uses. The
bytes this program writes therefore equal the bytes the operational program
reads.

## Program layout

```
global-accountant-definitions
  <- accountant-operational-core
    <- accountant-backfill-core
      <- ntt-global-accountant-backfill
```

An operational program never depends on `accountant-backfill-core`. A backfill
handler therefore cannot enter a live dispatch table and still compile.

The shell crate holds the Anchor boundary only:

- `declare_id!` and the program-id assert, in `src/lib.rs`.
- `NTT_BACKFILL_AUTHORITY`, in `src/lib.rs`.
- The `flatten_accounts!` macro, in `src/lib.rs`.
- Six `#[derive(Accounts)]` contexts, in `src/contexts.rs`.
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
| 3 | `backfill_relayer_chain_registration` | `backfill_chain_registration` | Two: `ChainRegistration`, then `RegisterChain` |
| 4 | `backfill_transceiver_hub` | `backfill_transceiver_hub` | One `TransceiverHub` |
| 5 | `backfill_transceiver_peer` | `backfill_transceiver_peer` | One `TransceiverPeer` |

Discriminators 0 to 3 mirror the sibling `global-accountant-backfill` program,
which shares the four handlers. Discriminators 4 and 5 are NTT-only.

### PDAs

| PDA | Tag | Seeds | Bytes |
|---|---|---|---|
| `BalanceAccount` | 2 | `b"account"`, chain, token_chain, token_address | 70 |
| `ChainRegistration` | 3 | `b"chain_registration"`, chain | 64 |
| `ModifyBalance` | 4 | `b"modify_balance"`, sequence | 112 |
| `RegisterChain` | 5 | `b"register_chain"`, sequence | 48 |
| `TransceiverHub` | 6 | `b"transceiver_hub"`, chain, address | 70 |
| `TransceiverPeer` | 7 | `b"transceiver_peer"`, chain, address, dest_chain | 70 |

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
| 0 | payer | W | S | Must equal `NTT_BACKFILL_AUTHORITY`. |
| 1 | NoReplay program | | | The runtime loads it; the CPI target is the constant `NOREPLAY_PROGRAM_ID`. |
| 2 | NoReplay authority PDA | | | This program's `[b"noreplay_authority"]` PDA. |
| 3 | system program | | | |
| 4.. | NoReplay bitmap bucket | W | | One per bucket, in walk order. |

The other five instructions:

| # | Account | W | S | Purpose |
|---|---------|---|---|---------|
| 0 | payer | W | S | Must equal `NTT_BACKFILL_AUTHORITY`. |
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
with `MaxInstructionTraceLengthExceeded`. The heap gives a lower practical
ceiling for the wider entries; `BackfillBalance` and `BackfillModifyBalance` are
measured at 58 entries per transaction.

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

`BackfillRelayerChainRegistration` entry, 42 bytes. Sort key `chain`:

| offset | size | field |
|---|---|---|
| 0 | 2 | chain |
| 2 | 8 | sequence |
| 10 | 32 | emitter |

`BackfillTransceiverHub` entry, 68 bytes. Sort key `(chain, address)`:

| offset | size | field |
|---|---|---|
| 0 | 2 | chain |
| 2 | 32 | address |
| 34 | 2 | hub_chain |
| 36 | 32 | hub_address |

An entry does not have to name itself as its own hub. The snapshot's
`transceiver_to_hub` map also holds spokes, which the operational
`register_peer` adoption arm wrote.

`BackfillTransceiverPeer` entry, 68 bytes. Sort key `(chain, address,
dest_chain)`:

| offset | size | field |
|---|---|---|
| 0 | 2 | chain |
| 2 | 32 | address |
| 34 | 2 | dest_chain |
| 36 | 32 | peer_address |

An entry with `dest_chain` equal to `chain` raises `SameChainPeer`. This is the
error the operational `register_peer` raises for the same shape.

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
bucket account list is the operator's bound; 30 buckets per transaction is
measured to pass. A bucket holds 1024 bits: the bucket index is `sequence / 1024`, and the
bit index is `sequence % 1024`. The handler ORs the bits of consecutive entries
that share one bucket into one 128-byte mask. It then sends one `MarkUsedBulk`
CPI per bucket.

The caller passes one bucket account per bucket, in walk order. A short or a
long bucket list raises `InvalidInstructionData`.

The handler also emits one `ACCDGST\0` commit-log record per entry, with
`UNPINNED_GUARDIAN_SET_INDEX` (0) in the guardian-set field. The wormchain
snapshot does not record the signing set, so an auditor checks such a record
against the VAA archive.

## Authority and build

`NTT_BACKFILL_AUTHORITY` is a `[u8; 32]` that
`const_crypto::bs58::decode_pubkey(env!("NTT_BACKFILL_AUTHORITY"))` decodes at
compile time. A missing variable is a build error, so every artifact names one
operator key. The WTT backfill reads a separate variable, `BACKFILL_AUTHORITY`,
so the two migrations run under their own keys.

`NTT_GLOBAL_ACCOUNTANT_PROGRAM_ID` is the second compile-time pin. The assert
after `declare_id!` fails the build when the two disagree, so one artifact can
target one program account only.

Run the recipes below from `svm/accountant`.

| Recipe | Result |
|---|---|
| `just build` | Test artifact. The `justfile` supplies the test keys. |
| `just build-prod` | Deploy artifact. The caller's environment supplies every name in `DEPLOY_VARS`. |

`just build-prod` prints the values it compiles in. It aborts with the list of
missing names when the caller sets none.

Check a deploy artifact against the intended operator key:

```
just verify-authority target/deploy/ntt_global_accountant_backfill.so <pubkey> ntt-global-accountant-backfill
```

The recipe runs the artifact's own authority gate inside a mollusk-hosted SBF
VM. The third argument names the crate that the `.so` came from, because the
check loads the artifact at that crate's `declare_id!` address.

## Cutover

The migration has no on-chain finalize marker, by design. A Solana program
cannot iterate its own PDAs, so no on-chain check can state that the snapshot is
complete. The off-chain parity check is the gate. The operator runs it first,
and then each guardian runs it independently against its own copy of the
snapshot.

The snapshot tooling lives outside this repository. It reads the wormchain dump
and emits the six batch formats above. The wire formats are the contract between
that tooling and this program.

CAUTION: `solana program upgrade` fails when the program-data account is too
small for the new image. Size the account at the first deploy.

Procedure:

1. Build the deploy artifact with `just build-prod`.
2. Check the artifact with `just verify-authority`, against the operator key.
3. Measure both images with `ls -l target/deploy/*.so`.
4. Deploy the backfill artifact at the NTT program id. Give `--max-len` at least
   the size of the operational image.
5. Send the `BackfillNoReplay` batches.
6. Send the `BackfillBalance` batches.
7. Send the `BackfillModifyBalance` batches.
8. Send the `BackfillRelayerChainRegistration` batches.
9. Send the `BackfillTransceiverHub` batches.
10. Send the `BackfillTransceiverPeer` batches.
11. Run the off-chain parity check as the operator.
12. Collect a parity result from every guardian.
13. Upgrade the account to `ntt_global_accountant.so` with `solana program
    upgrade`.
14. Tell the guardians to start signing.

At this commit the two images measure 157,888 bytes (backfill) and 294,608 bytes
(operational). Both numbers move with every code change, so step 3 measures them
again.

Steps 5 to 10 are independent of each other. The order above is the order the
surfpool lifecycle test uses.

## Errors

The program returns `GlobalAccountantError` codes as
`ProgramError::Custom(code)`. It declares no `#[error_code]` enum of its own, so
Anchor's `+6000` offset does not apply.

| Code | Name | Cause |
|---|---|---|
| 1 | `InvalidInstructionData` | A malformed batch, or a PDA account count that the entry count does not match. |
| 2 | `InvalidPda` | The PDA address is not the derived address, or the account already holds data. |
| 25 | `InvalidModificationKind` | A `ModifyBalance` entry carries a `kind` byte other than 1 or 2. |
| 42 | `SameChainPeer` | A `TransceiverPeer` entry carries `dest_chain` equal to `chain`. |
| 48 | `UnauthorizedCaller` | The payer is not `NTT_BACKFILL_AUTHORITY`. |

Anchor raises three more codes before a handler runs:

| Code | Name | Cause |
|---|---|---|
| 101 | `InstructionFallbackNotFound` | Empty instruction data, or a discriminator above 5. |
| 3010 | `AccountNotSigner` | The payer account carries no signature. |
| 4100 | `DeclaredProgramIdMismatch` | The runtime executes the image at another address. |

## Testing

- `just test` — the mollusk suites and the unit tests. The NTT backfill suites
  cover the wire parsers, every handler, authority isolation against the WTT
  artifact, and the program-id pin. `operational_parity` loads accounts that the
  backfill `.so` wrote into a mollusk that runs `ntt_global_accountant.so` at the
  same id, and settles a mainnet transfer through `submit_vaas`. The artifact
  check runs the built `.so`'s authority gate in an SBF VM.
- `just e2e-ntt-backfill` — the surfpool lifecycle. All six instructions run
  against a real validator and the real NoReplay program. The test checks every
  write for owner, length, rent and layout.
- `just e2e-ntt-cutover` — the cutover rehearsal. The loader swaps the backfill
  image for the operational image at one address, after which `submit_vaas`
  settles a backfilled transfer and rejects a backfilled sequence with
  `AlreadyAccounted`.
- `just e2e` — every surfpool suite of the workspace, including the two above.
