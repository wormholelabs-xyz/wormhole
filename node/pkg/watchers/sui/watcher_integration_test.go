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

// mockSuiRPC is a configurable JSON-RPC server stub used to exercise the watcher's HTTP path
// without a live Sui node. The handler returns a response body keyed by the JSON-RPC method
// name; callCounts tracks invocations per method so tests can assert hot-loop behavior.
type mockSuiRPC struct {
	server     *httptest.Server
	callCounts map[string]*int64
}

func init() {
	// The readiness package uses a process-global registry that panics on double-register.
	// Multiple integration tests in this file each spin up a Watcher.Run() which calls
	// readiness.SetReady against the same component key, so we suppress the panic for tests.
	readiness.NoPanic = true
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
// wiring that are orthogonal to the data-pump retry behavior under test.
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

// Test_GetEvents_EmptyResultSurfacesAsError is a regression test for the silent-empty failure
// mode that drove the 2026-05-25 high-CPU incident on vm-guardian-01. On non-devnet networks
// the core bridge has emitted events, so an empty result from suix_queryEvents indicates an
// upstream RPC fault (e.g. pruning, index drift) rather than a benign "no events" state. The
// fix surfaces this as an error so operators can rotate the endpoint, and the pump loop's
// backoff bounds the retry rate.
func Test_GetEvents_EmptyResultSurfacesAsError(t *testing.T) {
	mock := newMockSuiRPC(t, func(method string) string {
		if method == "suix_queryEvents" {
			return `{"jsonrpc":"2.0","id":1,"result":{"data":[],"nextCursor":null,"hasNextPage":false}}`
		}
		return `{"jsonrpc":"2.0","id":1,"result":null}`
	})

	w := newTestWatcher(mock.server.URL, "0xabc::publish_message::WormholeMessage")
	_, err := w.getEvents(context.Background())

	require.Error(t, err)
	require.Contains(t, err.Error(), "suspect upstream pruning")
	require.Equal(t, int64(1), mock.calls("suix_queryEvents"))
}

// Test_GetEvents_PropagatesUpstreamRPCError exercises the full call chain from getEvents
// down through suiQueryEvents and into the JSON-RPC error decode added by this commit. With
// the Error field present on SuiEventResponse, an upstream RPC failure (rate limit, pruned
// transaction, invalid params, etc.) is no longer silently coerced to an empty result; the
// code and message reach the caller intact so operators can diagnose the actual failure.
func Test_GetEvents_PropagatesUpstreamRPCError(t *testing.T) {
	mock := newMockSuiRPC(t, func(method string) string {
		if method == "suix_queryEvents" {
			return `{"jsonrpc":"2.0","id":1,"error":{"code":-32603,"message":"transaction events pruned"}}`
		}
		return `{"jsonrpc":"2.0","id":1,"result":null}`
	})

	w := newTestWatcher(mock.server.URL, "0xabc::publish_message::WormholeMessage")
	_, err := w.getEvents(context.Background())

	require.Error(t, err)
	require.Contains(t, err.Error(), "-32603")
	require.Contains(t, err.Error(), "transaction events pruned")
	require.NotContains(t, err.Error(), "suspect upstream pruning",
		"a JSON-RPC error must not be coerced to the generic empty-result message")
}

// Test_GetEvents_PartialProgressIsDiscardedOnEmptyPage documents pagination behavior that this
// commit does not change: when page 1 returns events with HasNextPage=true and page 2 returns
// an empty result set, getEvents returns the empty-page error and the events collected from
// page 1 are discarded. The pump loop's backoff now bounds the retry rate, so this no longer
// produces a hot loop, but the partial-progress loss remains and is worth marking explicitly
// in case a future change opts to return the accumulated events instead of discarding them.
func Test_GetEvents_PartialProgressIsDiscardedOnEmptyPage(t *testing.T) {
	var queryCalls int64
	mock := newMockSuiRPC(t, func(method string) string {
		switch method {
		case "suix_queryEvents":
			n := atomic.AddInt64(&queryCalls, 1)
			if n == 1 {
				return `{"jsonrpc":"2.0","id":1,"result":{"data":[{"id":{"txDigest":"DIGEST1","eventSeq":"0"},"packageId":"0xabc","transactionModule":"publish_message","sender":"0xdead","type":"0xabc::publish_message::WormholeMessage","parsedJson":{},"bcs":"","timestampMs":"0"}],"nextCursor":{"txDigest":"DIGEST1","eventSeq":"0"},"hasNextPage":true}}`
			}
			return `{"jsonrpc":"2.0","id":1,"result":{"data":[],"nextCursor":null,"hasNextPage":false}}`
		case "sui_getTransactionBlock":
			return `{"jsonrpc":"2.0","id":1,"result":{"digest":"DIGEST1","checkpoint":"99999999"}}`
		}
		return `{"jsonrpc":"2.0","id":1,"result":null}`
	})

	w := newTestWatcher(mock.server.URL, "0xabc::publish_message::WormholeMessage")
	retVal, err := w.getEvents(context.Background())

	require.Error(t, err)
	require.Contains(t, err.Error(), "suspect upstream pruning")
	require.Empty(t, retVal, "events collected on page 1 are discarded when a later page returns empty")
}

// runWatcherUnderSupervisor drives the watcher's full Run() loop under a real supervisor
// context for the requested duration. The mock RPC is supplied by the caller. The function
// blocks until the supervisor goroutine exits, so any post-call reads on the watcher are
// safely synchronized with writes from inside Run().
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

// Test_DataPump_AppliesBackoffOnPersistentError is a regression test for the no-backoff hot
// loop that drove ~24 getEvents calls/sec against the upstream RPC during the 2026-05-25
// incident. With the exponential backoff added by this commit (start at loopDelay, double on
// each consecutive failure, capped at 30s), a 500ms window starting from loopDelay=10ms emits
// at most ~7 calls: 10ms + 20 + 40 + 80 + 160 + 320 → next attempt is past the window. The
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

// Test_Restart_SetsCheckpointToHead proves that on every (re)start the watcher re-anchors
// latestProcessedCheckpoint to the current Sui head via sui_getLatestCheckpointSequenceNumber
// at watcher.go:407-411. This refutes the "stuck walking back to a stale cursor" theory: a
// fresh process cannot be persistently descending through historical events, because each
// start sets the floor to whatever the RPC reports as `latest` at that moment. Any persistent
// post-restart failure must therefore originate from page 1 of suix_queryEvents itself, not
// from the pagination walk.
func Test_Restart_SetsCheckpointToHead(t *testing.T) {
	const headCheckpoint int64 = 42424242

	mock := newMockSuiRPC(t, func(method string) string {
		switch method {
		case "sui_getLatestCheckpointSequenceNumber":
			return fmt.Sprintf(`{"jsonrpc":"2.0","id":1,"result":"%d"}`, headCheckpoint)
		case "suix_getLatestSuiSystemState":
			return `{"jsonrpc":"2.0","id":1,"result":{"epoch":"1","protocolVersion":"1"}}`
		case "suix_queryEvents":
			// Force the pump's error branch so latestProcessedCheckpoint cannot be advanced
			// further by the success path at watcher.go:445-447. Any value observed after
			// shutdown therefore came from the startup assignment, not from event processing.
			return `{"jsonrpc":"2.0","id":1,"result":{"data":[],"nextCursor":null,"hasNextPage":false}}`
		}
		return `{"jsonrpc":"2.0","id":1,"result":null}`
	})

	w := newTestWatcher(mock.server.URL, "0xabc::publish_message::WormholeMessage")
	w.latestProcessedCheckpoint = 0
	readiness.RegisterComponent(w.readinessSync)

	runWatcherUnderSupervisor(t, w, 200*time.Millisecond)

	require.Equal(t, headCheckpoint, w.latestProcessedCheckpoint,
		"on (re)start the watcher must anchor latestProcessedCheckpoint to the current Sui head")
}

