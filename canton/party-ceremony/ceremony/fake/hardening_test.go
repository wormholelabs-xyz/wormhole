package fake

import (
	"context"
	"testing"

	"github.com/wormhole-foundation/wormhole/canton/party-ceremony/ceremony"
)

// These tests target Submit's treatment of its decoded input as untrusted:
// each crafts a transaction a malicious or faulty caller could submit and
// asserts the fake rejects it, so the fake stays a faithful authorization
// oracle rather than a rubber stamp.

func rootedOwners(t *testing.T, p *Participant, n int) []owner {
	t.Helper()
	owners := make([]owner, n)
	for i := range owners {
		owners[i] = newOwner(t)
	}
	delegateAll(t, p, owners)
	return owners
}

func pubs(owners []owner) [][]byte {
	out := make([][]byte, len(owners))
	for i, o := range owners {
		out[i] = o.pubDER
	}
	return out
}

// B: the namespace id must be the hash of the owner keys, not a chosen value.
func TestSubmitRejectsUnboundNamespace(t *testing.T) {
	ledger := NewLedger()
	p := NewParticipant(ledger, "p1")
	owners := rootedOwners(t, p, 2)
	ctx := context.Background()

	tx, err := p.PrepareDecentralizedNamespace(ctx, pubs(owners), 2)
	if err != nil {
		t.Fatalf("prepare: %v", err)
	}
	// Re-encode with a chosen namespace that is not the hash of the keys.
	forged := reencode(t, tx, func(pl *txPayload) { pl.Namespace = "chosen-victim-namespace" })
	sigs := []ceremony.OwnerSignature{owners[0].sign(t, forged.HashHex), owners[1].sign(t, forged.HashHex)}
	if err := p.Submit(ctx, forged, sigs, nil); err == nil {
		t.Fatalf("accepted a namespace id not derived from its owner keys")
	}
}

// C (subsumed by B): a namespace cannot be redefined with a different owner
// set — since the id is bound to the keys, a different set yields a different
// id, and the original id cannot be reused for other owners.
func TestSubmitRejectsNamespaceRedefinition(t *testing.T) {
	ledger := NewLedger()
	p := NewParticipant(ledger, "p1")
	owners := rootedOwners(t, p, 3)
	ctx := context.Background()

	tx, err := p.PrepareDecentralizedNamespace(ctx, pubs(owners), 2)
	if err != nil {
		t.Fatalf("prepare: %v", err)
	}
	all := []ceremony.OwnerSignature{
		owners[0].sign(t, tx.HashHex), owners[1].sign(t, tx.HashHex), owners[2].sign(t, tx.HashHex),
	}
	if err := p.Submit(ctx, tx, all, nil); err != nil {
		t.Fatalf("first namespace submit: %v", err)
	}
	// Craft a new definition keeping the same id but a rogue single-owner set.
	rogue := rootedOwners(t, p, 1)
	forged := reencode(t, tx, func(pl *txPayload) {
		pl.OwnerPubs = pubs(rogue)
		pl.Threshold = 1
		// keep pl.Namespace = the original id
	})
	if err := p.Submit(ctx, forged, []ceremony.OwnerSignature{rogue[0].sign(t, forged.HashHex)}, nil); err == nil {
		t.Fatalf("accepted a redefinition of an existing namespace")
	}
}

// D: an empty delegation must not panic.
func TestSubmitRejectsEmptyDelegation(t *testing.T) {
	ledger := NewLedger()
	p := NewParticipant(ledger, "p1")
	ctx := context.Background()
	tx, err := p.PrepareRootDelegation(ctx, newOwner(t).pubDER)
	if err != nil {
		t.Fatalf("prepare: %v", err)
	}
	forged := reencode(t, tx, func(pl *txPayload) { pl.OwnerPubs = nil })
	if err := p.Submit(ctx, forged, nil, nil); err == nil {
		t.Fatalf("accepted a delegation with no key (should error, not panic)")
	}
}

// E: an empty owner set / zero threshold must not create a usable namespace.
func TestSubmitRejectsEmptyNamespace(t *testing.T) {
	ledger := NewLedger()
	p := NewParticipant(ledger, "p1")
	ctx := context.Background()
	tx, err := p.PrepareDecentralizedNamespace(ctx, pubs(rootedOwners(t, p, 1)), 1)
	if err != nil {
		t.Fatalf("prepare: %v", err)
	}
	forged := reencode(t, tx, func(pl *txPayload) { pl.OwnerPubs = nil; pl.Threshold = 0 })
	if err := p.Submit(ctx, forged, nil, nil); err == nil {
		t.Fatalf("accepted an empty, zero-threshold namespace")
	}
}

// F: duplicate owner keys must not inflate the signature count past the real
// distinct-key threshold.
func TestSubmitRejectsDuplicateOwnerKeys(t *testing.T) {
	ledger := NewLedger()
	p := NewParticipant(ledger, "p1")
	owners := rootedOwners(t, p, 1)
	ctx := context.Background()

	tx, err := p.PrepareDecentralizedNamespace(ctx, pubs(owners), 1)
	if err != nil {
		t.Fatalf("prepare: %v", err)
	}
	// Duplicate the single key to claim a "2-of-2" controlled by one key.
	forged := reencode(t, tx, func(pl *txPayload) {
		pl.OwnerPubs = [][]byte{owners[0].pubDER, owners[0].pubDER}
		pl.Threshold = 2
		pl.Namespace = namespaceOf(pl.OwnerPubs)
	})
	if err := p.Submit(ctx, forged, []ceremony.OwnerSignature{owners[0].sign(t, forged.HashHex)}, nil); err == nil {
		t.Fatalf("accepted a namespace with duplicate owner keys")
	}
}

// G: a consent is valid only if the named participant itself issued it; a
// fabricated blob cannot host a non-consenting participant.
func TestSubmitRejectsForgedConsent(t *testing.T) {
	ledger := NewLedger()
	p1 := NewParticipant(ledger, "p1")
	NewParticipant(ledger, "p2") // registered but never consents
	owners := rootedOwners(t, p1, 2)
	ctx := context.Background()

	dnsTx, err := p1.PrepareDecentralizedNamespace(ctx, pubs(owners), 2)
	if err != nil {
		t.Fatalf("prepare dns: %v", err)
	}
	if err := p1.Submit(ctx, dnsTx, []ceremony.OwnerSignature{owners[0].sign(t, dnsTx.HashHex), owners[1].sign(t, dnsTx.HashHex)}, nil); err != nil {
		t.Fatalf("submit dns: %v", err)
	}
	hostTx, err := p1.PreparePartyHosting(ctx, ceremony.HostingSpec{
		PartyName: "guardianObserver",
		Namespace: dnsTx.Namespace,
		Hosts: []ceremony.Host{
			{ParticipantUID: "p1", Permission: ceremony.Observation},
			{ParticipantUID: "p2", Permission: ceremony.Observation},
		},
		ConfirmationThreshold: 1,
	})
	if err != nil {
		t.Fatalf("prepare hosting: %v", err)
	}
	realP1, err := p1.Consent(ctx, hostTx)
	if err != nil {
		t.Fatalf("consent p1: %v", err)
	}
	sigs := []ceremony.OwnerSignature{owners[0].sign(t, hostTx.HashHex), owners[1].sign(t, hostTx.HashHex)}
	// p2 never called Consent; fabricate its consent blob.
	forgedP2 := "consent|p2|" + hostTx.HashHex
	if err := p1.Submit(ctx, hostTx, sigs, []string{realP1, forgedP2}); err == nil {
		t.Fatalf("accepted a fabricated consent for a non-consenting participant")
	}
}

// reencode rebuilds a PreparedTx after mutating its decoded payload, keeping
// the hash consistent with the new body (so the tamper is in the CONTENT, not
// a body/hash mismatch — that is covered separately by decodeTx).
func reencode(t *testing.T, tx ceremony.PreparedTx, mutate func(*txPayload)) ceremony.PreparedTx {
	t.Helper()
	pl, err := decodeTx(tx)
	if err != nil {
		t.Fatalf("decoding tx: %v", err)
	}
	mutate(&pl)
	out, err := encodeTx(pl)
	if err != nil {
		t.Fatalf("re-encoding tx: %v", err)
	}
	return out
}

// TestDescribeRejectsHashMismatch: a (body, hash) pair that does not match is
// rejected at Describe, before any owner signs.
func TestDescribeRejectsHashMismatch(t *testing.T) {
	ledger := NewLedger()
	p := NewParticipant(ledger, "p1")
	ctx := context.Background()
	tx, err := p.PrepareRootDelegation(ctx, newOwner(t).pubDER)
	if err != nil {
		t.Fatalf("prepare: %v", err)
	}
	tx.HashHex = "00" + tx.HashHex[2:] // corrupt the hash
	if _, err := p.Describe(ctx, tx); err == nil {
		t.Fatalf("Describe accepted a hash that does not match the body")
	}
}
