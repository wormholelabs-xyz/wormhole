package cosmwasm_test

import (
	"context"
	"testing"
	"time"

	"github.com/stretchr/testify/require"

	"github.com/certusone/wormhole/node/hack/leakharness/fakerpc/common"
	"github.com/certusone/wormhole/node/hack/leakharness/fakerpc/cosmwasm"
)

func TestCosmwasmFamily_HealthyRoundtrips(t *testing.T) {
	srv := cosmwasm.New()
	defer srv.Stop()

	worker := &cosmwasm.Worker{}
	ctx, cancel := context.WithTimeout(context.Background(), 500*time.Millisecond)
	defer cancel()
	worker.Run(ctx, srv.URL())

	require.Greater(t, srv.RequestCount(), int64(2),
		"healthy worker should issue multiple POSTs in 500ms; got %d", srv.RequestCount())
}

func TestCosmwasmFamily_StopAfterFault(t *testing.T) {
	srv := cosmwasm.New()
	srv.Set(common.FaultCloseAllConnections)

	worker := &cosmwasm.Worker{}
	ctx, cancel := context.WithTimeout(context.Background(), 200*time.Millisecond)
	defer cancel()
	worker.Run(ctx, srv.URL())

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

func TestCosmwasmFamily_WorkerReconnectsAfterFault(t *testing.T) {
	srv := cosmwasm.New()
	defer srv.Stop()

	worker := &cosmwasm.Worker{}
	ctx, cancel := context.WithTimeout(context.Background(), 600*time.Millisecond)
	defer cancel()

	go func() {
		time.Sleep(200 * time.Millisecond)
		srv.Set(common.FaultCloseAllConnections)
		time.Sleep(100 * time.Millisecond)
		srv.Set(common.FaultNone)
	}()

	worker.Run(ctx, srv.URL())

	require.Greater(t, srv.RequestCount(), int64(2),
		"worker must continue polling through the fault flap; got %d", srv.RequestCount())
}
