package solacctconn

import (
	"encoding/json"
	"net/http"
	"net/http/httptest"
	"sync"
	"testing"

	"github.com/gagliardetto/solana-go/rpc/jsonrpc"
	"github.com/stretchr/testify/require"
)

// rpcCall is one decoded JSON-RPC request.
type rpcCall struct {
	Method string            `json:"method"`
	Params []json.RawMessage `json:"params"`
	ID     any               `json:"id"`
}

// testRPC is a JSON-RPC server that records every call and answers from handler.
type testRPC struct {
	server  *httptest.Server
	handler func(t *testing.T, call rpcCall) (any, *jsonrpc.RPCError)

	mu    sync.Mutex
	calls []rpcCall
}

func newTestRPC(t *testing.T, handler func(t *testing.T, call rpcCall) (any, *jsonrpc.RPCError)) (*testRPC, *ClientConn) {
	t.Helper()

	srv := &testRPC{handler: handler}
	srv.server = httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		var call rpcCall
		require.NoError(t, json.NewDecoder(r.Body).Decode(&call))

		srv.mu.Lock()
		srv.calls = append(srv.calls, call)
		srv.mu.Unlock()

		result, fault := srv.handler(t, call)
		body := map[string]any{"jsonrpc": "2.0", "id": call.ID}
		if fault != nil {
			body["error"] = fault
		} else {
			body["result"] = result
		}
		w.Header().Set("Content-Type", "application/json")
		require.NoError(t, json.NewEncoder(w).Encode(body))
	}))
	t.Cleanup(srv.server.Close)

	conn, err := NewConn(srv.server.URL, "ws://127.0.0.1:1")
	require.NoError(t, err)
	t.Cleanup(conn.Close)
	return srv, conn
}

func (s *testRPC) recorded() []rpcCall {
	s.mu.Lock()
	defer s.mu.Unlock()
	out := make([]rpcCall, len(s.calls))
	copy(out, s.calls)
	return out
}

// accountKeys decodes the key array of a getMultipleAccounts call.
func accountKeys(t *testing.T, call rpcCall) []string {
	t.Helper()
	require.NotEmpty(t, call.Params)
	var keys []string
	require.NoError(t, json.Unmarshal(call.Params[0], &keys))
	return keys
}
