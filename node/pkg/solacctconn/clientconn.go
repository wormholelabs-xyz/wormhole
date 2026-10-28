// Package solacctconn is the production Conn: the Solana JSON-RPC and
// websocket surface the guardian's Solana accountant backend uses. It mirrors
// node/pkg/wormconn.
package solacctconn

import (
	"bytes"
	"context"
	"errors"
	"fmt"
	"io"
	"net/http"
	"net/url"
	"strings"
	"time"

	"github.com/gagliardetto/solana-go/rpc"
	"github.com/gagliardetto/solana-go/rpc/jsonrpc"
)

const (
	// Bounds one decoded RPC response body. getProgramAccounts at 152-byte pending accounts
	// fits about 90,000 accounts.
	maxRPCResponseBytes = 32 << 20

	// Bounds one RPC request, as the solana-go default does.
	rpcRequestTimeout = 5 * time.Minute
)

var _ Conn = (*ClientConn)(nil)

// ErrResponseTooLarge marks an RPC response body past the size limit.
var ErrResponseTooLarge = errors.New("rpc response body is past the size limit")

// ClientConn is a connection to one Solana cluster.
type ClientConn struct {
	rpc      *rpc.Client
	wsURL    string
	redactor endpointRedactor
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
	redactor := newEndpointRedactor(rpcURL, wsURL)
	rpcClient := &redactingRPCClient{
		inner:    jsonrpc.NewClientWithOpts(rpcURL, &jsonrpc.RPCClientOpts{HTTPClient: httpClient}),
		redactor: redactor,
	}
	return &ClientConn{rpc: rpc.NewWithCustomRPCClient(rpcClient), wsURL: wsURL, redactor: redactor}, nil
}

func (c *ClientConn) Close() {
	if c.rpc != nil {
		_ = c.rpc.Close()
	}
}

// limitedHTTPClient reads each response body in full, up to maxBytes. A body past maxBytes
// fails Do with ErrResponseTooLarge.
type limitedHTTPClient struct {
	inner    *http.Client
	maxBytes int64
}

func (c *limitedHTTPClient) Do(req *http.Request) (*http.Response, error) {
	resp, err := c.inner.Do(req)
	if err != nil {
		return nil, err
	}
	defer resp.Body.Close()
	// Read one byte past the limit to tell an exact fit from an overflow.
	body, err := io.ReadAll(io.LimitReader(resp.Body, c.maxBytes+1))
	if err != nil {
		return nil, err
	}
	if int64(len(body)) > c.maxBytes {
		return nil, fmt.Errorf("%w: %d bytes", ErrResponseTooLarge, c.maxBytes)
	}
	resp.Body = io.NopCloser(bytes.NewReader(body))
	return resp, nil
}

func (c *limitedHTTPClient) CloseIdleConnections() {
	c.inner.CloseIdleConnections()
}

const redactedEndpointPart = "<redacted>"

// endpointRedactor removes the path, query, userinfo and fragment of the endpoints from
// error text. Providers put API keys there. It matches the parts because a websocket dial
// error reports the http scheme.
type endpointRedactor struct {
	secrets []string
}

func newEndpointRedactor(endpoints ...string) endpointRedactor {
	var r endpointRedactor
	for _, endpoint := range endpoints {
		u, err := url.Parse(endpoint)
		if err != nil {
			// An endpoint that does not parse is redacted whole.
			r.secrets = append(r.secrets, endpoint)
			continue
		}
		for _, part := range []string{u.EscapedPath(), u.Path, u.RawQuery, u.Fragment} {
			if len(part) > 1 {
				r.secrets = append(r.secrets, part)
			}
		}
		if u.User != nil {
			r.secrets = append(r.secrets, u.User.String())
		}
	}
	return r
}

// redact returns err with each endpoint secret replaced. errors.Is and errors.As still
// reach err.
func (r endpointRedactor) redact(err error) error {
	if err == nil {
		return nil
	}
	msg := err.Error()
	clean := msg
	for _, secret := range r.secrets {
		clean = strings.ReplaceAll(clean, secret, redactedEndpointPart)
	}
	if clean == msg {
		return err
	}
	return &redactedError{msg: clean, err: err}
}

type redactedError struct {
	msg string
	err error
}

func (e *redactedError) Error() string { return e.msg }
func (e *redactedError) Unwrap() error { return e.err }

// redactingRPCClient redacts endpoint secrets from every error. solana-go writes the
// endpoint URL into its error text.
type redactingRPCClient struct {
	inner    jsonrpc.RPCClient
	redactor endpointRedactor
}

func (c *redactingRPCClient) CallForInto(ctx context.Context, out any, method string, params []any) error {
	return c.redactor.redact(c.inner.CallForInto(ctx, out, method, params))
}

func (c *redactingRPCClient) CallWithCallback(ctx context.Context, method string, params []any, callback func(*http.Request, *http.Response) error) error {
	return c.redactor.redact(c.inner.CallWithCallback(ctx, method, params, callback))
}

func (c *redactingRPCClient) CallBatch(ctx context.Context, requests jsonrpc.RPCRequests) (jsonrpc.RPCResponses, error) {
	res, err := c.inner.CallBatch(ctx, requests)
	return res, c.redactor.redact(err)
}

func (c *redactingRPCClient) Close() error {
	if closer, ok := c.inner.(io.Closer); ok {
		return closer.Close()
	}
	return nil
}
