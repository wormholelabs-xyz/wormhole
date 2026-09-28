// Package solacctconn is the production Conn: the Solana JSON-RPC and
// websocket surface the guardian's Solana accountant backend uses. It mirrors
// node/pkg/wormconn.
package solacctconn

import (
	"errors"

	"github.com/gagliardetto/solana-go/rpc"
)

var _ Conn = (*ClientConn)(nil)

// ClientConn is a connection to one Solana cluster.
type ClientConn struct {
	rpc   *rpc.Client
	wsURL string
}

// NewConn creates a connection to the Solana RPC endpoint at rpcURL and records the
// websocket endpoint at wsURL for log subscriptions.
//
// SECURITY: both endpoints are required. The flag layer also checks that both are set and
// checks their schemes. The first request opens the connection.
func NewConn(rpcURL string, wsURL string) (*ClientConn, error) {
	if rpcURL == "" {
		return nil, errors.New("solana accountant connection: the rpc endpoint is required")
	}
	if wsURL == "" {
		return nil, errors.New("solana accountant connection: the websocket endpoint is required")
	}
	return &ClientConn{rpc: rpc.New(rpcURL), wsURL: wsURL}, nil
}

func (c *ClientConn) Close() {
	if c.rpc != nil {
		_ = c.rpc.Close()
	}
}
