package evm

import (
	"context"
	"time"

	ethCommon "github.com/ethereum/go-ethereum/common"
	"go.uber.org/zap"

	"github.com/certusone/wormhole/node/hack/leakharness/fakerpc/common"
	"github.com/certusone/wormhole/node/pkg/watchers/evm/connectors"
)

// Family exposes the EVM fake to the harness orchestrator.
var Family = common.Family{
	Name: "evm",
	NewServer: func(id uint64) common.FaultableServer {
		return New(id)
	},
	NewWorker: func() common.Worker {
		logger := zap.NewNop()
		return &Worker{logger: logger}
	},
}

// Worker exercises the EthereumBaseConnector lifecycle: dial against
// the fake, hold the connection briefly, Close. This is the cycle that
// production supervisor restarts trace through; the leak this whole
// harness exists to detect lives here.
type Worker struct {
	logger *zap.Logger
}

func (w *Worker) Run(ctx context.Context, serverURL string) {
	addr := ethCommon.HexToAddress("0x0000000000000000000000000000000000000000")
	for {
		if ctx.Err() != nil {
			return
		}
		dialCtx, dialCancel := context.WithTimeout(ctx, 5*time.Second)
		conn, err := connectors.NewEthereumBaseConnector(dialCtx, "evm", serverURL, addr, nil, w.logger)
		dialCancel()
		if err != nil {
			select {
			case <-ctx.Done():
				return
			case <-time.After(50 * time.Millisecond):
			}
			continue
		}
		select {
		case <-ctx.Done():
			_ = conn.Close()
			return
		case <-time.After(100 * time.Millisecond):
		}
		_ = conn.Close()
	}
}

// LeakFamily is the EVM family with a deliberately leaking worker. It
// shares the same fake server but its worker abandons connectors without
// Close, reproducing the pre-fix supervisor-restart leak. Scenarios use
// it (fake: evm_leak) as the negative control that proves the harness
// detects the leak — pair it with max_goroutine_growth to assert.
var LeakFamily = common.Family{
	Name: "evm_leak",
	NewServer: func(id uint64) common.FaultableServer {
		return New(id)
	},
	NewWorker: func() common.Worker {
		return &LeakWorker{logger: zap.NewNop(), maxLeak: 60}
	},
}

// LeakWorker dials EthereumBaseConnectors and abandons them WITHOUT
// Close. Against a healthy fake the dropped connector's rpc.Client
// read/write/dispatch goroutines never exit (the live socket keeps them
// — and the connector — reachable), so each dial leaks ~3 goroutines and
// one socket. maxLeak caps the total so the self-test cannot exhaust the
// process FD limit; once reached the worker idles, holding the leak.
type LeakWorker struct {
	logger  *zap.Logger
	maxLeak int
}

func (w *LeakWorker) Run(ctx context.Context, serverURL string) {
	addr := ethCommon.HexToAddress("0x0000000000000000000000000000000000000000")
	leaked := 0
	for {
		if ctx.Err() != nil {
			return
		}
		if leaked < w.maxLeak {
			dialCtx, dialCancel := context.WithTimeout(ctx, 5*time.Second)
			conn, err := connectors.NewEthereumBaseConnector(dialCtx, "evm", serverURL, addr, nil, w.logger)
			dialCancel()
			if err == nil {
				// Intentionally NOT closed: the live connection keeps the
				// connector and its goroutines alive. This is the leak.
				_ = conn
				leaked++
			}
		}
		select {
		case <-ctx.Done():
			return
		case <-time.After(200 * time.Millisecond):
		}
	}
}
