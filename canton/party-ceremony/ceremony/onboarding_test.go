package ceremony_test

import (
	"strings"
	"testing"

	"github.com/wormhole-foundation/wormhole/canton/party-ceremony/ceremony"
	"github.com/wormhole-foundation/wormhole/canton/party-ceremony/ceremony/fake"
)

func TestNewOnboardingRejectsBadWiring(t *testing.T) {
	ledger := fake.NewLedger()
	guardians := newGuardians(t, 3, func(id string) ceremony.Topology {
		return fake.NewParticipant(ledger, "participant::"+id)
	})
	spec := specOf(t, guardians, 2)
	st := fake.NewStore()

	cases := []struct {
		name  string
		build func() (*ceremony.Onboarding, error)
		want  string
	}{
		{"unknown actor", func() (*ceremony.Onboarding, error) {
			return ceremony.NewOnboarding(spec, "stranger", guardians[0].topo, guardians[0].signer, st)
		}, "not an owner"},
		{"nil topology", func() (*ceremony.Onboarding, error) {
			return ceremony.NewOnboarding(spec, guardians[0].id, nil, guardians[0].signer, st)
		}, "required"},
		{"nil signer", func() (*ceremony.Onboarding, error) {
			return ceremony.NewOnboarding(spec, guardians[0].id, guardians[0].topo, nil, st)
		}, "required"},
	}
	for _, tc := range cases {
		t.Run(tc.name, func(t *testing.T) {
			if _, err := tc.build(); err == nil || !strings.Contains(err.Error(), tc.want) {
				t.Fatalf("error = %v, want mention of %q", err, tc.want)
			}
		})
	}
}

func TestSpecValidation(t *testing.T) {
	ledger := fake.NewLedger()
	guardians := newGuardians(t, 3, func(id string) ceremony.Topology {
		return fake.NewParticipant(ledger, "participant::"+id)
	})
	owners := []ceremony.Owner{
		{ID: guardians[0].id, PublicKeyDER: guardians[0].pubDER},
		{ID: guardians[1].id, PublicKeyDER: guardians[1].pubDER},
		{ID: guardians[2].id, PublicKeyDER: guardians[2].pubDER},
	}

	cases := []struct {
		name        string
		id          string
		coordinator string
		threshold   int
		owners      []ceremony.Owner
		want        string
	}{
		{"empty id", "", guardians[0].id, 2, owners, "workflow id"},
		{"threshold too high", "wf", guardians[0].id, 4, owners, "out of range"},
		{"threshold zero", "wf", guardians[0].id, 0, owners, "out of range"},
		{"coordinator not owner", "wf", "nobody", 2, owners, "not an owner"},
		{"duplicate owner", "wf", guardians[0].id, 2,
			append(append([]ceremony.Owner{}, owners...), owners[0]), "duplicate"},
		{"shared key", "wf", guardians[0].id, 2,
			[]ceremony.Owner{owners[0], {ID: "other", PublicKeyDER: owners[0].PublicKeyDER}}, "share a public key"},
	}
	for _, tc := range cases {
		t.Run(tc.name, func(t *testing.T) {
			_, err := ceremony.NewOnboardingSpec(tc.id, tc.coordinator, tc.threshold, tc.owners)
			if err == nil || !strings.Contains(err.Error(), tc.want) {
				t.Fatalf("error = %v, want mention of %q", err, tc.want)
			}
		})
	}
}

// TestRefusesToSignTamperedTransaction proves an owner will not sign a
// prepared transaction that does not match the spec: if the shared store's
// dns/prepare entry is swapped for a namespace over a different (attacker)
// key set, every owner's dns/sign step must refuse rather than blind-sign the
// planted hash. This is the defense against a tampered Git-distributed store.
func TestRefusesToSignTamperedTransaction(t *testing.T) {
	ledger := fake.NewLedger()
	guardians := newGuardians(t, 3, func(id string) ceremony.Topology {
		return fake.NewParticipant(ledger, "participant::"+id)
	})
	spec := specOf(t, guardians, 2)
	st := fake.NewStore()

	// Get through identify + delegate for everyone so dns/prepare is reachable.
	for round := 0; round < 2; round++ {
		for _, g := range guardians {
			advance(t, spec, g, st)
		}
	}

	// The coordinator prepared the real DNS. Overwrite the store entry with a
	// namespace over a rogue key set (as a tampered PR would), keeping the
	// key present so Get succeeds.
	rogue := newGuardians(t, 1, func(id string) ceremony.Topology {
		return fake.NewParticipant(ledger, "participant::rogue")
	})[0]
	rogueTx, err := rogue.topo.PrepareDecentralizedNamespace(nil, [][]byte{rogue.pubDER}, 1)
	if err != nil {
		t.Fatalf("preparing rogue tx: %v", err)
	}
	// Replace the recorded dns/prepare with the rogue transaction.
	tampered := fake.NewStore()
	copyStore(t, st, tampered, "dns/prepare")
	if err := tampered.Put("dns/prepare", rogueTx); err != nil {
		t.Fatalf("planting rogue tx: %v", err)
	}

	// A guardian's dns/sign step must now refuse.
	flow, err := ceremony.NewOnboarding(spec, guardians[0].id, guardians[0].topo, guardians[0].signer, tampered)
	if err != nil {
		t.Fatalf("constructing onboarding: %v", err)
	}
	_, err = flow.Advance(nil)
	if err == nil || !strings.Contains(err.Error(), "refusing to sign") {
		t.Fatalf("advance error = %v, want a refusal to sign the tampered namespace", err)
	}
}

// copyStore copies every entry of src into dst except skipKey.
func copyStore(t *testing.T, src, dst *fake.Store, skipKey string) {
	t.Helper()
	owners := []string{"guardian-1", "guardian-2", "guardian-3"}
	keys := []string{"dns/prepare"}
	for _, o := range owners {
		keys = append(keys, "identify/"+o, "delegate/"+o)
	}
	for _, key := range keys {
		if key == skipKey {
			continue
		}
		var raw any
		ok, err := src.Get(key, &raw)
		if err != nil {
			t.Fatalf("reading %s: %v", key, err)
		}
		if !ok {
			continue
		}
		if err := dst.Put(key, raw); err != nil {
			t.Fatalf("copying %s: %v", key, err)
		}
	}
}
