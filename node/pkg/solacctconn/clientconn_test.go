package solacctconn

import (
	"testing"

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
		{name: "both missing", wantErr: true},
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
