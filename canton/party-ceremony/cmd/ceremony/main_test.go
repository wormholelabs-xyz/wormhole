package main_test

import (
	"encoding/json"
	"os"
	"os/exec"
	"path/filepath"
	"strings"
	"testing"
)

// TestCeremonyCLIMultiProcess is the CLI-level e2e: it compiles the real
// binary and runs the whole onboarding as separate OS processes — keygen per
// guardian, one init, then repeated per-guardian resumes — asserting the
// documented exit-code contract (2 = come back later, 0 = complete) and the
// final artifacts. Five guardians, threshold 3, guardian-3 signing through
// the external custody command path (the binary's own `sign` subcommand) so
// both signer implementations are exercised end to end.
func TestCeremonyCLIMultiProcess(t *testing.T) {
	if testing.Short() {
		t.Skip("compiles and runs the real binary")
	}
	dir := t.TempDir()
	bin := filepath.Join(dir, "ceremony-bin")
	build := exec.Command("go", "build", "-o", bin, ".")
	build.Env = os.Environ()
	if out, err := build.CombinedOutput(); err != nil {
		t.Fatalf("building CLI: %v\n%s", err, out)
	}

	run := func(wantExit int, args ...string) string {
		t.Helper()
		cmd := exec.Command(bin, args...)
		cmd.Dir = dir
		out, err := cmd.CombinedOutput()
		exit := 0
		if err != nil {
			ee, ok := err.(*exec.ExitError)
			if !ok {
				t.Fatalf("running %v: %v\n%s", args, err, out)
			}
			exit = ee.ExitCode()
		}
		if exit != wantExit {
			t.Fatalf("%v exited %d, want %d\n%s", args, exit, wantExit, out)
		}
		return string(out)
	}

	guardians := []string{"guardian-1", "guardian-2", "guardian-3", "guardian-4", "guardian-5"}
	initArgs := []string{"init", "--dir", "ceremony", "--id", "wf-cli", "--threshold", "3", "--coordinator", "guardian-1"}
	for _, g := range guardians {
		run(0, "keygen", "--out", g)
		initArgs = append(initArgs, "--owner", g+"="+g+".pub")
	}
	run(0, initArgs...)

	resume := func(g string) []string {
		args := []string{"resume", "--dir", "ceremony", "--actor", g, "--backend", "fake", "--ledger", "ledger.json"}
		if g == "guardian-3" {
			// External custody path: the sign command is the binary itself.
			return append(args, "--sign-cmd", bin+" sign --key "+filepath.Join(dir, g+".key"))
		}
		return append(args, "--key", g+".key")
	}

	// Guardians take turns in rounds, each turn a separate OS process. The
	// exit-code contract: every invocation before completion exits 2 (come
	// back later); the completing one exits 0. No fixed schedule is assumed —
	// only that completion happens within a bounded number of rounds.
	completed := false
	var finalOut string
	for round := 0; round < 8 && !completed; round++ {
		for _, g := range guardians {
			cmd := exec.Command(bin, resume(g)...)
			cmd.Dir = dir
			out, err := cmd.CombinedOutput()
			switch {
			case err == nil:
				completed = true
				finalOut = string(out)
			default:
				ee, ok := err.(*exec.ExitError)
				if !ok || ee.ExitCode() != 2 {
					t.Fatalf("resume as %s: %v (want exit 2 before completion)\n%s", g, err, out)
				}
			}
			if completed {
				break
			}
		}
	}
	if !completed {
		t.Fatalf("ceremony did not complete within the round budget")
	}
	if !strings.Contains(finalOut, "ceremony complete") {
		t.Fatalf("completing resume output missing completion notice:\n%s", finalOut)
	}
	// Completed ceremonies stay complete on re-run (idempotent).
	run(0, resume("guardian-2")...)

	status := run(0, "status", "--dir", "ceremony")
	if !strings.Contains(status, "artifacts") {
		t.Fatalf("status missing artifacts:\n%s", status)
	}

	var reports map[string]json.RawMessage
	raw, err := os.ReadFile(filepath.Join(dir, "ceremony", "reports.json"))
	if err != nil {
		t.Fatalf("reading reports: %v", err)
	}
	if err := json.Unmarshal(raw, &reports); err != nil {
		t.Fatalf("parsing reports: %v", err)
	}
	var artifacts struct {
		Namespace         string `json:"namespace"`
		GovernancePartyID string `json:"governancePartyId"`
		ObserverPartyID   string `json:"observerPartyId"`
		Threshold         int    `json:"threshold"`
	}
	if err := json.Unmarshal(reports["artifacts"], &artifacts); err != nil {
		t.Fatalf("parsing artifacts: %v", err)
	}
	if artifacts.Threshold != 3 || artifacts.Namespace == "" {
		t.Fatalf("artifacts = %+v, want threshold 3 and a namespace", artifacts)
	}
	if !strings.HasPrefix(artifacts.GovernancePartyID, "guardianGovernance::") ||
		!strings.HasPrefix(artifacts.ObserverPartyID, "guardianObserver::") {
		t.Fatalf("party ids = %q / %q, want production party names", artifacts.GovernancePartyID, artifacts.ObserverPartyID)
	}
}
