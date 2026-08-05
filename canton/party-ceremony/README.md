# Guardian Party Ceremony

A small CLI for the multi-actor ceremonies that create and maintain the
guardian decentralized namespace and its two parties on Canton:

- **`guardianGovernance`** — the guardians' k-of-n external party
  (threshold signing keys, hosted at Confirmation on every guardian
  participant)
- **`guardianObserver`** — the keyless read party (hosted at Observation on
  every guardian participant; a purely observing party)

It is the production successor to the single-session devnet prototype in
[`../devnet/guardian_governance.canton`](../devnet/guardian_governance.canton):
same topology recipe and the same pluggable custody boundary, restructured so
**independent operators run it from their own machines at their own times** —
no shared session, no joint key ceremony. The operational shape is a standard
one for decentralized-party ceremonies: async init/resume, an append-only
report cache, threshold gates, exit code 2 = "come back later", and ceremony
state shared as files a Git PR can carry.

## How it works

A ceremony is a directory holding two files:

| File | Mutability | Content |
|---|---|---|
| `workflow.json` | immutable | the spec: owners (id + Ed25519 public key), threshold, coordinator |
| `reports.json` | append-only | one entry per completed operation (the idempotency cache) |

One operator runs `init`; everyone (including the initiator) then runs
`resume` whenever it is their turn. `resume` executes exactly the steps that
are (a) this actor's responsibility, (b) unblocked, and (c) not already
recorded — then exits `0` if the ceremony is complete or `2` if other actors
must act first. Re-running is always safe; nothing executes twice.

Onboarding step plan (op key → responsible actor):

```
identify/<owner>              each owner    record own participant uid
delegate/<owner>              each owner    self-signed root NamespaceDelegation
dns/prepare                   coordinator   DecentralizedNamespaceDefinition (unsigned)
dns/sign/<owner>              each owner    signature over the DNS hash
dns/submit                    coordinator   submit (creation needs ALL owners)
host/governance/prepare       coordinator   PartyToParticipant: keys@threshold, Confirmation everywhere
host/governance/sign/<o>      each owner    authorization signature (threshold gate)
host/governance/consent/<o>   each owner    own participant's consent-to-host
host/governance/submit        coordinator   submit at >= threshold sigs + every consent
host/observer/...             same shape    keyless party, Observation everywhere
artifacts                     coordinator   final record (namespace, party ids, participants)
```

Before signing any hash, a guardian re-derives it from the transaction body
and checks the decoded transaction against the spec (owner keys, threshold,
party name, permission) via `Topology.Describe` — the store is untrusted
shared state, so a tampered `prepare` entry is refused, never blind-signed.

Onboarding requires **every** owner to take part: namespace creation needs all
owners' signatures and hosting needs every participant's consent. The k-of-n
threshold governs the parties' ongoing authorization — the governance party is
hosted at Confirmation with a confirmation threshold equal to the guardian
threshold, and carries the same k-of-n signing keys — not attendance at the
one-time bootstrap. A guardian that misses onboarding is added afterwards by a
separate add-guardian ceremony.

## Design

Ports-and-adapters with constructor injection throughout; every collaborator
is a small interface with a fake that carries real business logic (no mocks):

| Port | Real implementations | Fake |
|---|---|---|
| `ceremony.Topology` | `topology/console` — drives `dpm canton-console` per op (the proven `guardian_governance.canton` recipe, split into prepare/describe/consent/submit) | `fake.Participant` on a shared `fake.Ledger` that enforces Canton's authorization rules — rooted namespaces, all-owners namespace creation, threshold party authorization, per-host consent — and verifies **real Ed25519 signatures** |
| `ceremony.Signer` | `sign.KeySigner` (local key file), `sign.CmdSigner` (external custody command: hex hash in, hex signature out — the same contract as `GG_OWNER_SIGN_CMD` / `guardian_key_tool.js`) | in-memory Ed25519 |
| `ceremony.Store` | `store.FS` (atomic `workflow.json` / `reports.json`) | `fake.Store` |

The domain (`ceremony` package) is pure stdlib and knows nothing about Canton
wire formats: prepared transactions are opaque `(bytes, hash)` pairs, so the
topology backend can move from the console recipe to an Admin API gRPC client
without touching the workflow.

## Testing

```
go test ./...        # everything below; -race clean
```

- **Multi-guardian e2e** (`ceremony/e2e_test.go`): the production shape —
  seven guardians, threshold 5 — advancing in random order with a **fresh
  workflow object per turn** (every turn is a simulated process restart),
  verified down to per-host permissions on the resulting ledger; a stall
  proof that the ceremony cannot complete while any owner never acts; and an
  on-disk variant running the CLI's exact wiring (FS store, file-backed
  ledger, key-file signers).
- **CLI multi-process e2e** (`cmd/ceremony/main_test.go`): compiles the real
  binary and runs a five-guardian ceremony as separate OS processes — one
  invocation per guardian turn, one guardian signing through the external
  custody-command path — asserting the exit-code contract (2 until complete,
  0 on completion, idempotent re-run) and the final artifacts.
- **Authorization-rule tests** (`ceremony/fake/topology_test.go`): forged
  delegation rejected; namespace creation refuses 2-of-3 signatures and
  accepts 3-of-3; hosting refuses sub-threshold signatures, missing consents,
  and consents from unknown participants; idempotent re-submits.
- Adapter unit tests for signers (round-trip verify, custody-command
  contract) and the FS store (idempotent/conflicting writes, restart
  visibility).
- **Real-Canton e2e** (`topology/console/e2e_test.go`, build tag
  `canton_e2e`): drives the onboarding state machine against a live Canton
  sandbox through the console adapter — the actual topology operations — and
  asserts the namespace and both guardian parties end up on the synchronizer.
  Requires a running sandbox and is opt-in:

  ```sh
  dpm sandbox --no-tty &                       # in a scratch dir
  go test -tags canton_e2e -timeout 20m ./topology/console
  ```

  It is skipped automatically when no sandbox is listening on `localhost:6865`.

## Usage (rehearsal walkthrough)

```sh
go build -o ceremony ./cmd/ceremony

# each guardian, on its own machine:
./ceremony keygen --out guardian-1            # -> guardian-1.key / guardian-1.pub

# coordinator collects the .pub files and initializes:
./ceremony init --dir ./wf --id onboarding-1 --threshold 5 --coordinator guardian-1 \
  --owner guardian-1=guardian-1.pub --owner guardian-2=guardian-2.pub ... 

# each guardian, whenever it is their turn (exit 2 = come back later):
./ceremony resume --dir ./wf --actor guardian-2 --key guardian-2.key \
  --backend fake --ledger ./ledger.json
# or through external custody:
./ceremony resume --dir ./wf --actor guardian-3 \
  --sign-cmd 'my-hsm-sign --key-id abc' --backend fake --ledger ./ledger.json

# against real Canton (drives dpm canton-console via the console adapter):
./ceremony resume --dir ./wf --actor guardian-2 --key guardian-2.key \
  --backend console --ops-script ./topology/console/ops.canton

./ceremony status --dir ./wf
```

In the production workflow the ceremony directory travels through a Git
repository (one PR per ceremony; operators pull, `resume`, push), and
`--backend console` points at a real participant.

## Status & next steps

- **Implemented and proven on real Canton:** the onboarding workflow end to
  end (fake backend *and* the `topology/console` adapter against a live
  sandbox), both signer paths, the FS store, the CLI (`--backend console`),
  and the full test suite above including the real-Canton e2e.
- **Single-participant scope today.** The console adapter hosts on the one
  connected participant (the sandbox), so the confirmation threshold is 1 and
  the hosting participant applies its own consent at submit. The domain
  already computes the confirmation threshold as `min(guardian threshold, host
  count)` and dedups hosts, so multi-participant is a backend concern.
- **Next: multi-participant rehearsal** on the NTT playground's Splice
  LocalNet (dedicated guardian-governance and guardian-observer participants),
  which needs the adapter to target multiple participants (per-participant
  `-c` config or the gRPC Admin-API adapter) so each host signs its own
  consent — the true k-of-n, multi-node proof. The port is shaped so this does
  not touch the domain or its tests.
- Later workflows on the same engine: `add-guardian` (with ACS import),
  `kick-guardian`, `rotate-namespace-key`, and the genesis contract-deploy
  ceremony (operator + guardians 2-of-2 via interactive submission).
