package solacctconn

import (
	"context"
	"encoding/json"
	"testing"

	"github.com/gagliardetto/solana-go"
	"github.com/gagliardetto/solana-go/rpc/jsonrpc"
	"github.com/stretchr/testify/assert"
	"github.com/stretchr/testify/require"
)

func TestGetLatestBlockhash(t *testing.T) {
	hash := solana.Hash{1, 2, 3}

	tests := []struct {
		name    string
		value   any
		wantErr bool
	}{
		{name: "blockhash and height", value: map[string]any{"blockhash": hash.String(), "lastValidBlockHeight": 1234}},
		{name: "zero blockhash", value: map[string]any{"blockhash": solana.Hash{}.String(), "lastValidBlockHeight": 1234}, wantErr: true},
		{name: "zero height", value: map[string]any{"blockhash": hash.String(), "lastValidBlockHeight": 0}, wantErr: true},
		{name: "empty value", value: nil, wantErr: true},
	}

	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			_, conn := newTestRPC(t, func(call rpcCall) (any, *jsonrpc.RPCError) {
				return map[string]any{"context": map[string]any{"slot": 1}, "value": tt.value}, nil
			})

			got, err := conn.GetLatestBlockhash(context.Background())
			if tt.wantErr {
				require.Error(t, err)
				return
			}
			require.NoError(t, err)
			assert.Equal(t, hash, got.Hash)
			assert.Equal(t, uint64(1234), got.LastValidBlockHeight)
		})
	}
}

func TestSendTransaction(t *testing.T) {
	tx := testTransaction(testKeys(2)[1], []byte{0})
	sig := tx.Signatures[0]

	t.Run("returns the cluster signature", func(t *testing.T) {
		_, conn := newTestRPC(t, func(call rpcCall) (any, *jsonrpc.RPCError) {
			return sig.String(), nil
		})
		got, err := conn.SendTransaction(context.Background(), tx)
		require.NoError(t, err)
		assert.Equal(t, sig, got)
	})

	t.Run("rejects a different signature", func(t *testing.T) {
		_, conn := newTestRPC(t, func(call rpcCall) (any, *jsonrpc.RPCError) {
			return solana.Signature{8, 8}.String(), nil
		})
		_, err := conn.SendTransaction(context.Background(), tx)
		require.Error(t, err)
	})

	t.Run("rejects an unsigned transaction", func(t *testing.T) {
		srv, conn := newTestRPC(t, func(call rpcCall) (any, *jsonrpc.RPCError) { return nil, nil })
		_, err := conn.SendTransaction(context.Background(), nil)
		require.Error(t, err)
		_, err = conn.SendTransaction(context.Background(), &solana.Transaction{})
		require.Error(t, err)
		assert.Empty(t, srv.recorded())
	})

	t.Run("preflight failure carries the transaction error", func(t *testing.T) {
		_, conn := newTestRPC(t, func(call rpcCall) (any, *jsonrpc.RPCError) {
			return nil, &jsonrpc.RPCError{
				Code:    -32002,
				Message: "Transaction simulation failed: Error processing Instruction 2: custom program error: 0xb",
				Data:    json.RawMessage(preflightCustomData),
			}
		})

		_, err := conn.SendTransaction(context.Background(), tx)
		require.Error(t, err)
		var txErr *TxError
		require.ErrorAs(t, err, &txErr)
		assert.True(t, txErr.HasCustomCode)
		assert.Equal(t, uint32(11), txErr.CustomCode)
	})
}

func TestGetSignatureStatuses(t *testing.T) {
	sigs := []solana.Signature{{1}, {2}, {3}}

	t.Run("no signatures makes no call", func(t *testing.T) {
		srv, conn := newTestRPC(t, func(call rpcCall) (any, *jsonrpc.RPCError) { return nil, nil })
		statuses, err := conn.GetSignatureStatuses(context.Background(), nil)
		require.NoError(t, err)
		assert.Empty(t, statuses)
		assert.Empty(t, srv.recorded())
	})

	t.Run("past the request limit", func(t *testing.T) {
		srv, conn := newTestRPC(t, func(call rpcCall) (any, *jsonrpc.RPCError) { return nil, nil })
		_, err := conn.GetSignatureStatuses(context.Background(), make([]solana.Signature, maxStatusesPerRequest+1))
		require.Error(t, err)
		assert.Empty(t, srv.recorded())
	})

	t.Run("unknown signature is a nil element", func(t *testing.T) {
		_, conn := newTestRPC(t, func(call rpcCall) (any, *jsonrpc.RPCError) {
			return map[string]any{"context": map[string]any{"slot": 1}, "value": []any{
				map[string]any{"slot": 1, "confirmations": nil, "err": nil, "confirmationStatus": "finalized"},
				nil,
				map[string]any{"slot": 2, "confirmations": 3, "err": map[string]any{"InstructionError": []any{2, map[string]any{"Custom": 7}}}, "confirmationStatus": "processed"},
			}}, nil
		})

		statuses, err := conn.GetSignatureStatuses(context.Background(), sigs)
		require.NoError(t, err)
		require.Len(t, statuses, 3)

		require.NotNil(t, statuses[0])
		assert.True(t, statuses[0].Confirmed)
		assert.Nil(t, statuses[0].Err)

		assert.Nil(t, statuses[1])

		require.NotNil(t, statuses[2])
		assert.False(t, statuses[2].Confirmed)
		require.NotNil(t, statuses[2].Err)
		assert.True(t, statuses[2].Err.HasCustomCode)
		assert.Equal(t, uint32(7), statuses[2].Err.CustomCode)
	})

	t.Run("length mismatch is rejected", func(t *testing.T) {
		_, conn := newTestRPC(t, func(call rpcCall) (any, *jsonrpc.RPCError) {
			return map[string]any{"context": map[string]any{"slot": 1}, "value": []any{nil}}, nil
		})
		_, err := conn.GetSignatureStatuses(context.Background(), sigs)
		require.Error(t, err)
	})
}
