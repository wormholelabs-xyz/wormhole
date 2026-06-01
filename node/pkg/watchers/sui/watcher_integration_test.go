package sui

import (
	"context"
	"fmt"
	"sync/atomic"
	"testing"
	"time"

	"github.com/certusone/wormhole/node/pkg/readiness"
	"github.com/stretchr/testify/require"
)

// Additional Sui watcher integration tests. The shared helpers (mockSuiRPC, newMockSuiRPC,
// newTestWatcher, runWatcherUnderSupervisor) and the backoff regression test live in
// watcher_backoff_test.go in this same package.

// Test_GetEvents_EmptyResultSurfacesAsError verifies that on non-devnet networks an empty
// suix_queryEvents result is surfaced as an error rather than a benign "no events" state: a
// non-devnet core bridge has emitted events, so an empty result indicates an upstream data gap
// (e.g. pruning, index drift). The pump loop's backoff bounds the retry rate.
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

// Test_GetEvents_PropagatesUpstreamRPCError exercises the full call chain from getEvents down
// through suiQueryEvents and into the JSON-RPC error decode. With the Error field present on
// SuiEventResponse, an upstream RPC failure (rate limit, pruned transaction, invalid params,
// etc.) is no longer silently coerced to an empty result; the code and message reach the caller
// intact.
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

// Test_GetEvents_PartialProgressIsDiscardedOnEmptyPage documents pagination behaviour that the
// fix does not change: when page 1 returns events with HasNextPage=true and page 2 returns an
// empty result set, getEvents returns the empty-page error and the events collected from page 1
// are discarded. The pump loop's backoff bounds the retry rate, so this no longer produces a hot
// loop, but the partial-progress loss remains and is worth marking explicitly in case a future
// change opts to return the accumulated events instead of discarding them.
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

// Test_Restart_SetsCheckpointToHead proves that on every (re)start the watcher re-anchors
// latestProcessedCheckpoint to the current Sui head via sui_getLatestCheckpointSequenceNumber.
// A fresh process cannot be persistently descending through historical events, because each
// start sets the floor to whatever the RPC reports as latest at that moment. Any persistent
// post-restart failure must therefore originate from page 1 of suix_queryEvents itself, not from
// the pagination walk.
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
			// further by the success path. Any value observed after shutdown therefore came
			// from the startup assignment, not from event processing.
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

// Test_GetEvents_PrunedTxOnPageOne_GetCheckpointForDigest probes a transaction whose body has
// been pruned from the upstream RPC's transaction store while still appearing in the event
// index. The watcher calls getCheckpointForDigest (sui_getTransactionBlock) against the oldest
// tx in each page to decide whether to paginate further; if that tx has been pruned, the RPC
// returns a JSON-RPC error -32602. GetCheckpointResponse has no Error field today (a sibling
// defect to the SuiEventResponse fix), so json.Unmarshal silently drops the error,
// Result.Checkpoint stays empty, and the watcher reports an opaque ParseInt("") failure instead
// of the actionable upstream-pruning message.
func Test_GetEvents_PrunedTxOnPageOne_GetCheckpointForDigest(t *testing.T) {
	mock := newMockSuiRPC(t, func(method string) string {
		switch method {
		case "suix_queryEvents":
			// Page 1: a single event referencing a tx that exists in the event index but not in
			// the transaction store. HasNextPage=true so we reach the getCheckpointForDigest
			// call before any break-condition shortcut fires.
			return `{"jsonrpc":"2.0","id":1,"result":{"data":[{"id":{"txDigest":"PRUNED_TX_ON_PAGE_ONE","eventSeq":"0"},"packageId":"0xabc","transactionModule":"publish_message","sender":"0xdead","type":"0xabc::publish_message::WormholeMessage","parsedJson":{},"bcs":"","timestampMs":"0"}],"nextCursor":{"txDigest":"PRUNED_TX_ON_PAGE_ONE","eventSeq":"0"},"hasNextPage":true}}`
		case "sui_getTransactionBlock":
			return `{"jsonrpc":"2.0","id":1,"error":{"code":-32602,"message":"Could not find the referenced transaction [TransactionDigest(PRUNED_TX_ON_PAGE_ONE)]."}}`
		}
		return `{"jsonrpc":"2.0","id":1,"result":null}`
	})

	w := newTestWatcher(mock.server.URL, "0xabc::publish_message::WormholeMessage")
	_, err := w.getEvents(context.Background())

	require.Error(t, err)
	// Current behaviour demonstrates the sibling silent-coercion defect: the upstream -32602 /
	// "Could not find the referenced transaction" message is lost, and the caller sees a generic
	// ParseInt failure that gives no hint about the actual cause. Once GetCheckpointResponse
	// grows an Error field analogous to SuiEventResponse.Error, this assertion should flip to
	// check for "-32602" and "Could not find the referenced transaction" in the surfaced error.
	require.Contains(t, err.Error(), "getCheckpointForDigest failed to ParseInt",
		"sibling defect: GetCheckpointResponse silently coerces JSON-RPC errors to ParseInt failure")
}

// Test_GetEvents_PrunedTxInBatch_MultiGetTransactionBlocks documents the analogous defect on the
// batched checkpoint lookup. When the watcher gets past the per-tx checkpoint check and calls
// getMultipleBlocks (sui_multiGetTransactionBlocks), the RPC returns one entry per requested
// digest, but pruned entries are degraded to digest-only (no checkpoint field). The watcher then
// ParseInt("")s on the missing field. MultipleBlockResult / TxBlockResult also have no Error
// field, so a JSON-RPC error from this method would similarly be coerced to an empty-shape
// response and produce the same ParseInt failure.
func Test_GetEvents_PrunedTxInBatch_MultiGetTransactionBlocks(t *testing.T) {
	mock := newMockSuiRPC(t, func(method string) string {
		switch method {
		case "suix_queryEvents":
			// Page 1 with one event; HasNextPage=false so we break out of pagination and reach
			// the getMultipleBlocks call directly.
			return `{"jsonrpc":"2.0","id":1,"result":{"data":[{"id":{"txDigest":"PRUNED_BATCH_TX","eventSeq":"0"},"packageId":"0xabc","transactionModule":"publish_message","sender":"0xdead","type":"0xabc::publish_message::WormholeMessage","parsedJson":{},"bcs":"","timestampMs":"0"}],"nextCursor":null,"hasNextPage":false}}`
		case "sui_getTransactionBlock":
			// Allow the per-tx checkpoint lookup to succeed so we reach getMultipleBlocks with a
			// tx the watcher believes is fresh.
			return `{"jsonrpc":"2.0","id":1,"result":{"digest":"PRUNED_BATCH_TX","checkpoint":"99999999"}}`
		case "sui_multiGetTransactionBlocks":
			// One entry per digest, but pruned txs come back with only the digest field
			// populated — no checkpoint.
			return `{"jsonrpc":"2.0","id":1,"result":[{"digest":"PRUNED_BATCH_TX"}]}`
		}
		return `{"jsonrpc":"2.0","id":1,"result":null}`
	})

	w := newTestWatcher(mock.server.URL, "0xabc::publish_message::WormholeMessage")
	_, err := w.getEvents(context.Background())

	require.Error(t, err)
	require.Contains(t, err.Error(), "getEvents failed to ParseInt",
		"missing checkpoint field on a batched response is surfaced as opaque ParseInt failure")
}
