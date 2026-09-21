# NTT Global Accountant

The NTT Global Accountant is a Solana Anchor port of the wormchain NTT Global
Accountant CosmWasm contract. It tracks a balance per `(chain, hub_chain,
hub_address)` triple, where the hub is the locking transceiver of one NTT
network. Each NTT transfer changes two of these balances. The program never
lets a chain move more of a hub's token than the guardian network has
attested for it.

The program accepts two kinds of input: signed VAAs and pre-quorum guardian
observations. It also accepts NTT registration messages from transceivers,
and governance actions gated on a fixed governance emitter address.

The program shares its machinery with the sibling `global-accountant`
program through the `accountant-operational-core` crate. The two programs
run under separate program IDs, so their PDAs never collide.

## Routing

An NTT message names its transceiver two ways. A transceiver that publishes
directly is the VAA emitter. A transceiver that publishes through the
Wormhole Standard Relayer is the `sender` inside the relayer's
`DeliveryInstruction`, and the relayer contract is the VAA emitter. For every
VAA-carrying message the program resolves the transceiver the same way: when
the emitter is the registered relayer of its chain, it unwraps the envelope;
otherwise the emitter is the transceiver. A guardian observation carries the
resolved `sender` as a field instead; the program checks that `sender` differs
from the emitter exactly when the emitter is the registered relayer.

Two registries route a transfer:

- `TransceiverHub` at `(chain, transceiver)` names the hub the transceiver
  belongs to. A hub names itself. The hub's `(chain, address)` is the token
  identity of every balance the transceiver touches.
- `TransceiverPeer` at `(chain, transceiver, dest_chain)` names the
  transceiver's counterparty on `dest_chain`. A transfer moves balances only
  when the sender's entry names the peer and the peer's entry names the
  sender.

NoReplay keys on the VAA emitter and sequence, as on wormchain. A relayed
message therefore occupies the relayer's sequence slot.

## Instructions

| # | Name | Purpose |
|---|------|---------|
| 0 | `submit_observations` | Accumulate guardian signatures before a VAA exists. |
| 1 | `close_pending` | Reclaim rent from a pending-observations PDA. |
| 2 | `submit_vaas` | Apply a fully signed NTT transfer VAA. |
| 3 | `register_relayer_chain` | Governance: set the Standard Relayer emitter for a chain. |
| 4 | `modify_balance` | Governance: apply a manual balance correction. |
| 5 | `register_hub` | Transceiver message: a locking transceiver registers itself as a hub. |
| 6 | `register_peer` | Transceiver message: a transceiver registers its peer on another chain. |
| 7 | `upgrade_contract` | Governance: replace this program's code. |

Each account table below lists write and signer flags as they apply on
Solana: **W** marks a writable account, **S** marks a required signer.

### 0. `submit_observations`

Guardians gossip observations of one message before a VAA exists. Each
observation adds one signature to a pending PDA keyed on `(chain, emitter,
sequence, guardian_set_index, digest)`. The observation that reaches quorum
commits the transfer, marks NoReplay, and closes the pending PDA.
`submit_vaas` shares this NoReplay state: each `(chain, emitter, sequence)`
commits once through either path.

The observation is a fixed 219-byte struct. The guardian resolves the
transceiver and copies the raw trimmed amount; the program normalizes the
amount to eight decimals at quorum. The signing prefix is
`ntt_acct_sub_obsfig_00000000000000|`.

The program checks two NTT rules before it recovers the signature. The
`sender` field differs from the emitter only when the emitter is the
registered relayer of its chain. The sender must have a hub.

| # | Account | W | S | Purpose |
|---|---------|---|---|---------|
| 0 | submitter | W | S | Pays rent for the pending PDA. |
| 1 | pending PDA | W | | Accumulates guardian signatures. |
| 2 | Core Bridge `GuardianSet` PDA | | | Checks the guardian signature. |
| 3 | NoReplay bitmap PDA | W | | Marked on quorum. |
| 4 | system program | | | |
| 5 | NoReplay program | | | |
| 6 | NoReplay authority PDA | | | This program's NoReplay CPI authority. |
| 7 | source-chain balance PDA | W | | For the hub token; checked on quorum. |
| 8 | recipient-chain balance PDA | W | | As above. |
| 9 | rent recipient | W | | Must equal the pending PDA's recorded payer. |
| 10 | relayer `ChainRegistration` PDA | | | For the emitter chain; can be absent. |
| 11 | `TransceiverHub` PDA | | | At `(chain, sender)`; must exist. |
| 12 | `TransceiverPeer` PDA | | | At `(chain, sender, recipient_chain)`. |
| 13 | `TransceiverPeer` PDA | | | At `(recipient_chain, peer, chain)`; must name the sender. |

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

Applies one fully signed NTT transfer VAA. The Verify VAA Shim checks the
guardian signatures. The program resolves the transceiver, parses the
`TransceiverMessage`, loads the transceiver's hub, marks NoReplay, and moves
the hub token's balances after the peer checks.

The transfer parser is stricter than the wormchain reader. It checks every
nested length field and rejects trailing bytes, as the EVM receivers do.

| # | Account | W | S | Purpose |
|---|---------|---|---|---------|
| 0 | submitter | W | S | Pays rent for a new NoReplay bucket. |
| 1 | Verify VAA Shim program | | | |
| 2 | Core Bridge `GuardianSet` PDA | | | |
| 3 | `GuardianSignatures` PDA | | | Posted through the shim before this call. |
| 4 | NoReplay bitmap PDA | W | | |
| 5 | NoReplay program | | | |
| 6 | NoReplay authority PDA | | | |
| 7 | source-chain balance PDA | W | | For the hub token. |
| 8 | recipient-chain balance PDA | W | | As above. |
| 9 | system program | | | |
| 10 | relayer `ChainRegistration` PDA | | | For the emitter chain; can be absent. |
| 11 | `TransceiverHub` PDA | | | At `(chain, sender)`; must exist. |
| 12 | `TransceiverPeer` PDA | | | At `(chain, sender, recipient_chain)`. |
| 13 | `TransceiverPeer` PDA | | | At `(recipient_chain, peer, chain)`; must name the sender. |

### 3. `register_relayer_chain`

Governance action with the `WormholeRelayer` module. Writes or overwrites
the `ChainRegistration` PDA for one chain with the Standard Relayer emitter
the VAA carries. A per-sequence `RegisterChain` PDA is the replay guard. The
account list equals the `global-accountant` `register_chain` instruction.

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

Governance action with the `NTTGlobalAccountant` module. Applies an `Add`
or `Subtract` delta to one balance PDA. `Add` on an absent PDA creates it
with `balance = amount`. `Subtract` on an absent PDA, or past the current
balance, fails. A per-sequence `ModifyBalance` PDA is the replay guard and
the audit record.

| # | Account | W | S | Purpose |
|---|---------|---|---|---------|
| 0 | payer | W | S | Pays rent for both PDAs below. |
| 1 | Verify VAA Shim program | | | |
| 2 | Core Bridge `GuardianSet` PDA | | | |
| 3 | `GuardianSignatures` PDA | | | |
| 4 | `BalanceAccount` PDA | W | | Adjusted by the delta. |
| 5 | system program | | | |
| 6 | `ModifyBalance` PDA | W | | Replay guard and audit record, keyed on sequence. |

### 5. `register_hub`

Transceiver message. A `WormholeTransceiverInfo` VAA in `Locking` mode
registers its transceiver as a hub that names itself. The program rejects a
`Burning` mode message and leaves the slot free. Guardian quorum is the only
authentication; NoReplay on the emitter's sequence is the replay guard.

| # | Account | W | S | Purpose |
|---|---------|---|---|---------|
| 0 | payer | W | S | Pays rent for the hub PDA. |
| 1 | Verify VAA Shim program | | | |
| 2 | Core Bridge `GuardianSet` PDA | | | |
| 3 | `GuardianSignatures` PDA | | | |
| 4 | relayer `ChainRegistration` PDA | | | For the emitter chain; can be absent. |
| 5 | `TransceiverHub` PDA | W | | At `(chain, sender)`; must not exist. |
| 6 | NoReplay bitmap PDA | W | | |
| 7 | NoReplay program | | | |
| 8 | NoReplay authority PDA | | | |
| 9 | system program | | | |

### 6. `register_peer`

Transceiver message. A `WormholeTransceiverRegistration` VAA registers the
sender's peer on `dest_chain`. The peer must be on another chain, and the
entry must not exist yet. The program then applies the wormchain rules on
the pair `(sender hub, peer hub)`:

- Neither has a hub: rejected.
- Only the sender has a hub: the sender must be a hub itself. This is how a
  hub pre-registers a spoke.
- Only the peer has a hub: the peer must be a hub itself, and the hub must
  already name the sender as its peer on the sender's chain. The sender then
  adopts that hub. This is how a spoke joins a network.
- Both have hubs: the hubs must be equal.

| # | Account | W | S | Purpose |
|---|---------|---|---|---------|
| 0 | payer | W | S | Pays rent for the new PDAs. |
| 1 | Verify VAA Shim program | | | |
| 2 | Core Bridge `GuardianSet` PDA | | | |
| 3 | `GuardianSignatures` PDA | | | |
| 4 | relayer `ChainRegistration` PDA | | | For the emitter chain; can be absent. |
| 5 | sender's `TransceiverHub` PDA | W | | At `(chain, sender)`; written only on adoption. |
| 6 | peer's `TransceiverHub` PDA | | | At `(dest_chain, peer)`; can be absent. |
| 7 | peer's `TransceiverPeer` PDA | | | At `(dest_chain, peer, chain)`; address always checked, contents read on adoption. |
| 8 | `TransceiverPeer` PDA | W | | At `(chain, sender, dest_chain)`; must not exist. |
| 9 | NoReplay bitmap PDA | W | | |
| 10 | NoReplay program | | | |
| 11 | NoReplay authority PDA | | | |
| 12 | system program | | | |

### 7. `upgrade_contract`

Governance action with the `NTTGlobalAccountant` module. Replaces this
program's code through the BPF upgradeable loader, using a buffer the VAA
names. The shared NoReplay bitmap is the replay guard, keyed on the fixed
governance emitter's address and chain. The account list equals the
`global-accountant` `upgrade_contract` instruction.

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

**Governance gating.** `register_relayer_chain`, `modify_balance`, and
`upgrade_contract` each require a VAA from Solana chain 1, signed by the
fixed governance emitter address, with the module the instruction expects.
The Verify VAA Shim and this check run before any state change.

**Registration is permissionless but ordered.** Any transceiver can send a
hub or peer registration; guardian quorum is the only signature. The
four-way rule in `register_peer` is what keeps a network closed. A spoke
adopts a hub only after the hub has named the spoke. Two rogue transceivers
cannot inherit a hub by registering each other.

**Transfers need both peers.** `submit_vaas` and `submit_observations` load
the sender's peer entry and the peer's entry for the sender's chain. A
transceiver writes only its own entries, so one transceiver cannot move a
hub's balances to a counterparty that did not register it.

**Hub gate before signature work.** `submit_observations` rejects a sender
with no hub before it recovers the guardian signature, as the
`global-accountant` program rejects an unregistered emitter.

**One PDA toolkit.** Every program-owned PDA has one key type that holds its
seeds. Address checks, existence probes, and creation all go through the
shared `pda` module in `accountant-operational-core`. This program must own
every initialised PDA; any other owner is an error.

**Account layout.** Every account carries a 1-byte `AccountTag` at offset 0.
Handlers read state with `UncheckedAccount` plus `bytemuck`, not Anchor's
`#[account(zero_copy)]`. PDA creation goes through
`CreateAccountAllowPrefund`, since `#[account(init)]` fails on a prefunded
PDA.

## Testing

- `just test` — the Mollusk integration suite for both programs, plus unit
  tests. The NTT suite includes all 28 mainnet NTT vectors, booked through
  `submit_vaas` and through observation quorum.
- `just e2e` — end-to-end tests for both programs against a real `surfpool`
  instance and the real Verify VAA Shim. `just e2e-ntt` runs the NTT
  lifecycle alone: hub, peers, relayer registration, a relayed transfer, an
  observation quorum, and a balance correction.
- `just e2e-upgrade-deploy ntt-global-accountant` /
  `just e2e-upgrade-submit ntt-global-accountant` /
  `just e2e-upgrade-stop ntt-global-accountant` — the two-step
  `upgrade_contract` end-to-end test for this program.
