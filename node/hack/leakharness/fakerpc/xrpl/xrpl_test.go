package xrpl_test

import (
	"context"
	"testing"
	"time"

	"github.com/stretchr/testify/require"

	"github.com/certusone/wormhole/node/hack/leakharness/fakerpc/common"
	"github.com/certusone/wormhole/node/hack/leakharness/fakerpc/xrpl"
)

func TestXRPLFamily_HealthyRoundtrips(t *testing.T) {
	srv := xrpl.New()
	defer srv.Stop()

	worker := &xrpl.Worker{}
	ctx, cancel := context.WithTimeout(context.Background(), 500*time.Millisecond)
	defer cancel()
	worker.Run(ctx, srv.URL())

	require.Greater(t, srv.RequestCount(), int64(2),
		"healthy worker should complete multiple WS roundtrips in 500ms; got %d", srv.RequestCount())
}

func TestXRPLFamily_FaultCloseDropsConnections(t *testing.T) {
	srv := xrpl.New()
	defer srv.Stop()

	worker := &xrpl.Worker{}
	ctx, cancel := context.WithTimeout(context.Background(), 500*time.Millisecond)
	defer cancel()

	// Toggle the fault mid-run so the worker is forced to redial.
	go func() {
		time.Sleep(150 * time.Millisecond)
		srv.Set(common.FaultCloseAllConnections)
		time.Sleep(100 * time.Millisecond)
		srv.Set(common.FaultNone)
	}()

	worker.Run(ctx, srv.URL())

	require.Greater(t, srv.RequestCount(), int64(1),
		"worker must reconnect after WS close; got %d", srv.RequestCount())
}
