package connectors

import (
	"context"
	"net/http"
	"net/http/httptest"
	"runtime"
	"strings"
	"testing"
	"time"

	ethCommon "github.com/ethereum/go-ethereum/common"
	"github.com/gorilla/websocket"
	"github.com/stretchr/testify/require"
	"go.uber.org/goleak"
	"go.uber.org/zap/zaptest"
)

// fakeEthRPCServer is an in-process JSON-RPC endpoint that accepts WebSocket
// upgrades and idles. It is the minimum surface area required to exercise
// EthereumBaseConnector's lifecycle: the constructor only dials, so the
// server does not need to answer any RPC methods. Idle connections let the
// go-ethereum rpc.Client spawn its dispatch/read/write goroutines, which is
// the goroutine cohort whose ownership Close() must reclaim.
func fakeEthRPCServer(t *testing.T) string {
	t.Helper()
	upgrader := websocket.Upgrader{
		CheckOrigin: func(r *http.Request) bool { return true },
	}
	srv := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		conn, err := upgrader.Upgrade(w, r, nil)
		if err != nil {
			return
		}
		// Hold the connection open until the peer closes it. We do not need
		// to answer any methods because the constructor does no RPC calls.
		for {
			if _, _, err := conn.NextReader(); err != nil {
				_ = conn.Close()
				return
			}
		}
	}))
	t.Cleanup(srv.Close)
	return "ws" + strings.TrimPrefix(srv.URL, "http")
}

// goroutineDelta returns the number of goroutines currently running minus
// the baseline. It runs a GC and a small settle delay so transient goroutines
// from the previous step have time to exit.
func goroutineDelta(baseline int) int {
	runtime.GC()
	time.Sleep(200 * time.Millisecond)
	return runtime.NumGoroutine() - baseline
}

// TestEthereumBaseConnector_CloseReleasesGoroutines asserts the central API
// contract under test: constructing N connectors and Close()-ing each must
// not leak goroutines.
//
// (*evm.Watcher).Run() replaced w.ethConn on each supervisor restart without
// closing the prior client, and the go-ethereum *rpc.Client spawns
// dispatch/read/write goroutines on Dial that are reclaimed only by Close().
func TestEthereumBaseConnector_CloseReleasesGoroutines(t *testing.T) {
	url := fakeEthRPCServer(t)
	logger := zaptest.NewLogger(t)
	addr := ethCommon.HexToAddress("0x0000000000000000000000000000000000000000")

	// Warm-up dial: the very first WebSocket upgrade in a test process spins
	// up TLS/HTTP transport singletons that are not reclaimed by Close().
	// Doing this before snapshotting the goroutine baseline excludes that
	// noise from the assertion.
	ctx, cancel := context.WithTimeout(context.Background(), 5*time.Second)
	defer cancel()
	warmup, err := NewEthereumBaseConnector(ctx, "warmup", url, addr, nil, logger)
	require.NoError(t, err)
	require.NoError(t, warmup.Close(), "Close() must succeed on a freshly-dialed connector")

	// IgnoreCurrent snapshots the goroutines that already exist here so the
	// check at the end only flags goroutines spawned by the dial/close loop
	// below. This baseline is required on every platform: the httptest server's
	// serve loops are still running (t.Cleanup tears them down only after this
	// deferred check), and the warm-up dial above leaves net/http's shared
	// transport connection pool active. Neither is a leak.
	defer goleak.VerifyNone(t, goleak.IgnoreCurrent())

	baseline := func() int {
		runtime.GC()
		time.Sleep(200 * time.Millisecond)
		return runtime.NumGoroutine()
	}()

	const restarts = 25
	for i := 0; i < restarts; i++ {
		dialCtx, dialCancel := context.WithTimeout(context.Background(), 5*time.Second)
		c, err := NewEthereumBaseConnector(dialCtx, "test", url, addr, nil, logger)
		dialCancel()
		require.NoError(t, err, "dial %d", i)
		require.NoError(t, c.Close(), "close %d", i)
	}

	delta := goroutineDelta(baseline)
	// Five is a generous upper bound: go-ethereum's *rpc.Client spawns
	// roughly three goroutines per dial (dispatch + read + write), so a leak
	// of even a single un-closed connector across 25 restarts would push the
	// delta to ≳ 75. Five accommodates GC timing jitter without masking the
	// regression.
	require.LessOrEqual(t, delta, 5,
		"goroutines leaked across %d dial/close cycles (delta=%d); Close() is not reclaiming the rpc.Client dispatcher", restarts, delta)
}
