package fake

import (
	"context"
	"crypto/ed25519"
	"crypto/rand"
	"crypto/x509"
	"encoding/hex"
	"strings"
	"testing"

	"github.com/wormhole-foundation/wormhole/canton/party-ceremony/ceremony"
)

type owner struct {
	pubDER []byte
	priv   ed25519.PrivateKey
}

func newOwner(t *testing.T) owner {
	t.Helper()
	pub, priv, err := ed25519.GenerateKey(rand.Reader)
	if err != nil {
		t.Fatalf("generating key: %v", err)
	}
	pubDER, err := x509.MarshalPKIXPublicKey(pub)
	if err != nil {
		t.Fatalf("encoding key: %v", err)
	}
	return owner{pubDER: pubDER, priv: priv}
}

func (o owner) sign(t *testing.T, hashHex string) ceremony.OwnerSignature {
	t.Helper()
	hash, err := hex.DecodeString(hashHex)
	if err != nil {
		t.Fatalf("decoding hash: %v", err)
	}
	return ceremony.OwnerSignature{OwnerID: "x", SigHex: hex.EncodeToString(ed25519.Sign(o.priv, hash))}
}

// delegateAll roots every owner's namespace on the ledger.
func delegateAll(t *testing.T, p *Participant, owners []owner) {
	t.Helper()
	ctx := context.Background()
	for _, o := range owners {
		tx, err := p.PrepareRootDelegation(ctx, o.pubDER)
		if err != nil {
			t.Fatalf("preparing delegation: %v", err)
		}
		if err := p.Submit(ctx, tx, []ceremony.OwnerSignature{o.sign(t, tx.HashHex)}, nil); err != nil {
			t.Fatalf("submitting delegation: %v", err)
		}
	}
}

func TestDelegationRejectsForeignSignature(t *testing.T) {
	ledger := NewLedger()
	p := NewParticipant(ledger, "p1")
	honest, imposter := newOwner(t), newOwner(t)
	ctx := context.Background()

	tx, err := p.PrepareRootDelegation(ctx, honest.pubDER)
	if err != nil {
		t.Fatalf("prepare: %v", err)
	}
	err = p.Submit(ctx, tx, []ceremony.OwnerSignature{imposter.sign(t, tx.HashHex)}, nil)
	if err == nil || !strings.Contains(err.Error(), "not signed by its own key") {
		t.Fatalf("error = %v, want self-signature rejection", err)
	}
}

func TestNamespaceCreationRequiresAllOwners(t *testing.T) {
	ledger := NewLedger()
	p := NewParticipant(ledger, "p1")
	owners := []owner{newOwner(t), newOwner(t), newOwner(t)}
	delegateAll(t, p, owners)
	ctx := context.Background()

	pubs := [][]byte{owners[0].pubDER, owners[1].pubDER, owners[2].pubDER}
	tx, err := p.PrepareDecentralizedNamespace(ctx, pubs, 2)
	if err != nil {
		t.Fatalf("prepare: %v", err)
	}

	partial := []ceremony.OwnerSignature{owners[0].sign(t, tx.HashHex), owners[1].sign(t, tx.HashHex)}
	if err := p.Submit(ctx, tx, partial, nil); err == nil {
		t.Fatalf("namespace creation accepted with 2 of 3 signatures")
	}

	full := append(partial, owners[2].sign(t, tx.HashHex))
	if err := p.Submit(ctx, tx, full, nil); err != nil {
		t.Fatalf("namespace creation with all signatures: %v", err)
	}
	if _, ok := ledger.DNS(tx.Namespace); !ok {
		t.Fatalf("namespace not recorded")
	}
}

func TestHostingEnforcesThresholdAndConsent(t *testing.T) {
	ledger := NewLedger()
	p1 := NewParticipant(ledger, "p1")
	p2 := NewParticipant(ledger, "p2")
	owners := []owner{newOwner(t), newOwner(t), newOwner(t)}
	delegateAll(t, p1, owners)
	ctx := context.Background()

	pubs := [][]byte{owners[0].pubDER, owners[1].pubDER, owners[2].pubDER}
	dnsTx, err := p1.PrepareDecentralizedNamespace(ctx, pubs, 2)
	if err != nil {
		t.Fatalf("prepare dns: %v", err)
	}
	all := []ceremony.OwnerSignature{
		owners[0].sign(t, dnsTx.HashHex), owners[1].sign(t, dnsTx.HashHex), owners[2].sign(t, dnsTx.HashHex),
	}
	if err := p1.Submit(ctx, dnsTx, all, nil); err != nil {
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
	consent1, err := p1.Consent(ctx, hostTx)
	if err != nil {
		t.Fatalf("consent p1: %v", err)
	}
	consent2, err := p2.Consent(ctx, hostTx)
	if err != nil {
		t.Fatalf("consent p2: %v", err)
	}
	sigs2of3 := []ceremony.OwnerSignature{owners[0].sign(t, hostTx.HashHex), owners[1].sign(t, hostTx.HashHex)}

	// One valid signature is below the 2-of-3 threshold.
	if err := p1.Submit(ctx, hostTx, sigs2of3[:1], []string{consent1, consent2}); err == nil {
		t.Fatalf("hosting accepted below signature threshold")
	}
	// Threshold met but a hosting participant's consent is missing.
	if err := p1.Submit(ctx, hostTx, sigs2of3, []string{consent1}); err == nil {
		t.Fatalf("hosting accepted without every host's consent")
	}
	// A consent from an unregistered participant is rejected.
	forged := "consent|ghost|" + hostTx.HashHex
	if err := p1.Submit(ctx, hostTx, sigs2of3, []string{consent1, forged}); err == nil {
		t.Fatalf("hosting accepted a consent from an unknown participant")
	}
	// Threshold signatures + all consents commits, and re-submit is a no-op.
	if err := p1.Submit(ctx, hostTx, sigs2of3, []string{consent1, consent2}); err != nil {
		t.Fatalf("valid hosting rejected: %v", err)
	}
	if err := p1.Submit(ctx, hostTx, sigs2of3, []string{consent1, consent2}); err != nil {
		t.Fatalf("idempotent re-submit failed: %v", err)
	}
	party, ok := ledger.Party(hostTx.PartyID)
	if !ok {
		t.Fatalf("party not recorded")
	}
	if party.Hosts["p2"] != ceremony.Observation {
		t.Fatalf("p2 permission = %s, want Observation", party.Hosts["p2"])
	}
}

func TestHostingRequiresKnownNamespace(t *testing.T) {
	ledger := NewLedger()
	p := NewParticipant(ledger, "p1")
	ctx := context.Background()
	tx, err := p.PreparePartyHosting(ctx, ceremony.HostingSpec{
		PartyName:             "guardianObserver",
		Namespace:             "deadbeef",
		Hosts:                 []ceremony.Host{{ParticipantUID: "p1", Permission: ceremony.Observation}},
		ConfirmationThreshold: 1,
	})
	if err != nil {
		t.Fatalf("prepare: %v", err)
	}
	consent, err := p.Consent(ctx, tx)
	if err != nil {
		t.Fatalf("consent: %v", err)
	}
	if err := p.Submit(ctx, tx, nil, []string{consent}); err == nil {
		t.Fatalf("hosting accepted under an unknown namespace")
	}
}
