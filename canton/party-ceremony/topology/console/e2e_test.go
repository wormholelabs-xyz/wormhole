//go:build canton_e2e

// Real-Canton end-to-end test for the console Topology adapter.
//
// It drives the party-ceremony onboarding state machine against a live Canton
// sandbox via the console adapter — the actual production topology operations,
// not the fake — and asserts the guardian namespace and both guardian parties
// end up on the synchronizer.
//
// Requires a running sandbox on localhost:6865 (`dpm sandbox --no-tty`) and
// `dpm` + `java` on PATH. Skipped otherwise. Build/run with:
//
//	go test -tags canton_e2e -run TestConsoleOnboarding -timeout 20m ./topology/console
//
// It is slow (each topology op is a fresh console JVM) and opt-in via the
// build tag, so it never runs in the default unit suite.
package console_test

import (
	"context"
	"fmt"
	"net"
	"path/filepath"
	"runtime"
	"testing"
	"time"

	"github.com/wormhole-foundation/wormhole/canton/party-ceremony/ceremony"
	"github.com/wormhole-foundation/wormhole/canton/party-ceremony/ceremony/fake"
	"github.com/wormhole-foundation/wormhole/canton/party-ceremony/sign"
	"github.com/wormhole-foundation/wormhole/canton/party-ceremony/topology/console"
)

func opsScriptPath(t *testing.T) string {
	t.Helper()
	_, thisFile, _, ok := runtime.Caller(0)
	if !ok {
		t.Fatal("cannot locate test file")
	}
	return filepath.Join(filepath.Dir(thisFile), "ops.canton")
}

func requireSandbox(t *testing.T) {
	t.Helper()
	conn, err := net.DialTimeout("tcp", "localhost:6865", 2*time.Second)
	if err != nil {
		t.Skipf("no Canton sandbox on localhost:6865 (%v); start `dpm sandbox --no-tty`", err)
	}
	conn.Close()
}

// TestConsoleOnboarding runs the full onboarding (2 guardians, threshold 2)
// through the console adapter against the live sandbox — the single sandbox
// participant hosts both parties — and verifies the parties on-ledger.
func TestConsoleOnboarding(t *testing.T) {
	requireSandbox(t)
	topo := console.New(opsScriptPath(t))
	dir := t.TempDir()

	owners := make([]ceremony.Owner, 0, 2)
	signers := map[string]ceremony.Signer{}
	for i := 1; i <= 2; i++ {
		id := fmt.Sprintf("guardian-%d", i)
		prefix := filepath.Join(dir, id)
		pub, err := sign.GenerateKeyFiles(prefix)
		if err != nil {
			t.Fatalf("generating key for %s: %v", id, err)
		}
		s, err := sign.NewKeySigner(prefix + ".key")
		if err != nil {
			t.Fatalf("loading signer for %s: %v", id, err)
		}
		signers[id] = s
		owners = append(owners, ceremony.Owner{ID: id, PublicKeyDER: pub})
	}

	spec, err := ceremony.NewOnboardingSpec("wf-canton-e2e", "guardian-1", 2, owners)
	if err != nil {
		t.Fatalf("building spec: %v", err)
	}
	store := fake.NewStore()
	ctx := context.Background()

	// Each guardian takes turns advancing (all against the one sandbox
	// participant, so they share the topology adapter). A fresh Onboarding per
	// turn exercises resume-from-store.
	complete := false
	for round := 0; round < 12 && !complete; round++ {
		for _, o := range owners {
			flow, err := ceremony.NewOnboarding(spec, o.ID, topo, signers[o.ID], store)
			if err != nil {
				t.Fatalf("constructing onboarding for %s: %v", o.ID, err)
			}
			status, err := flow.Advance(ctx)
			if err != nil {
				t.Fatalf("advance as %s: %v", o.ID, err)
			}
			if status.Complete {
				complete = true
				break
			}
		}
	}
	if !complete {
		t.Fatal("ceremony did not complete against the sandbox within the round budget")
	}

	var art ceremony.Artifacts
	ok, err := store.Get("artifacts", &art)
	if err != nil || !ok {
		t.Fatalf("artifacts missing: ok=%v err=%v", ok, err)
	}
	if art.Threshold != 2 || art.Namespace == "" {
		t.Fatalf("artifacts = %+v, want threshold 2 and a namespace", art)
	}
	t.Logf("namespace=%s governance=%s observer=%s", art.Namespace, art.GovernancePartyID, art.ObserverPartyID)

	// The real proof: both parties exist on the synchronizer.
	for name, pid := range map[string]string{"governance": art.GovernancePartyID, "observer": art.ObserverPartyID} {
		present, err := topo.PartyPresent(ctx, pid)
		if err != nil {
			t.Fatalf("checking %s party %s: %v", name, pid, err)
		}
		if !present {
			t.Fatalf("%s party %s not present on the synchronizer", name, pid)
		}
	}
}
