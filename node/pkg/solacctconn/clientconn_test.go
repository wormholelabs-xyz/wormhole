package solacctconn

import (
	"context"
	"net/http"
	"net/http/httptest"
	"testing"
	"time"

	"github.com/gagliardetto/solana-go"
	"github.com/stretchr/testify/assert"
	"github.com/stretchr/testify/require"
)

func TestNewConnRequiresBothEndpoints(t *testing.T) {
	tests := []struct {
		name    string
		rpcURL  string
		wsURL   string
		wantErr bool
	}{
		{name: "both set", rpcURL: "http://127.0.0.1:8899", wsURL: "ws://127.0.0.1:8900"},
		{name: "rpc missing", wsURL: "ws://127.0.0.1:8900", wantErr: true},
		{name: "websocket missing", rpcURL: "http://127.0.0.1:8899", wantErr: true},
	}

	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			conn, err := NewConn(tt.rpcURL, tt.wsURL)
			if tt.wantErr {
				require.Error(t, err)
				assert.Nil(t, conn)
				return
			}
			require.NoError(t, err)
			require.NotNil(t, conn)
			conn.Close()
		})
	}
}

// Providers put API keys in the endpoint path or query.
func TestConnErrorsOmitEndpointSecrets(t *testing.T) {
	const secret = "SECRETKEY"
	srv := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		http.Error(w, "not json", http.StatusInternalServerError)
	}))
	t.Cleanup(srv.Close)

	tests := []struct {
		name string
		rpc  string
		ws   string
		call func(ctx context.Context, conn *ClientConn) error
	}{
		{
			name: "rpc status error, key in query",
			rpc:  srv.URL + "/?api-key=" + secret,
			ws:   "ws://127.0.0.1:1",
			call: func(ctx context.Context, conn *ClientConn) error {
				_, err := conn.GetBalance(ctx, solana.PublicKey{1})
				return err
			},
		},
		{
			name: "rpc transport error, key in path",
			rpc:  "http://127.0.0.1:1/" + secret,
			ws:   "ws://127.0.0.1:1",
			call: func(ctx context.Context, conn *ClientConn) error {
				_, err := conn.GetBalance(ctx, solana.PublicKey{1})
				return err
			},
		},
		{
			name: "websocket dial error, key in query",
			rpc:  "http://127.0.0.1:1",
			ws:   "ws://127.0.0.1:1/?api-key=" + secret,
			call: func(ctx context.Context, conn *ClientConn) error {
				_, err := conn.SubscribeLogs(ctx, solana.PublicKey{1})
				return err
			},
		},
	}
	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			conn, err := NewConn(tt.rpc, tt.ws)
			require.NoError(t, err)
			t.Cleanup(conn.Close)
			ctx, cancel := context.WithTimeout(context.Background(), 10*time.Second)
			defer cancel()

			err = tt.call(ctx, conn)
			require.Error(t, err)
			assert.NotContains(t, err.Error(), secret)
		})
	}
}
