package ceremony_test

import (
	"context"
	"crypto/ed25519"
	"crypto/rand"
	"crypto/x509"
	"fmt"
	mrand "math/rand"
	"path/filepath"
	"testing"

	"github.com/wormhole-foundation/wormhole/canton/party-ceremony/ceremony"
	"github.com/wormhole-foundation/wormhole/canton/party-ceremony/ceremony/fake"
	"github.com/wormhole-foundation/wormhole/canton/party-ceremony/sign"
	fsstore "github.com/wormhole-foundation/wormhole/canton/party-ceremony/store"
)

// guardian bundles one actor's identity and dependencies for tests.
type guardian struct {
	id     string
	signer ceremony.Signer
	pubDER []byte
	topo   ceremony.Topology
}

type memorySigner struct{ priv ed25519.PrivateKey }

func (m memorySigner) Sign(hashHex string) (string, error) {
	return signHex(m.priv, hashHex)
}

func signHex(priv ed25519.PrivateKey, hashHex string) (string, error) {
	var hash []byte
	if _, err := fmt.Sscanf(hashHex, "%x", &hash); err != nil {
		return "", err
	}
	return fmt.Sprintf("%x", ed25519.Sign(priv, hash)), nil
}

func newGuardians(t *testing.T, n int, topoFor func(id string) ceremony.Topology) []guardian {
	t.Helper()
	guardians := make([]guardian, n)
	for i := range guardians {
		pub, priv, err := ed25519.GenerateKey(rand.Reader)
		if err != nil {
			t.Fatalf("generating key: %v", err)
		}
		pubDER, err := x509.MarshalPKIXPublicKey(pub)
		if err != nil {
			t.Fatalf("encoding key: %v", err)
		}
		id := fmt.Sprintf("guardian-%d", i+1)
		guardians[i] = guardian{id: id, signer: memorySigner{priv: priv}, pubDER: pubDER, topo: topoFor(id)}
	}
	return guardians
}

func specOf(t *testing.T, guardians []guardian, threshold int) ceremony.Spec {
	t.Helper()
	owners := make([]ceremony.Owner, len(guardians))
	for i, g := range guardians {
		owners[i] = ceremony.Owner{ID: g.id, PublicKeyDER: g.pubDER}
	}
	spec, err := ceremony.NewOnboardingSpec("wf-test", guardians[0].id, threshold, owners)
	if err != nil {
		t.Fatalf("building spec: %v", err)
	}
	return spec
}

// advance builds a FRESH Onboarding for the actor — every turn simulates a
// separate process invocation, so resume-from-persisted-state is exercised
// constantly, not as a special case.
func advance(t *testing.T, spec ceremony.Spec, g guardian, st ceremony.Store) ceremony.Status {
	t.Helper()
	flow, err := ceremony.NewOnboarding(spec, g.id, g.topo, g.signer, st)
	if err != nil {
		t.Fatalf("constructing onboarding for %s: %v", g.id, err)
	}
	status, err := flow.Advance(context.Background())
	if err != nil {
		t.Fatalf("advance as %s: %v", g.id, err)
	}
	return status
}

// TestOnboardingSevenGuardians runs the full production-shaped ceremony —
// seven guardians, threshold five — with actors advancing in random order
// until completion, then verifies the resulting topology in detail.
func TestOnboardingSevenGuardians(t *testing.T) {
	ledger := fake.NewLedger()
	guardians := newGuardians(t, 7, func(id string) ceremony.Topology {
		return fake.NewParticipant(ledger, "participant::"+id)
	})
	spec := specOf(t, guardians, 5)
	st := fake.NewStore()

	rng := mrand.New(mrand.NewSource(42))
	complete := false
	for round := 0; round < 50 && !complete; round++ {
		g := guardians[rng.Intn(len(guardians))]
		if advance(t, spec, g, st).Complete {
			complete = true
		}
	}
	if !complete {
		t.Fatalf("ceremony did not complete within the round budget")
	}

	var artifacts ceremony.Artifacts
	ok, err := st.Get("artifacts", &artifacts)
	if err != nil || !ok {
		t.Fatalf("artifacts missing: ok=%v err=%v", ok, err)
	}
	if artifacts.Threshold != 5 {
		t.Errorf("artifacts threshold = %d, want 5", artifacts.Threshold)
	}

	dns, ok := ledger.DNS(artifacts.Namespace)
	if !ok {
		t.Fatalf("decentralized namespace %s not on ledger", artifacts.Namespace)
	}
	if dns.Threshold != 5 || len(dns.OwnerFPs) != 7 {
		t.Errorf("dns = threshold %d over %d owners, want 5 over 7", dns.Threshold, len(dns.OwnerFPs))
	}

	gov, ok := ledger.Party(artifacts.GovernancePartyID)
	if !ok {
		t.Fatalf("governance party %s not on ledger", artifacts.GovernancePartyID)
	}
	if len(gov.Hosts) != 7 {
		t.Errorf("governance party hosted on %d participants, want 7", len(gov.Hosts))
	}
	for uid, perm := range gov.Hosts {
		if perm != ceremony.Confirmation {
			t.Errorf("governance host %s has permission %s, want Confirmation", uid, perm)
		}
	}
	if gov.ConfirmationThreshold != 5 {
		t.Errorf("governance confirmation threshold = %d, want the guardian threshold 5 (not 1)", gov.ConfirmationThreshold)
	}
	if len(gov.SigningKeysDER) != 7 || gov.SigningThreshold != 5 {
		t.Errorf("governance signing keys = %d @ threshold %d, want 7 @ 5", len(gov.SigningKeysDER), gov.SigningThreshold)
	}

	obs, ok := ledger.Party(artifacts.ObserverPartyID)
	if !ok {
		t.Fatalf("observer party %s not on ledger", artifacts.ObserverPartyID)
	}
	if len(obs.Hosts) != 7 {
		t.Errorf("observer party hosted on %d participants, want 7", len(obs.Hosts))
	}
	for uid, perm := range obs.Hosts {
		if perm != ceremony.Observation {
			t.Errorf("observer host %s has permission %s, want Observation", uid, perm)
		}
	}
	if len(obs.SigningKeysDER) != 0 {
		t.Errorf("observer party has %d signing keys, want none (purely observing)", len(obs.SigningKeysDER))
	}

	// Idempotency: advancing again changes nothing and stays complete.
	status := advance(t, spec, guardians[0], st)
	if !status.Complete || len(status.Ran) != 0 {
		t.Errorf("re-advance after completion: complete=%v ran=%v, want complete and empty", status.Complete, status.Ran)
	}
}

// TestOnboardingStallsWithoutEveryOwner proves the ceremony cannot finish
// while any owner has never shown up: namespace creation needs all owners,
// and hosting needs every participant's consent.
func TestOnboardingStallsWithoutEveryOwner(t *testing.T) {
	ledger := fake.NewLedger()
	guardians := newGuardians(t, 7, func(id string) ceremony.Topology {
		return fake.NewParticipant(ledger, "participant::"+id)
	})
	spec := specOf(t, guardians, 5)
	st := fake.NewStore()

	active := guardians[:6] // guardian-7 never participates
	for round := 0; round < 30; round++ {
		for _, g := range active {
			if advance(t, spec, g, st).Complete {
				t.Fatalf("ceremony completed without guardian-7")
			}
		}
	}
	status := advance(t, spec, guardians[0], st)
	if status.Complete {
		t.Fatalf("ceremony reported complete while an owner never acted")
	}
	if _, ok := ledger.Party("guardianGovernance::" + namespaceOnLedger(t, st)); ok {
		t.Fatalf("governance party must not exist before every consent is in")
	}
}

func namespaceOnLedger(t *testing.T, st ceremony.Store) string {
	t.Helper()
	var dns ceremony.PreparedTx
	if ok, err := st.Get("dns/prepare", &dns); err != nil || !ok {
		return "unprepared"
	}
	return dns.Namespace
}

// TestOnboardingAcrossProcessesOnDisk runs the same ceremony through the real
// filesystem store, the file-backed ledger, and file-based key signers — the
// exact wiring the CLI uses — with every turn loading all state from disk, so
// it proves the multi-process (one invocation per guardian per turn) flow.
func TestOnboardingAcrossProcessesOnDisk(t *testing.T) {
	dir := t.TempDir()
	ledgerPath := filepath.Join(dir, "ledger.json")

	guardians := make([]guardian, 5)
	owners := make([]ceremony.Owner, 5)
	for i := range guardians {
		id := fmt.Sprintf("guardian-%d", i+1)
		prefix := filepath.Join(dir, id)
		pubDER, err := sign.GenerateKeyFiles(prefix)
		if err != nil {
			t.Fatalf("generating key files: %v", err)
		}
		signer, err := sign.NewKeySigner(prefix + ".key")
		if err != nil {
			t.Fatalf("loading key signer: %v", err)
		}
		guardians[i] = guardian{
			id:     id,
			signer: signer,
			pubDER: pubDER,
			topo:   fake.NewFileTopology(ledgerPath, "participant::"+id),
		}
		owners[i] = ceremony.Owner{ID: id, PublicKeyDER: pubDER}
	}
	spec, err := ceremony.NewOnboardingSpec("wf-disk", "guardian-1", 3, owners)
	if err != nil {
		t.Fatalf("building spec: %v", err)
	}
	if _, err := fsstore.Init(filepath.Join(dir, "ceremony"), spec); err != nil {
		t.Fatalf("initializing store: %v", err)
	}

	complete := false
	for round := 0; round < 20 && !complete; round++ {
		for _, g := range guardians {
			// Re-open the store every turn: nothing survives in memory
			// between turns, exactly like separate CLI processes.
			st, loadedSpec, err := fsstore.Open(filepath.Join(dir, "ceremony"))
			if err != nil {
				t.Fatalf("re-opening store: %v", err)
			}
			if advance(t, loadedSpec, g, st).Complete {
				complete = true
				break
			}
		}
	}
	if !complete {
		t.Fatalf("on-disk ceremony did not complete")
	}

	inspector := fake.NewFileTopology(ledgerPath, "inspector")
	ledger, err := inspector.Inspect()
	if err != nil {
		t.Fatalf("inspecting ledger: %v", err)
	}
	st, _, err := fsstore.Open(filepath.Join(dir, "ceremony"))
	if err != nil {
		t.Fatalf("re-opening store: %v", err)
	}
	var artifacts ceremony.Artifacts
	if ok, err := st.Get("artifacts", &artifacts); err != nil || !ok {
		t.Fatalf("artifacts missing after on-disk run")
	}
	if _, ok := ledger.Party(artifacts.GovernancePartyID); !ok {
		t.Errorf("governance party missing from persisted ledger")
	}
	if _, ok := ledger.Party(artifacts.ObserverPartyID); !ok {
		t.Errorf("observer party missing from persisted ledger")
	}
}
