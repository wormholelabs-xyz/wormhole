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
		// No key can be the zero address. The keys must be different from each other.
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

// overLogLimit is one log line past MaxLogLinesPerTx.
func overLogLimit() []string {
	logs := make([]string, MaxLogLinesPerTx+1)
	for i := range logs {
		logs[i] = "Program log: noise"
	}
	return logs
}

// testTransaction builds a signed-shaped transaction with one instruction to program.
func testTransaction(program solana.PublicKey, data []byte) *solana.Transaction {
	tx := &solana.Transaction{
		Signatures: []solana.Signature{{9}},
		Message: solana.Message{
			AccountKeys:     []solana.PublicKey{testKeys(1)[0], program},
			RecentBlockhash: solana.Hash{7},
			Instructions: []solana.CompiledInstruction{
				{ProgramIDIndex: 1, Accounts: []uint16{0}, Data: data},
			},
		},
	}
	tx.Message.Header.NumRequiredSignatures = 1
	return tx
}

func TestGetOwnedAccountsChunking(t *testing.T) {
	tests := []struct {
		name      string
		count     int
		wantCalls []int
	}{
		{name: "exactly one chunk", count: 100, wantCalls: []int{100}},
		{name: "one past a chunk", count: 101, wantCalls: []int{100, 1}},
		{name: "two and a half chunks", count: 250, wantCalls: []int{100, 100, 50}},
	}

	owner := testKeys(1)[0]
	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			srv, conn := newTestRPC(t, func(call rpcCall) (any, *jsonrpc.RPCError) {
				var keys []string
				_ = json.Unmarshal(call.Params[0], &keys)
				values := make([]any, 0, len(keys))
				for range keys {
					values = append(values, accountValue(owner, []byte{0xaa}))
				}
				return map[string]any{"context": map[string]any{"slot": 1}, "value": values}, nil
			})

			results, err := conn.GetOwnedAccounts(context.Background(), testKeys(tt.count), owner, CommitmentFinalized)
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

func TestGetOwnedAccountsClassifiesOwner(t *testing.T) {
	owner := testKeys(1)[0]
	third := testKeys(3)[2]
	pending := make([]byte, 88)
	pending[0] = 0x07
	prefunded := accountValue(solana.SystemProgramID, nil)
	prefunded["lamports"] = 650_240
	executable := accountValue(owner, pending)
	executable["executable"] = true

	tests := []struct {
		name    string
		value   any
		want    OwnedAccount
		wantErr bool
	}{
		{name: "absent", value: nil, want: OwnedAccount{State: AccountAbsent}},
		{name: "prefunded system account is uninitialised", value: prefunded, want: OwnedAccount{State: AccountUninitialised}},
		{name: "owned account is initialised", value: accountValue(owner, pending), want: OwnedAccount{State: AccountInitialised, Data: pending}},
		{name: "system account with data", value: accountValue(solana.SystemProgramID, pending), wantErr: true},
		{name: "third-party owner", value: accountValue(third, pending), wantErr: true},
		{name: "executable owned account", value: executable, wantErr: true},
	}

	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			_, conn := newTestRPC(t, func(call rpcCall) (any, *jsonrpc.RPCError) {
				return map[string]any{"context": map[string]any{"slot": 1}, "value": []any{tt.value}}, nil
			})

			results, err := conn.GetOwnedAccounts(context.Background(), testKeys(1), owner, CommitmentFinalized)
			if tt.wantErr {
				require.Error(t, err)
				assert.Nil(t, results)
				return
			}
			require.NoError(t, err)
			require.Len(t, results, 1)
			assert.Equal(t, tt.want, results[0])
		})
	}
}

func TestGetOwnedAccountsRejects(t *testing.T) {
	owner := testKeys(1)[0]
	tests := []struct {
		name  string
		owner solana.PublicKey
		value []any
	}{
		{name: "short result", owner: owner, value: []any{accountValue(owner, []byte{1})}},
		{name: "system program as owner", owner: solana.SystemProgramID, value: []any{nil, nil, nil}},
	}

	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			_, conn := newTestRPC(t, func(call rpcCall) (any, *jsonrpc.RPCError) {
				return map[string]any{"context": map[string]any{"slot": 1}, "value": tt.value}, nil
			})

			results, err := conn.GetOwnedAccounts(context.Background(), testKeys(3), tt.owner, CommitmentFinalized)
			require.Error(t, err)
			assert.Nil(t, results)
		})
	}
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
			srv, conn := newTestRPC(t, func(call rpcCall) (any, *jsonrpc.RPCError) {
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
				Filters []struct {
					DataSize uint64 `json:"dataSize"`
					Memcmp   *struct {
						Bytes string `json:"bytes"`
					} `json:"memcmp"`
				} `json:"filters"`
			}
			calls := srv.recorded()
			require.Len(t, calls, 1)
			require.NoError(t, json.Unmarshal(calls[0].Params[1], &opts))
			require.Len(t, opts.Filters, 2)
			require.NotNil(t, opts.Filters[0].Memcmp)
			assert.Equal(t, solana.Base58{1}.String(), opts.Filters[0].Memcmp.Bytes)
			assert.Equal(t, uint64(88), opts.Filters[1].DataSize)
		})
	}
}

func TestGetSignaturesForAddress(t *testing.T) {
	addr := testKeys(1)[0]
	sig := solana.Signature{1, 2, 3}

	t.Run("limit is bounded", func(t *testing.T) {
		srv, conn := newTestRPC(t, func(call rpcCall) (any, *jsonrpc.RPCError) { return nil, nil })
		_, err := conn.GetSignaturesForAddress(context.Background(), addr, 0)
		require.Error(t, err)
		_, err = conn.GetSignaturesForAddress(context.Background(), addr, maxSignaturesPerRequest+1)
		require.Error(t, err)
		assert.Empty(t, srv.recorded())
	})

	t.Run("returns signatures newest first", func(t *testing.T) {
		_, conn := newTestRPC(t, func(call rpcCall) (any, *jsonrpc.RPCError) {
			return []any{map[string]any{"signature": sig.String(), "slot": 7}}, nil
		})
		sigs, err := conn.GetSignaturesForAddress(context.Background(), addr, 10)
		require.NoError(t, err)
		require.Len(t, sigs, 1)
		assert.Equal(t, sig, sigs[0])
	})

	t.Run("more results than the limit is rejected", func(t *testing.T) {
		_, conn := newTestRPC(t, func(call rpcCall) (any, *jsonrpc.RPCError) {
			return []any{
				map[string]any{"signature": sig.String(), "slot": 7},
				map[string]any{"signature": sig.String(), "slot": 8},
			}, nil
		})
		_, err := conn.GetSignaturesForAddress(context.Background(), addr, 1)
		require.Error(t, err)
	})
}

// encodedTransaction is the base64 wire form of testTransaction.
func encodedTransaction(t *testing.T, program solana.PublicKey, data []byte) string {
	t.Helper()
	raw, err := testTransaction(program, data).MarshalBinary()
	require.NoError(t, err)
	return base64.StdEncoding.EncodeToString(raw)
}

func TestGetTransaction(t *testing.T) {
	program := testKeys(2)[1]
	sig := solana.Signature{5}
	encoded := encodedTransaction(t, program, []byte{0x00, 0x01})

	tests := []struct {
		name       string
		meta       any
		wantErr    bool
		wantFailed bool
	}{
		{name: "succeeded", meta: map[string]any{"err": nil, "logMessages": []any{"Program log: hi"}}},
		{name: "failed", meta: map[string]any{"err": "AlreadyProcessed", "logMessages": []any{}}, wantFailed: true},
		{name: "no metadata", meta: nil, wantErr: true},
		{name: "too many log lines", meta: map[string]any{"err": nil, "logMessages": overLogLimit()}, wantErr: true},
	}

	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			_, conn := newTestRPC(t, func(call rpcCall) (any, *jsonrpc.RPCError) {
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

func TestGetOwnedAccountsCommitment(t *testing.T) {
	tests := []struct {
		name       string
		commitment Commitment
		want       string
		wantErr    bool
	}{
		{name: "confirmed", commitment: CommitmentConfirmed, want: "confirmed"},
		{name: "zero value is rejected", commitment: Commitment{}, wantErr: true},
	}

	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			srv, conn := newTestRPC(t, func(call rpcCall) (any, *jsonrpc.RPCError) {
				return map[string]any{"context": map[string]any{"slot": 1}, "value": []any{nil}}, nil
			})

			_, err := conn.GetOwnedAccounts(context.Background(), testKeys(1), testKeys(2)[1], tt.commitment)
			if tt.wantErr {
				require.Error(t, err)
				assert.Empty(t, srv.recorded())
				return
			}
			require.NoError(t, err)

			var opts struct {
				Commitment string `json:"commitment"`
			}
			calls := srv.recorded()
			require.Len(t, calls, 1)
			require.NoError(t, json.Unmarshal(calls[0].Params[1], &opts))
			assert.Equal(t, tt.want, opts.Commitment)
		})
	}
}
