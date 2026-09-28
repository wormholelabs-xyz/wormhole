//go:build surfpool

// Surfpool process control, cheat codes and account seeding for the guardian end-to-end
// test. Mirrors svm/accountant/programs/global-accountant/tests/surfpool/harness.rs.

package accountant

import (
	"bytes"
	"encoding/binary"
	"encoding/hex"
	"encoding/json"
	"fmt"
	"io"
	"net"
	"net/http"
	"os"
	"os/exec"
	"path/filepath"
	"syscall"
	"testing"
	"time"

	"github.com/gagliardetto/solana-go"
	"github.com/stretchr/testify/require"
	"github.com/wormhole-foundation/wormhole/sdk/vaa"
)

const (
	// Matches SURFPOOL_BOOT_TIMEOUT in harness.rs.
	surfpoolBootTimeout   = 45 * time.Second
	surfpoolReadyPoll     = 250 * time.Millisecond
	surfpoolMaxReadyPolls = int(surfpoolBootTimeout / surfpoolReadyPoll)
	surfpoolHealthTimeout = 2 * time.Second

	// Covers one hex-encoded program upload.
	surfpoolRPCTimeout = 60 * time.Second

	surfpoolStopTimeout = 10 * time.Second

	// A 100 ms slot confirms a transaction in well under a second.
	surfpoolSlotTimeMillis = "100"

	// TEST_GLOBAL_ACCOUNTANT_PROGRAM_ID and TEST_NOREPLAY_PROGRAM_ID in svm/accountant/justfile.
	surfpoolAccountantProgramID = "US517G5965aydkZ46HS38QLi7UQiSojurfbQfKCELFx"
	surfpoolNoreplayProgramID   = "repMHgR5BEpGLeZvM5iGoNNDPw4eu2BS6sXJzaC8K4t"

	// Relative to node/pkg/accountant.
	surfpoolAccountantSOPath = "../../../svm/accountant/target/deploy/global_accountant.so"
	surfpoolNoreplaySOPath   = "../../../svm/accountant/crates/test-fixtures/data/solana_noreplay.so"

	// ChainRegistrationLayout::LEN and AccountTag::ChainRegistration, state.rs.
	chainRegistrationLen = 64
	chainRegistrationTag = 3

	// Core Bridge GuardianSet: index and key count, 20-byte keys, creation and expiration time.
	guardianSetHeaderLen = 8
	guardianSetKeyLen    = 20
	guardianSetFooterLen = 8
)

// surfpoolWatchdogScript runs surfpool as a child of sh and kills it once sh reads EOF on
// stdin. The test holds the write end, so surfpool stops when the test process exits by
// any path, including a timeout panic or a signal that skips t.Cleanup.
const surfpoolWatchdogScript = `"$@" </dev/null & child=$!; read -r _; kill "$child" 2>/dev/null; wait "$child"`

// surfpoolHarness is one running surfpool instance and its JSON-RPC client.
type surfpoolHarness struct {
	t            *testing.T
	rpcURL       string
	wsURL        string
	client       *http.Client
	healthClient *http.Client
	logDir       string
	exited       <-chan struct{}
}

// surfpoolBinary locates surfpool on PATH, then at ~/.local/bin/surfpool, the path the
// official install script uses. The test skips when neither exists.
func surfpoolBinary(t *testing.T) string {
	t.Helper()
	if found, err := exec.LookPath("surfpool"); err == nil {
		return found
	}
	candidate := filepath.Join(os.Getenv("HOME"), ".local/bin/surfpool")
	if info, err := os.Stat(candidate); err == nil && info.Mode().IsRegular() {
		return candidate
	}
	t.Skip("surfpool is not on PATH and not at ~/.local/bin/surfpool; install: curl -sL https://run.surfpool.run/ | bash")
	return ""
}

// readBuiltProgram returns the bytes of a program built from this tree. The test skips
// until buildCommand has produced it.
func readBuiltProgram(t *testing.T, path string, buildCommand string) []byte {
	t.Helper()
	elf, err := os.ReadFile(path)
	if err != nil {
		t.Skipf("%s is missing; run %s first: %v", path, buildCommand, err)
	}
	require.NotEmpty(t, elf, path)
	return elf
}

// readCommittedProgram returns the bytes of a program image checked into the repository.
func readCommittedProgram(t *testing.T, path string) []byte {
	t.Helper()
	elf, err := os.ReadFile(path)
	require.NoError(t, err, "committed program image")
	require.NotEmpty(t, elf, path)
	return elf
}

// freeLoopbackPorts reserves n distinct loopback ports. All listeners stay open until
// every port is chosen, so no two are equal; each port is free again before surfpool
// binds it.
func freeLoopbackPorts(t *testing.T, n int) []int {
	t.Helper()
	require.Positive(t, n)

	listeners := make([]net.Listener, 0, n)
	defer func() {
		for _, listener := range listeners {
			_ = listener.Close()
		}
	}()
	ports := make([]int, 0, n)
	for range n {
		listener, err := net.Listen("tcp", "127.0.0.1:0")
		require.NoError(t, err)
		listeners = append(listeners, listener)
		ports = append(ports, listener.Addr().(*net.TCPAddr).Port)
	}

	require.Len(t, ports, n)
	return ports
}

// startSurfpool launches one offline surfpool instance under the stdin watchdog and blocks
// until its RPC answers getHealth. Test cleanup stops it.
func startSurfpool(t *testing.T) *surfpoolHarness {
	t.Helper()

	bin := surfpoolBinary(t)
	ports := freeLoopbackPorts(t, 3)
	rpcPort, wsPort, studioPort := ports[0], ports[1], ports[2]

	// Surfpool writes its workspace into the working directory.
	workdir, err := os.MkdirTemp("", fmt.Sprintf("ga-guardian-e2e-%d-", rpcPort))
	require.NoError(t, err)

	stdout, err := os.Create(filepath.Join(workdir, "surfpool.stdout.log"))
	require.NoError(t, err)
	stderr, err := os.Create(filepath.Join(workdir, "surfpool.stderr.log"))
	require.NoError(t, err)

	cmd := exec.Command("/bin/sh", "-c", surfpoolWatchdogScript, "sh", //nolint:gosec // bin comes from PATH or the install location
		bin,
		"start",
		"--no-tui",
		"--no-studio",
		"--no-deploy",
		"-y",
		"--port", fmt.Sprint(rpcPort),
		"--ws-port", fmt.Sprint(wsPort),
		"--studio-port", fmt.Sprint(studioPort),
		"--slot-time", surfpoolSlotTimeMillis,
		"--log-level", "warn",
		"--offline",
	)
	cmd.Dir = workdir
	cmd.Stdout = stdout
	cmd.Stderr = stderr
	// Own process group: a terminal interrupt reaches the test, which closes the watchdog pipe.
	cmd.SysProcAttr = &syscall.SysProcAttr{Setpgid: true}
	watchdog, err := cmd.StdinPipe()
	require.NoError(t, err)
	require.NoError(t, cmd.Start())
	t.Logf("surfpool started: pid=%d rpc=%d ws=%d logs=%s", cmd.Process.Pid, rpcPort, wsPort, workdir)

	exited := make(chan struct{})
	go func() {
		_ = cmd.Wait()
		close(exited)
	}()

	t.Cleanup(func() {
		stopSurfpool(t, cmd, watchdog, exited)
		stdout.Close()
		stderr.Close()
	})

	h := &surfpoolHarness{
		t:            t,
		rpcURL:       fmt.Sprintf("http://127.0.0.1:%d", rpcPort),
		wsURL:        fmt.Sprintf("ws://127.0.0.1:%d", wsPort),
		client:       &http.Client{Timeout: surfpoolRPCTimeout},
		healthClient: &http.Client{Timeout: surfpoolHealthTimeout},
		logDir:       workdir,
		exited:       exited,
	}
	h.waitForHealth()
	return h
}

// stopSurfpool closes the watchdog pipe and waits for sh to reap surfpool. After
// surfpoolStopTimeout it kills the whole process group.
func stopSurfpool(t *testing.T, cmd *exec.Cmd, watchdog io.WriteCloser, exited <-chan struct{}) {
	t.Helper()
	_ = watchdog.Close()
	select {
	case <-exited:
		return
	case <-time.After(surfpoolStopTimeout):
	}
	t.Logf("surfpool did not stop within %v, killing process group %d", surfpoolStopTimeout, cmd.Process.Pid)
	_ = syscall.Kill(-cmd.Process.Pid, syscall.SIGKILL)
	select {
	case <-exited:
	case <-time.After(surfpoolStopTimeout):
		t.Errorf("surfpool process group %d survived SIGKILL", cmd.Process.Pid)
	}
}

// waitForHealth polls getHealth until it answers, the process exits, or the boot budget
// runs out.
func (h *surfpoolHarness) waitForHealth() {
	h.t.Helper()
	deadline := time.Now().Add(surfpoolBootTimeout)
	var lastErr error
	for poll := 0; poll < surfpoolMaxReadyPolls && time.Now().Before(deadline); poll++ {
		select {
		case <-h.exited:
			h.t.Fatalf("surfpool exited before its RPC became healthy (logs in %s)", h.logDir)
		default:
		}
		_, err := h.rpcCall(h.healthClient, "getHealth", []any{})
		if err == nil {
			h.t.Logf("surfpool RPC ready at %s", h.rpcURL)
			return
		}
		lastErr = err
		time.Sleep(surfpoolReadyPoll)
	}
	h.t.Fatalf("surfpool RPC at %s was not healthy within %v: %v (logs in %s)", h.rpcURL, surfpoolBootTimeout, lastErr, h.logDir)
}

// rpcCall sends one JSON-RPC request and returns its result.
func (h *surfpoolHarness) rpcCall(client *http.Client, method string, params any) (json.RawMessage, error) {
	payload, err := json.Marshal(map[string]any{
		"jsonrpc": "2.0",
		"id":      1,
		"method":  method,
		"params":  params,
	})
	if err != nil {
		return nil, err
	}

	resp, err := client.Post(h.rpcURL, "application/json", bytes.NewReader(payload))
	if err != nil {
		return nil, err
	}
	defer resp.Body.Close()
	if resp.StatusCode != http.StatusOK {
		return nil, fmt.Errorf("%s: http status %d", method, resp.StatusCode)
	}

	var envelope struct {
		Result json.RawMessage `json:"result"`
		Error  *struct {
			Code    int    `json:"code"`
			Message string `json:"message"`
		} `json:"error"`
	}
	if err := json.NewDecoder(resp.Body).Decode(&envelope); err != nil {
		return nil, fmt.Errorf("%s: decode response: %w", method, err)
	}
	if envelope.Error != nil {
		return nil, fmt.Errorf("%s: rpc error %d: %s", method, envelope.Error.Code, envelope.Error.Message)
	}
	return envelope.Result, nil
}

// cheatCode sends one surfnet_* request, failing the test on any error.
func (h *surfpoolHarness) cheatCode(method string, params any) {
	h.t.Helper()
	_, err := h.rpcCall(h.client, method, params)
	require.NoError(h.t, err, "cheat code %s", method)
}

// writeProgram loads an ELF as an executable program account.
func (h *surfpoolHarness) writeProgram(id solana.PublicKey, elf []byte) {
	h.t.Helper()
	h.cheatCode("surfnet_writeProgram", []any{id.String(), hex.EncodeToString(elf), 0})
	h.t.Logf("deployed %d bytes at %s", len(elf), id)
}

// setAccount writes one non-executable account, replacing any existing one.
func (h *surfpoolHarness) setAccount(key solana.PublicKey, lamports uint64, owner solana.PublicKey, data []byte) {
	h.t.Helper()
	h.cheatCode("surfnet_setAccount", []any{
		key.String(),
		map[string]any{
			"lamports":   lamports,
			"owner":      owner.String(),
			"executable": false,
			"rent_epoch": 0,
			"data":       hex.EncodeToString(data),
		},
	})
}

// guardianSetAccountData builds a Core Bridge GuardianSet account body: index LE u32,
// key count LE u32, 20-byte keys, creation_time LE u32, expiration_time LE u32. Mirrors
// guardian_set_account in tests/common/guardians.rs.
//
// SECURITY: precondition len(keys) > 0; a set with no keys has no quorum.
func guardianSetAccountData(t *testing.T, index uint32, keys [][guardianSetKeyLen]byte, creationTime uint32, expirationTime uint32) []byte {
	t.Helper()
	require.NotEmpty(t, keys)
	wantLen := guardianSetHeaderLen + len(keys)*guardianSetKeyLen + guardianSetFooterLen

	out := make([]byte, 0, wantLen)
	out = binary.LittleEndian.AppendUint32(out, index)
	out = binary.LittleEndian.AppendUint32(out, uint32(len(keys))) // #nosec G115 -- a guardian set is far below 2^32
	for _, key := range keys {
		out = append(out, key[:]...)
	}
	out = binary.LittleEndian.AppendUint32(out, creationTime)
	out = binary.LittleEndian.AppendUint32(out, expirationTime)

	require.Len(t, out, wantLen)
	return out
}

// chainRegistrationAccountData builds a ChainRegistrationLayout account body: tag(1),
// pad(1), chain LE u16, governance_sequence LE u64, pad(20), emitter(32).
func chainRegistrationAccountData(t *testing.T, chain vaa.ChainID, emitter vaa.Address, governanceSequence uint64) []byte {
	t.Helper()

	out := make([]byte, chainRegistrationLen)
	out[0] = chainRegistrationTag
	binary.LittleEndian.PutUint16(out[2:4], uint16(chain))
	binary.LittleEndian.PutUint64(out[4:12], governanceSequence)
	copy(out[32:64], emitter[:])

	require.Len(t, out, chainRegistrationLen)
	return out
}
