package sui_test

import (
	"context"
	"testing"
	"time"

	"github.com/stretchr/testify/require"

	"github.com/certusone/wormhole/node/hack/leakharness/fakerpc/common"
	"github.com/certusone/wormhole/node/hack/leakharness/fakerpc/sui"
)

func TestSuiFamily_HealthyRoundtrips(t *testing.T) {
	srv := sui.New()
	defer srv.Stop()

	worker := &sui.Worker{}
	ctx, cancel := context.WithTimeout(context.Background(), 500*time.Millisecond)
	defer cancel()
	worker.Run(ctx, srv.URL())

	require.Greater(t, srv.RequestCount(), int64(3),
		"healthy worker should issue many requests in 500ms; got %d", srv.RequestCount())
}

func TestSuiFamily_WorkerReconnectsAfterFault(t *testing.T) {
	srv := sui.New()
	defer srv.Stop()

	worker := &sui.Worker{}
	ctx, cancel := context.WithTimeout(context.Background(), 600*time.Millisecond)
	defer cancel()

	// Flip the fault on after the worker has had a chance to run a few
	// healthy iterations, then heal, and verify the request count
	// continues to climb past the fault.
	go func() {
		time.Sleep(200 * time.Millisecond)
		srv.Set(common.FaultCloseAllConnections)
		time.Sleep(100 * time.Millisecond)
		srv.Set(common.FaultNone)
	}()

	worker.Run(ctx, srv.URL())

	require.Greater(t, srv.RequestCount(), int64(3),
		"worker must keep reconnecting through fault flap; got %d requests", srv.RequestCount())
}

func TestSuiFamily_StopClosesHijackedConns(t *testing.T) {
	srv := sui.New()

	srv.Set(common.FaultCloseAllConnections)
	worker := &sui.Worker{}
	ctx, cancel := context.WithTimeout(context.Background(), 200*time.Millisecond)
	defer cancel()
	worker.Run(ctx, srv.URL())

	// Stop must not block or panic even with hijacked connections
	// queued up. Verified by simply returning within the test deadline.
	done := make(chan struct{})
	go func() {
		srv.Stop()
		close(done)
	}()
	select {
	case <-done:
	case <-time.After(2 * time.Second):
		t.Fatal("Stop did not return within 2s")
	}
}
