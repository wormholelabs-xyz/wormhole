//go:build integration

// End-to-end integration test for the emitter-approval crank.
//
// It boots a live Canton sandbox via `dpm`, seeds a PENDING EmitterRequest
// (Test.TestCrank:integrationSetup, which deliberately does NOT approve it),
// runs the crank as the operator, and asserts the crank creates the Emitter and
// consumes the request. It then proves exactly-once behaviour independently for
// both guards (command deduplication and the consuming choice), and restart
// idempotency by draining a backlog with a fresh crank instance.
//
// Run:
//
//	go test -tags integration -run TestCantonCrankIntegration ./pkg/cantoncrank -v
//
// Requires `dpm` (PATH or ~/.dpm/bin) + a JDK; skipped otherwise. Slow (boots a
// JVM sandbox). The sandbox lifecycle mirrors watcher_integration_test.go.
package cantoncrank

import (
	"context"
	"crypto/rand"
	"encoding/hex"
	"encoding/json"
	"net"
	"os"
	"os/exec"
	"path/filepath"
	"sort"
	"strings"
	"syscall"
	"testing"
	"time"

	"github.com/stretchr/testify/assert"
	"github.com/stretchr/testify/require"
	"go.uber.org/zap"
	"google.golang.org/grpc"
	"google.golang.org/grpc/credentials/insecure"

	"github.com/certusone/wormhole/node/pkg/cantonclient"
)

func findDpm(t *testing.T) string {
	if p, err := exec.LookPath("dpm"); err == nil {
		return p
	}
	home, _ := os.UserHomeDir()
	p := filepath.Join(home, ".dpm", "bin", "dpm")
	if _, err := os.Stat(p); err == nil {
		return p
	}
	t.Skip("dpm not found (PATH or ~/.dpm/bin); skipping Canton integration test")
	return ""
}

type crankParties struct {
	Operator  string `json:"operator"`
	Requester string `json:"requester"`
}

// addRequestInput mirrors Test.TestCrank:AddRequestInput; Daml-Script decodes it
// from the --input-file JSON.
type addRequestInput struct {
	Operator  string `json:"operator"`
	Requester string `json:"requester"`
	Count     int    `json:"count"`
}

// crankLedger is the concrete gRPC client surface the end-to-end test drives: the
// crank's narrow Reader + Submitter plus a test-only ActiveEmitters read helper.
// The *grpcClient returned by NewCantonGrpcClient satisfies it.
type crankLedger interface {
	Reader
	Submitter
	ActiveEmitters(ctx context.Context, operator string) ([]cantonclient.EmitterInfo, error)
	Close() error
}

func TestCantonCrankIntegration(t *testing.T) {
	dpm := findDpm(t)

	cantonDir, err := filepath.Abs("../../../canton")
	require.NoError(t, err)
	dar := filepath.Join(cantonDir, "test", ".daml", "dist", "wormhole-core-test-0.1.0.dar")

	port := os.Getenv("CANTON_SANDBOX_PORT")
	if port == "" {
		port = "6865"
	}
	addr := "localhost:" + port

	runCmd(t, cantonDir, dpm, "build", "--all")

	ctx, cancel := context.WithCancel(context.Background())
	defer cancel()
	sandboxDir := t.TempDir()
	logPath := filepath.Join(sandboxDir, "sandbox.log")
	logFile, err := os.Create(logPath)
	require.NoError(t, err)
	defer logFile.Close()

	sandbox := exec.CommandContext(ctx, dpm, "sandbox", "--no-tty")
	sandbox.Dir = sandboxDir
	sandbox.Stdout = logFile
	sandbox.Stderr = logFile
	sandbox.SysProcAttr = &syscall.SysProcAttr{Setpgid: true}
	require.NoError(t, sandbox.Start())
	t.Cleanup(func() {
		if sandbox.Process != nil {
			_ = syscall.Kill(-sandbox.Process.Pid, syscall.SIGKILL)
		}
	})

	require.Eventually(t, func() bool {
		b, _ := os.ReadFile(logPath)
		return strings.Contains(string(b), "Canton sandbox is ready")
	}, 180*time.Second, 2*time.Second, "sandbox never became ready")
	conn, err := net.DialTimeout("tcp", addr, 5*time.Second)
	require.NoError(t, err)
	_ = conn.Close()

	// Seed a PENDING EmitterRequest (no approval) and capture the parties.
	partyFile := filepath.Join(sandboxDir, "parties.json")
	out, err := exec.Command(dpm, "script", "--dar", dar, "--upload-dar", "yes",
		"--script-name", "Test.TestCrank:integrationSetup",
		"--ledger-host", "localhost", "--ledger-port", port,
		"--output-file", partyFile).CombinedOutput()
	require.NoErrorf(t, err, "dpm script failed: %s", out)

	raw, err := os.ReadFile(partyFile)
	require.NoError(t, err)
	var parties crankParties
	require.NoError(t, json.Unmarshal(raw, &parties))
	require.NotEmpty(t, parties.Operator)
	require.NotEmpty(t, parties.Requester)

	// Construct the real gRPC client (insecure — the dev sandbox is unauthenticated).
	rawClient, err := cantonclient.NewCantonGrpcClient(addr, parties.Operator, zap.NewNop(),
		grpc.WithTransportCredentials(insecure.NewCredentials()))
	require.NoError(t, err)
	defer rawClient.Close()
	client, ok := rawClient.(crankLedger)
	require.True(t, ok, "gRPC client must satisfy the crank read+write surface")

	// --- Phase 1: pending pre-condition ----------------------------------------
	// Exactly one pending request, zero Emitters — nothing auto-approves.
	reqs, err := client.ActiveEmitterRequests(ctx, parties.Operator)
	require.NoError(t, err)
	require.Len(t, reqs, 1, "integrationSetup must leave exactly one pending EmitterRequest")
	firstReq := reqs[0]
	require.Equal(t, parties.Requester, firstReq.Requester)
	require.NotEmpty(t, firstReq.TemplateID.EntityName, "template id must be echoed from the ACS event")

	ems, err := client.ActiveEmitters(ctx, parties.Operator)
	require.NoError(t, err)
	require.Empty(t, ems, "no Emitter must exist before the crank runs")

	// --- Phase 2: crank approves the pending request ----------------------------
	c := New(client, client, parties.Operator, ApproveAll, time.Second, 0, zap.NewNop())
	crankCtx, stop := context.WithCancel(ctx)
	done := make(chan struct{})
	go func() { defer close(done); _ = c.Run(crankCtx) }()

	require.Eventually(t, func() bool {
		reqs, err := client.ActiveEmitterRequests(ctx, parties.Operator)
		if err != nil {
			return false
		}
		ems, err := client.ActiveEmitters(ctx, parties.Operator)
		if err != nil {
			return false
		}
		return len(reqs) == 0 && hasEmitter(ems, parties.Operator, parties.Requester, 0)
	}, 60*time.Second, time.Second, "crank did not approve the pending request into Emitter (operator, requester, 0)")

	// Stop the crank AND wait for its goroutine to fully exit before the manual
	// exactly-once probes so it cannot race them (deterministic isolation — the
	// probes assume no concurrent approver).
	stop()
	select {
	case <-done:
	case <-time.After(10 * time.Second):
		t.Fatal("phase 2 crank goroutine did not exit after stop()")
	}

	// --- Phase 3: exactly-once, guard 1 (command deduplication) -----------------
	// Isolate the DEDUP guard. The command id derived for firstReq
	// (commandIDFor(firstReq.ContractID)) was already accepted by the crank in
	// Phase 2, so its change id (act_as=operator, fixed user id, that command id)
	// is spent for the deduplication period. Resubmit that SAME command id — but
	// against a DIFFERENT, still-ACTIVE EmitterRequest — and the participant
	// rejects it with DUPLICATE_COMMAND -> ErrDuplicateCommand.
	//
	// Targeting an ACTIVE contract is what isolates this guard: the consuming
	// choice cannot explain the rejection (there is a live contract to approve),
	// so a DUPLICATE_COMMAND can only be the dedup layer. (Resubmitting the same
	// command id against the already-consumed firstReq does NOT work: Canton
	// resolves the archived target during interpretation and returns
	// CONTRACT_NOT_FOUND before the dedup check, masking the dedup guard — see
	// Phase 4, which relies on exactly that ordering. Confirmed on Canton 3.5.1.)
	//
	// The probe request is deliberately left PENDING (dedup blocked its approval);
	// Phase 5's fresh crank drains it with its own distinct command id.
	dupProbeFile := filepath.Join(sandboxDir, "dup-probe.json")
	dupProbeJSON, err := json.Marshal(addRequestInput{Operator: parties.Operator, Requester: parties.Requester, Count: 1})
	require.NoError(t, err)
	require.NoError(t, os.WriteFile(dupProbeFile, dupProbeJSON, 0o600))
	out, err = exec.Command(dpm, "script", "--dar", dar,
		"--script-name", "Test.TestCrank:addRequest",
		"--ledger-host", "localhost", "--ledger-port", port,
		"--input-file", dupProbeFile).CombinedOutput()
	require.NoErrorf(t, err, "dpm addRequest (dup probe) failed: %s", out)

	probeReqs, err := client.ActiveEmitterRequests(ctx, parties.Operator)
	require.NoError(t, err)
	require.Len(t, probeReqs, 1, "exactly one fresh pending request for the dedup probe")
	dupProbeReq := probeReqs[0]
	require.NotEqual(t, firstReq.ContractID, dupProbeReq.ContractID, "probe must target a different contract")

	sameCmdID := commandIDFor(firstReq.ContractID)
	err = client.SubmitApproveEmitter(ctx, parties.Operator, dupProbeReq, sameCmdID)
	require.ErrorIs(t, err, ErrDuplicateCommand, "dedup must reject a resubmission of the same (spent) change id, even against an active contract")

	// The probe request must remain active — dedup rejected its approval, so no
	// Emitter was minted for it.
	stillPending, err := client.ActiveEmitterRequests(ctx, parties.Operator)
	require.NoError(t, err)
	require.Len(t, stillPending, 1, "dedup-rejected probe request must stay pending")

	// --- Phase 4: exactly-once, guard 2 (consuming choice) ----------------------
	// Deliberately bypass dedup with a FRESH random command id. The firstReq
	// contract was consumed by ApproveEmitter in Phase 2, so interpretation fails
	// on the archived contract (CONTRACT_NOT_FOUND / CONTRACT_NOT_ACTIVE) ->
	// ErrContractInactive. Asserting this specific sentinel proves the
	// CONSUMING-CHOICE guard fired, independently of dedup: consumption alone
	// prevents a double-approve even with a brand-new change id.
	freshCmdID := "approve-manual-" + randomHex(t)
	require.NotEqual(t, sameCmdID, freshCmdID)
	err = client.SubmitApproveEmitter(ctx, parties.Operator, firstReq, freshCmdID)
	require.ErrorIs(t, err, ErrContractInactive, "the consuming choice must reject a second approve of the archived request")

	ems, err = client.ActiveEmitters(ctx, parties.Operator)
	require.NoError(t, err)
	require.Len(t, ems, 1, "exactly one Emitter after both duplicate-approve attempts")
	require.Equal(t, 1, countEmittersWithID(ems, 0), "exactly one Emitter with emitterId 0")

	// --- Phase 5: restart idempotency with a backlog ----------------------------
	// Seed N more pending requests, then drain the whole backlog — the N new ones
	// plus the Phase 3 dedup-probe request still pending — with a FRESH crank
	// instance (no in-memory carryover from the first). Exactly-once is keyed
	// purely on deterministic command ids + ledger state.
	const backlog = 3
	// Total Emitters expected once drained: emitter 0 (firstReq, Phase 2), the
	// Phase 3 probe request, and the `backlog` requests seeded here.
	const wantEmitters = backlog + 2
	inputFile := filepath.Join(sandboxDir, "add-request.json")
	inputJSON, err := json.Marshal(addRequestInput{Operator: parties.Operator, Requester: parties.Requester, Count: backlog})
	require.NoError(t, err)
	require.NoError(t, os.WriteFile(inputFile, inputJSON, 0o600))

	out, err = exec.Command(dpm, "script", "--dar", dar,
		"--script-name", "Test.TestCrank:addRequest",
		"--ledger-host", "localhost", "--ledger-port", port,
		"--input-file", inputFile).CombinedOutput()
	require.NoErrorf(t, err, "dpm addRequest script failed: %s", out)

	c2 := New(client, client, parties.Operator, ApproveAll, time.Second, 0, zap.NewNop())
	c2Ctx, stop2 := context.WithCancel(ctx)
	done2 := make(chan struct{})
	go func() { defer close(done2); _ = c2.Run(c2Ctx) }()
	// Stop and wait for the goroutine to exit at test end (same disciplined
	// lifecycle as Phase 2) so it cannot outlive the test.
	defer func() {
		stop2()
		select {
		case <-done2:
		case <-time.After(10 * time.Second):
			t.Error("phase 5 crank goroutine did not exit after stop()")
		}
	}()

	require.Eventually(t, func() bool {
		reqs, err := client.ActiveEmitterRequests(ctx, parties.Operator)
		if err != nil {
			return false
		}
		ems, err := client.ActiveEmitters(ctx, parties.Operator)
		if err != nil {
			return false
		}
		return len(reqs) == 0 && len(ems) == wantEmitters
	}, 90*time.Second, time.Second, "fresh crank did not drain the backlog exactly once")

	// Emitter ids must be exactly 0..N-1 with no duplicates (exactly-once).
	ems, err = client.ActiveEmitters(ctx, parties.Operator)
	require.NoError(t, err)
	require.Len(t, ems, wantEmitters)
	assert.Equal(t, expectedIDs(wantEmitters), emitterIDs(ems), "emitter ids must be exactly 0..N-1, each once")
}

// hasEmitter reports whether an Emitter with the given key components is present.
func hasEmitter(ems []cantonclient.EmitterInfo, operator, owner string, id uint64) bool {
	for _, e := range ems {
		if e.Operator == operator && e.Owner == owner && e.EmitterID == id {
			return true
		}
	}
	return false
}

func countEmittersWithID(ems []cantonclient.EmitterInfo, id uint64) int {
	n := 0
	for _, e := range ems {
		if e.EmitterID == id {
			n++
		}
	}
	return n
}

// emitterIDs returns the sorted, distinct-preserving list of emitter ids.
func emitterIDs(ems []cantonclient.EmitterInfo) []uint64 {
	ids := make([]uint64, 0, len(ems))
	for _, e := range ems {
		ids = append(ids, e.EmitterID)
	}
	sort.Slice(ids, func(i, j int) bool { return ids[i] < ids[j] })
	return ids
}

func expectedIDs(n int) []uint64 {
	ids := make([]uint64, n)
	for i := range ids {
		ids[i] = uint64(i)
	}
	return ids
}

func randomHex(t *testing.T) string {
	t.Helper()
	var b [8]byte
	_, err := rand.Read(b[:])
	require.NoError(t, err)
	return hex.EncodeToString(b[:])
}

// runCmd mirrors the watcher integration helper.
func runCmd(t *testing.T, dir, name string, args ...string) {
	t.Helper()
	cmd := exec.Command(name, args...)
	cmd.Dir = dir
	if out, err := cmd.CombinedOutput(); err != nil {
		t.Fatalf("%s %v failed: %v\n%s", name, args, err, out)
	}
}
