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
