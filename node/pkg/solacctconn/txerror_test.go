package solacctconn

import (
	"encoding/json"
	"errors"
	"fmt"
	"strings"
	"testing"

	"github.com/gagliardetto/solana-go/rpc/jsonrpc"
	"github.com/stretchr/testify/assert"
	"github.com/stretchr/testify/require"
)

// decodeWithNumbers mirrors the RPC client's preflight decoding, which keeps numbers as
// json.Number.
func decodeWithNumbers(t *testing.T, raw string) any {
	t.Helper()
	dec := json.NewDecoder(strings.NewReader(raw))
	dec.UseNumber()
	var out any
	require.NoError(t, dec.Decode(&out))
	return out
}

// decodePlain mirrors the decoding of a signature status, where numbers become float64.
func decodePlain(t *testing.T, raw string) any {
	t.Helper()
	var out any
	require.NoError(t, json.Unmarshal([]byte(raw), &out))
	return out
}

// preflightError builds the error the RPC client returns when preflight rejects a send.
func preflightError(t *testing.T, dataJSON string) error {
	t.Helper()
	return &jsonrpc.RPCError{
		Code:    -32002,
		Message: "Transaction simulation failed: Error processing Instruction 2: custom program error: 0xb",
		Data:    decodeWithNumbers(t, dataJSON),
	}
}

const preflightCustomData = `{
  "accounts": null,
  "err": {"InstructionError": [2, {"Custom": 11}]},
  "logs": ["Program 11111111111111111111111111111111 invoke [1]", "Program log: AlreadySigned"],
  "unitsConsumed": 4321
}`

func TestParseTxError(t *testing.T) {
	custom := func(code uint32) *TxError { return &TxError{Kind: TxErrOther, CustomCode: code, HasCustomCode: true} }
	kind := func(k TxErrKind) *TxError { return &TxError{Kind: k} }

	tests := []struct {
		name  string
		value func(t *testing.T) any
		want  *TxError
	}{
		{name: "nil", value: func(t *testing.T) any { return nil }},
		{
			name:  "preflight data",
			value: func(t *testing.T) any { return decodeWithNumbers(t, preflightCustomData).(map[string]any)["err"] },
			want:  custom(11),
		},
		{
			name:  "signature status error",
			value: func(t *testing.T) any { return decodePlain(t, `{"InstructionError": [2, {"Custom": 7}]}`) },
			want:  custom(7),
		},
		{
			name:  "code zero",
			value: func(t *testing.T) any { return decodePlain(t, `{"InstructionError": [0, {"Custom": 0}]}`) },
			want:  custom(0),
		},
		{name: "blockhash not found as a string", value: func(t *testing.T) any { return "BlockhashNotFound" }, want: kind(TxErrBlockhashNotFound)},
		{
			name:  "blockhash not found as an object",
			value: func(t *testing.T) any { return decodePlain(t, `{"BlockhashNotFound": null}`) },
			want:  kind(TxErrBlockhashNotFound),
		},
		{name: "already processed", value: func(t *testing.T) any { return "AlreadyProcessed" }, want: kind(TxErrAlreadyProcessed)},
		{
			name:  "fee payer account not found",
			value: func(t *testing.T) any { return decodePlain(t, `{"AccountNotFound": null}`) },
			want:  kind(TxErrAccountNotFound),
		},
		{name: "insufficient funds for fee", value: func(t *testing.T) any { return "InsufficientFundsForFee" }, want: kind(TxErrInsufficientFundsForFee)},
		{name: "unknown unit variant", value: func(t *testing.T) any { return "AccountInUse" }, want: kind(TxErrOther)},
		{
			name:  "object with two keys",
			value: func(t *testing.T) any { return decodePlain(t, `{"BlockhashNotFound": null, "AlreadyProcessed": null}`) },
			want:  kind(TxErrOther),
		},
		{
			name:  "instruction error without a custom code",
			value: func(t *testing.T) any { return decodePlain(t, `{"InstructionError": [0, "InvalidAccountData"]}`) },
			want:  kind(TxErrOther),
		},
		{
			name:  "instruction error tuple too short",
			value: func(t *testing.T) any { return decodePlain(t, `{"InstructionError": [0]}`) },
			want:  kind(TxErrOther),
		},
		{
			name:  "custom code at the top level",
			value: func(t *testing.T) any { return decodePlain(t, `{"Custom": 5}`) },
			want:  kind(TxErrOther),
		},
		{
			name:  "negative custom code",
			value: func(t *testing.T) any { return decodePlain(t, `{"InstructionError": [0, {"Custom": -1}]}`) },
			want:  kind(TxErrOther),
		},
		{
			name:  "custom code past uint32",
			value: func(t *testing.T) any { return decodePlain(t, `{"InstructionError": [0, {"Custom": 4294967296}]}`) },
			want:  kind(TxErrOther),
		},
		{
			name:  "fractional custom code",
			value: func(t *testing.T) any { return decodePlain(t, `{"InstructionError": [0, {"Custom": 1.5}]}`) },
			want:  kind(TxErrOther),
		},
	}

	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			got := parseTxError(tt.value(t))
			if tt.want == nil {
				assert.Nil(t, got)
				return
			}
			require.NotNil(t, got)
			assert.Equal(t, tt.want.Kind, got.Kind)
			assert.Equal(t, tt.want.HasCustomCode, got.HasCustomCode)
			assert.Equal(t, tt.want.CustomCode, got.CustomCode)
		})
	}
}

func TestPreflightTxError(t *testing.T) {
	tests := []struct {
		name string
		err  func(t *testing.T) error
		want *TxError
	}{
		{name: "custom program error", err: func(t *testing.T) error { return preflightError(t, preflightCustomData) }, want: &TxError{CustomCode: 11, HasCustomCode: true}},
		{
			name: "wrapped",
			err:  func(t *testing.T) error { return fmt.Errorf("send: %w", preflightError(t, preflightCustomData)) },
			want: &TxError{CustomCode: 11, HasCustomCode: true},
		},
		{
			name: "blockhash not found",
			err:  func(t *testing.T) error { return preflightError(t, `{"err": "BlockhashNotFound", "logs": []}`) },
			want: &TxError{Kind: TxErrBlockhashNotFound},
		},
		{name: "no err key", err: func(t *testing.T) error { return preflightError(t, `{"logs": []}`) }},
		{name: "no data", err: func(t *testing.T) error { return &jsonrpc.RPCError{Code: -32002} }},
		{name: "transport error", err: func(t *testing.T) error { return errors.New("connection reset") }},
	}

	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			got := preflightTxError(tt.err(t))
			if tt.want == nil {
				assert.Nil(t, got)
				return
			}
			require.NotNil(t, got)
			assert.Equal(t, tt.want.Kind, got.Kind)
			assert.Equal(t, tt.want.HasCustomCode, got.HasCustomCode)
			assert.Equal(t, tt.want.CustomCode, got.CustomCode)
		})
	}
}
