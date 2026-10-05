// Package solacctconn is the production Conn: the Solana JSON-RPC and
// websocket surface the guardian's Solana accountant backend uses. It mirrors
// node/pkg/wormconn.
package solacctconn

import (
	"errors"
	"fmt"
	"io"
	"net/http"
	"time"

	"github.com/gagliardetto/solana-go/rpc"
	"github.com/gagliardetto/solana-go/rpc/jsonrpc"
)

const (
	// Bounds one decoded RPC response body. getProgramAccounts at 88-byte pending accounts
	// fits about 100,000 accounts, ten times the audit read cap.
	maxRPCResponseBytes = 32 << 20

	// Bounds one RPC request, as the solana-go default does.
	rpcRequestTimeout = 5 * time.Minute
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
// SECURITY: both endpoints must be set. The flag layer also checks that both are set.
// It also checks their schemes. The first request opens the connection.
func NewConn(rpcURL string, wsURL string) (*ClientConn, error) {
	return newConn(rpcURL, wsURL, maxRPCResponseBytes)
}

// newConn is NewConn with a response body limit of maxResponseBytes.
func newConn(rpcURL string, wsURL string, maxResponseBytes int64) (*ClientConn, error) {
	if rpcURL == "" {
		return nil, errors.New("solana accountant connection: the rpc endpoint is required")
	}
	if wsURL == "" {
		return nil, errors.New("solana accountant connection: the websocket endpoint is required")
	}
	if maxResponseBytes <= 0 {
		return nil, fmt.Errorf("solana accountant connection: response limit %d is not positive", maxResponseBytes)
	}
	httpClient := &limitedHTTPClient{
		inner:    &http.Client{Timeout: rpcRequestTimeout},
		maxBytes: maxResponseBytes,
	}
	rpcClient := jsonrpc.NewClientWithOpts(rpcURL, &jsonrpc.RPCClientOpts{HTTPClient: httpClient})
	return &ClientConn{rpc: rpc.NewWithCustomRPCClient(rpcClient), wsURL: wsURL}, nil
}

func (c *ClientConn) Close() {
	if c.rpc != nil {
		_ = c.rpc.Close()
	}
}

// limitedHTTPClient fails the read of a response body past maxBytes.
type limitedHTTPClient struct {
	inner    *http.Client
	maxBytes int64
}

func (c *limitedHTTPClient) Do(req *http.Request) (*http.Response, error) {
	resp, err := c.inner.Do(req)
	if err != nil {
		return nil, err
	}
	resp.Body = &limitedBody{body: resp.Body, limit: c.maxBytes, remaining: c.maxBytes}
	return resp, nil
}

func (c *limitedHTTPClient) CloseIdleConnections() {
	c.inner.CloseIdleConnections()
}

// limitedBody returns an error once a read passes the limit. A silent truncation would
// surface as a confusing JSON decode error.
type limitedBody struct {
	body      io.ReadCloser
	limit     int64
	remaining int64
}

func (b *limitedBody) Read(p []byte) (int, error) {
	if b.remaining < 0 {
		return 0, fmt.Errorf("rpc response body is past the %d-byte size limit", b.limit)
	}
	// Read one byte past the limit to tell an exact fit from an overflow.
	if int64(len(p)) > b.remaining+1 {
		p = p[:b.remaining+1]
	}
	n, err := b.body.Read(p)
	b.remaining -= int64(n)
	if b.remaining < 0 {
		return 0, fmt.Errorf("rpc response body is past the %d-byte size limit", b.limit)
	}
	return n, err
}

func (b *limitedBody) Close() error {
	return b.body.Close()
}
