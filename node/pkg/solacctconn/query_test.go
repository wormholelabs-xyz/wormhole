package solacctconn

import (
	"context"
	"encoding/base64"
	"encoding/json"
	"testing"

	"github.com/gagliardetto/solana-go"
	"github.com/gagliardetto/solana-go/rpc/jsonrpc"
	"github.com/stretchr/testify/assert"
	"github.com/stretchr/testify/require"
)

func testKeys(n int) []solana.PublicKey {
	keys := make([]solana.PublicKey, n)
	for i := range keys {
		keys[i][0] = byte(i % 256)
		keys[i][1] = byte(i / 256)
		// Keep every key distinct and non-zero.
		keys[i][31] = 0x01
	}
	return keys
}

func accountValue(owner solana.PublicKey, data []byte) map[string]any {
	return map[string]any{
		"lamports":   1,
		"owner":      owner.String(),
		"data":       []string{base64.StdEncoding.EncodeToString(data), "base64"},
		"executable": false,
		"rentEpoch":  0,
		"space":      len(data),
	}
}

func TestGetMultipleAccountsChunking(t *testing.T) {
	tests := []struct {
		name      string
		count     int
		wantCalls []int
	}{
		{name: "no keys makes no call", count: 0, wantCalls: nil},
		{name: "one key", count: 1, wantCalls: []int{1}},
		{name: "exactly one chunk", count: 100, wantCalls: []int{100}},
		{name: "one past a chunk", count: 101, wantCalls: []int{100, 1}},
		{name: "two and a half chunks", count: 250, wantCalls: []int{100, 100, 50}},
	}

	owner := testKeys(1)[0]
	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			srv, conn := newTestRPC(t, func(t *testing.T, call rpcCall) (any, *jsonrpc.RPCError) {
				require.Equal(t, "getMultipleAccounts", call.Method)
				keys := accountKeys(t, call)
				values := make([]any, 0, len(keys))
				for range keys {
					values = append(values, accountValue(owner, []byte{0xaa}))
				}
				return map[string]any{"context": map[string]any{"slot": 1}, "value": values}, nil
			})

			results, err := conn.GetMultipleAccounts(context.Background(), testKeys(tt.count))
			require.NoError(t, err)
			require.Len(t, results, tt.count)

			calls := srv.recorded()
			require.Len(t, calls, len(tt.wantCalls))
			for i, want := range tt.wantCalls {
				assert.Len(t, accountKeys(t, calls[i]), want)
			}
		})
	}
}

func TestGetMultipleAccountsResults(t *testing.T) {
	owner := testKeys(1)[0]

	tests := []struct {
		name    string
		reply   func(keys []string) any
		wantErr bool
	}{
		{
			name: "absent accounts are positional nils",
			reply: func(keys []string) any {
				return map[string]any{"context": map[string]any{"slot": 1}, "value": []any{
					accountValue(owner, []byte{1}), nil, accountValue(owner, []byte{3}),
				}}
			},
		},
		{
			name: "short result is rejected",
			reply: func(keys []string) any {
				return map[string]any{"context": map[string]any{"slot": 1}, "value": []any{accountValue(owner, []byte{1})}}
			},
			wantErr: true,
		},
		{
			name: "long result is rejected",
			reply: func(keys []string) any {
				return map[string]any{"context": map[string]any{"slot": 1}, "value": []any{
					accountValue(owner, []byte{1}), nil, nil, nil,
				}}
			},
			wantErr: true,
		},
	}

	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			_, conn := newTestRPC(t, func(t *testing.T, call rpcCall) (any, *jsonrpc.RPCError) {
				return tt.reply(accountKeys(t, call)), nil
			})

			results, err := conn.GetMultipleAccounts(context.Background(), testKeys(3))
			if tt.wantErr {
				require.Error(t, err)
				return
			}
			require.NoError(t, err)
			require.Len(t, results, 3)
			require.NotNil(t, results[0])
			assert.Equal(t, []byte{1}, results[0].Data)
			assert.Nil(t, results[1])
			require.NotNil(t, results[2])
			assert.Equal(t, []byte{3}, results[2].Data)
		})
	}
}

func TestGetMultipleAccountsRPCError(t *testing.T) {
	_, conn := newTestRPC(t, func(t *testing.T, call rpcCall) (any, *jsonrpc.RPCError) {
		return nil, &jsonrpc.RPCError{Code: -32000, Message: "node is behind"}
	})

	_, err := conn.GetMultipleAccounts(context.Background(), testKeys(1))
	require.Error(t, err)
	assert.Contains(t, err.Error(), "getMultipleAccounts")
}

func TestGetProgramAccountsByTag(t *testing.T) {
	program := testKeys(1)[0]
	pda := testKeys(2)[1]

	tests := []struct {
		name    string
		data    []byte
		owner   solana.PublicKey
		wantErr bool
	}{
		{name: "matching account", data: make([]byte, 88), owner: program},
		{name: "wrong data size", data: make([]byte, 87), owner: program, wantErr: true},
		{name: "wrong owner", data: make([]byte, 88), owner: pda, wantErr: true},
	}

	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			srv, conn := newTestRPC(t, func(t *testing.T, call rpcCall) (any, *jsonrpc.RPCError) {
				require.Equal(t, "getProgramAccounts", call.Method)
				return []any{map[string]any{"pubkey": pda.String(), "account": accountValue(tt.owner, tt.data)}}, nil
			})

			accounts, err := conn.GetProgramAccountsByTag(context.Background(), program, 1, 88)
			if tt.wantErr {
				require.Error(t, err)
				return
			}
			require.NoError(t, err)
			require.Len(t, accounts, 1)
			assert.Equal(t, pda, accounts[0].Address)
			assert.Len(t, accounts[0].Data, 88)

			var opts struct {
				Commitment  string `json:"commitment"`
				Encoding    string `json:"encoding"`
				SortResults bool   `json:"sortResults"`
				Filters     []struct {
					DataSize uint64 `json:"dataSize"`
					Memcmp   *struct {
						Offset uint64 `json:"offset"`
						Bytes  string `json:"bytes"`
					} `json:"memcmp"`
				} `json:"filters"`
			}
			calls := srv.recorded()
			require.Len(t, calls, 1)
			require.NoError(t, json.Unmarshal(calls[0].Params[1], &opts))
			assert.Equal(t, "finalized", opts.Commitment)
			assert.Equal(t, "base64", opts.Encoding)
			assert.True(t, opts.SortResults)
			require.Len(t, opts.Filters, 2)
			require.NotNil(t, opts.Filters[0].Memcmp)
			assert.Equal(t, uint64(0), opts.Filters[0].Memcmp.Offset)
			assert.Equal(t, solana.Base58{1}.String(), opts.Filters[0].Memcmp.Bytes)
			assert.Equal(t, uint64(88), opts.Filters[1].DataSize)
		})
	}
}

func TestGetSignaturesForAddress(t *testing.T) {
	addr := testKeys(1)[0]
	sig := solana.Signature{1, 2, 3}

	t.Run("limit is bounded", func(t *testing.T) {
		_, conn := newTestRPC(t, func(t *testing.T, call rpcCall) (any, *jsonrpc.RPCError) {
			t.Fatal("no request expected")
			return nil, nil
		})
		_, err := conn.GetSignaturesForAddress(context.Background(), addr, 0)
		require.Error(t, err)
		_, err = conn.GetSignaturesForAddress(context.Background(), addr, maxSignaturesPerRequest+1)
		require.Error(t, err)
	})

	t.Run("returns signatures newest first", func(t *testing.T) {
		_, conn := newTestRPC(t, func(t *testing.T, call rpcCall) (any, *jsonrpc.RPCError) {
			require.Equal(t, "getSignaturesForAddress", call.Method)
			return []any{map[string]any{"signature": sig.String(), "slot": 7}}, nil
		})
		sigs, err := conn.GetSignaturesForAddress(context.Background(), addr, 10)
		require.NoError(t, err)
		require.Len(t, sigs, 1)
		assert.Equal(t, sig, sigs[0])
	})

	t.Run("more results than the limit is rejected", func(t *testing.T) {
		_, conn := newTestRPC(t, func(t *testing.T, call rpcCall) (any, *jsonrpc.RPCError) {
			return []any{
				map[string]any{"signature": sig.String(), "slot": 7},
				map[string]any{"signature": sig.String(), "slot": 8},
			}, nil
		})
		_, err := conn.GetSignaturesForAddress(context.Background(), addr, 1)
		require.Error(t, err)
	})
}

// encodedTransaction builds a signed-shaped transaction and its base64 wire form.
func encodedTransaction(t *testing.T, program solana.PublicKey, data []byte) string {
	t.Helper()
	payer := testKeys(1)[0]
	tx := solana.Transaction{
		Signatures: []solana.Signature{{9}},
		Message: solana.Message{
			AccountKeys:     []solana.PublicKey{payer, program},
			RecentBlockhash: solana.Hash{7},
			Instructions: []solana.CompiledInstruction{
				{ProgramIDIndex: 1, Accounts: []uint16{0}, Data: data},
			},
		},
	}
	tx.Message.Header.NumRequiredSignatures = 1
	raw, err := tx.MarshalBinary()
	require.NoError(t, err)
	return base64.StdEncoding.EncodeToString(raw)
}

func TestGetTransaction(t *testing.T) {
	program := testKeys(2)[1]
	sig := solana.Signature{5}
	encoded := encodedTransaction(t, program, []byte{0x00, 0x01})

	manyLogs := make([]any, MaxLogLinesPerTx+1)
	for i := range manyLogs {
		manyLogs[i] = "Program log: noise"
	}

	tests := []struct {
		name       string
		meta       any
		wantErr    bool
		wantFailed bool
	}{
		{name: "succeeded", meta: map[string]any{"err": nil, "logMessages": []any{"Program log: hi"}}},
		{name: "failed", meta: map[string]any{"err": "AlreadyProcessed", "logMessages": []any{}}, wantFailed: true},
		{name: "no metadata", meta: nil, wantErr: true},
		{name: "too many log lines", meta: map[string]any{"err": nil, "logMessages": manyLogs}, wantErr: true},
	}

	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			_, conn := newTestRPC(t, func(t *testing.T, call rpcCall) (any, *jsonrpc.RPCError) {
				require.Equal(t, "getTransaction", call.Method)
				out := map[string]any{"slot": 11, "transaction": []string{encoded, "base64"}}
				if tt.meta != nil {
					out["meta"] = tt.meta
				}
				return out, nil
			})

			res, err := conn.GetTransaction(context.Background(), sig)
			if tt.wantErr {
				require.Error(t, err)
				return
			}
			require.NoError(t, err)
			assert.Equal(t, tt.wantFailed, res.Failed)
			require.Len(t, res.Instructions, 1)
			assert.Equal(t, program, res.Instructions[0].ProgramID)
			assert.Equal(t, []byte{0x00, 0x01}, res.Instructions[0].Data)
		})
	}
}
