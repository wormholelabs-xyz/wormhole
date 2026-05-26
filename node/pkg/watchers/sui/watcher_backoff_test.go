package sui

import (
	"context"
	"encoding/json"
	"fmt"
	"io"
	"net/http"
	"net/http/httptest"
	"sync/atomic"
	"testing"
	"time"

	"github.com/certusone/wormhole/node/pkg/common"
	gossipv1 "github.com/certusone/wormhole/node/pkg/proto/gossip/v1"
	"github.com/certusone/wormhole/node/pkg/readiness"
	"github.com/certusone/wormhole/node/pkg/supervisor"
	"github.com/stretchr/testify/require"
	"github.com/wormhole-foundation/wormhole/sdk/vaa"
	"go.uber.org/zap/zaptest"
)

func init() {
	// The readiness package uses a process-global registry that panics on double-register.
	// Tests here each spin up a Watcher.Run() which calls readiness.SetReady against the same
	// component key, so we suppress the panic for tests.
	readiness.NoPanic = true
}

// mockSuiRPC is a configurable JSON-RPC server stub used to exercise the watcher's HTTP path
// without a live Sui node. The handler returns a response body keyed by the JSON-RPC method
// name; callCounts tracks invocations per method so tests can assert hot-loop behaviour.
type mockSuiRPC struct {
	server     *httptest.Server
	callCounts map[string]*int64
}

func newMockSuiRPC(t *testing.T, handler func(method string) string) *mockSuiRPC {
	t.Helper()
	m := &mockSuiRPC{callCounts: make(map[string]*int64)}
	for _, method := range []string{
		"sui_getLatestCheckpointSequenceNumber",
		"suix_getLatestSuiSystemState",
		"suix_queryEvents",
		"sui_multiGetTransactionBlocks",
		"sui_getEvents",
		"sui_getTransactionBlock",
	} {
		var c int64
		m.callCounts[method] = &c
	}
	m.server = httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		defer r.Body.Close()
		raw, _ := io.ReadAll(r.Body)
		var req struct {
			Method string `json:"method"`
		}
		_ = json.Unmarshal(raw, &req)
		if counter, ok := m.callCounts[req.Method]; ok {
			atomic.AddInt64(counter, 1)
		}
		w.Header().Set("Content-Type", "application/json")
		_, _ = w.Write([]byte(handler(req.Method)))
	}))
	t.Cleanup(m.server.Close)
	return m
}

func (m *mockSuiRPC) calls(method string) int64 {
	return atomic.LoadInt64(m.callCounts[method])
}

// newTestWatcher constructs a minimally-configured Watcher pointing at a mock RPC. The
// production constructor (NewWatcher) is bypassed because it requires SDK lookups and txVerifier
// wiring that are orthogonal to the data-pump retry behaviour under test.
func newTestWatcher(rpcURL, moveEventType string) *Watcher {
	return &Watcher{
		suiRPC:                    rpcURL,
		suiMoveEventType:          moveEventType,
		msgChan:                   make(chan *common.MessagePublication, 16),
		obsvReqC:                  make(chan *gossipv1.ObservationRequest, 1),
		readinessSync:             common.MustConvertChainIdToReadinessSyncing(vaa.ChainIDSui),
		latestProcessedCheckpoint: 0,
		maximumBatchSize:          10,
		descendingOrder:           true,
		loopDelay:                 10 * time.Millisecond,
		queryEventsCmd: fmt.Sprintf(`{"jsonrpc":"2.0", "id": 1, "method": "suix_queryEvents", "params": [{ "MoveEventType": "%s" }, null, %d, %t]}`,
			moveEventType, 10, true),
		postTimeout: 5 * time.Second,
	}
}

// runWatcherUnderSupervisor runs w.Run under a real supervisor for the given duration, then
// cancels and waits for a clean shutdown. It blocks until the supervisor goroutine exits, so any
// post-call reads on the watcher are safely synchronized with writes from inside Run().
func runWatcherUnderSupervisor(t *testing.T, w *Watcher, runFor time.Duration) {
	t.Helper()
	rootCtx, cancel := context.WithCancel(context.Background())
	defer cancel()

	logger := zaptest.NewLogger(t)
	done := make(chan struct{})

	go supervisor.New(rootCtx, logger, func(ctx context.Context) error {
		if err := supervisor.Run(ctx, "sui-test", w.Run); err != nil {
			return err
		}
		supervisor.Signal(ctx, supervisor.SignalHealthy)
		<-ctx.Done()
		close(done)
		return nil
	}, supervisor.WithPropagatePanic)

	time.Sleep(runFor)
	cancel()

	select {
	case <-done:
	case <-time.After(2 * time.Second):
		t.Fatal("supervisor did not shut down within 2s of cancel")
	}
}

// Test_DataPump_AppliesBackoffOnPersistentError is a regression test for the unbounded retry
// hot-loop. With the exponential backoff added by this commit (start at loopDelay, double on
// each consecutive failure, capped at 30s), a 500ms window starting from loopDelay=10ms emits
// at most ~7 calls: 10ms + 20 + 40 + 80 + 160 + 320 -> the next attempt is past the window. The
// pre-fix code emitted thousands of calls in the same window. The 20-call ceiling gives a
// healthy safety margin against scheduler jitter while still failing loudly on any regression
// that removes or weakens the backoff.
func Test_DataPump_AppliesBackoffOnPersistentError(t *testing.T) {
	mock := newMockSuiRPC(t, func(method string) string {
		switch method {
		case "sui_getLatestCheckpointSequenceNumber":
			return `{"jsonrpc":"2.0","id":1,"result":"12345"}`
		case "suix_getLatestSuiSystemState":
			return `{"jsonrpc":"2.0","id":1,"result":{"epoch":"1","protocolVersion":"1"}}`
		case "suix_queryEvents":
			return `{"jsonrpc":"2.0","id":1,"result":{"data":[],"nextCursor":null,"hasNextPage":false}}`
		}
		return `{"jsonrpc":"2.0","id":1,"result":null}`
	})

	w := newTestWatcher(mock.server.URL, "0xabc::publish_message::WormholeMessage")
	readiness.RegisterComponent(w.readinessSync)

	runWatcherUnderSupervisor(t, w, 500*time.Millisecond)

	calls := mock.calls("suix_queryEvents")
	require.LessOrEqual(t, calls, int64(20),
		"expected ≤20 suix_queryEvents calls in 500ms with exponential backoff; got %d. "+
			"A regression that removes the backoff would push this into the thousands.",
		calls)
	require.GreaterOrEqual(t, calls, int64(1),
		"the data pump must still issue at least one call; got %d", calls)
}
