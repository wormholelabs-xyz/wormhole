// Classification of Solana transaction errors. Two shapes reach the guardian: the
// TransactionError of a signature status, and the same value under the "err" key of the
// Data field of a preflight *jsonrpc.RPCError.
//
// Unit variants of the Rust TransactionError enum serialize as a bare string; some RPC
// providers wrap them as a single-key object with a null value. Both are accepted.

package solacctconn

import (
	"encoding/json"
	"errors"
	"math"

	"github.com/gagliardetto/solana-go/rpc/jsonrpc"
)

// [instruction index, detail]
const instructionErrorTupleLen = 2

type TxErrKind uint8

const (
	TxErrOther TxErrKind = iota
	TxErrBlockhashNotFound
	TxErrAlreadyProcessed
	TxErrAccountNotFound
	TxErrInsufficientFundsForFee
)

// TxError is a classified TransactionError. HasCustomCode is set for an
// InstructionError::Custom; code 0 is a valid program error.
type TxError struct {
	Kind          TxErrKind
	CustomCode    uint32
	HasCustomCode bool

	detailJSON string
}

func (e *TxError) Error() string {
	return "transaction error: " + e.detailJSON
}

// parseTxError returns nil for a nil value.
func parseTxError(v any) *TxError {
	if v == nil {
		return nil
	}

	detailJSON, err := json.Marshal(v)
	if err != nil {
		detailJSON = []byte("unencodable")
	}
	txErr := &TxError{Kind: TxErrOther, detailJSON: string(detailJSON)}
	if code, ok := customProgramError(v); ok {
		txErr.CustomCode = code
		txErr.HasCustomCode = true
		return txErr
	}

	switch unitVariant(v) {
	case "BlockhashNotFound":
		txErr.Kind = TxErrBlockhashNotFound
	case "AlreadyProcessed":
		txErr.Kind = TxErrAlreadyProcessed
	case "AccountNotFound":
		txErr.Kind = TxErrAccountNotFound
	case "InsufficientFundsForFee":
		txErr.Kind = TxErrInsufficientFundsForFee
	}
	return txErr
}

// preflightTxError returns the TransactionError a preflight *jsonrpc.RPCError carries, or
// nil when err carries none.
func preflightTxError(err error) *TxError {
	var rpcErr *jsonrpc.RPCError
	if !errors.As(err, &rpcErr) || rpcErr == nil {
		return nil
	}
	data, ok := rpcErr.Data.(map[string]any)
	if !ok {
		return nil
	}
	return parseTxError(data["err"])
}

// customProgramError returns the program error code carried by an InstructionError. The
// instruction index is ignored, because the accountant instruction is the only one in the
// transaction that raises a custom code.
func customProgramError(v any) (uint32, bool) {
	inner, ok := v.(map[string]any)
	if !ok {
		return 0, false
	}

	tuple, ok := inner["InstructionError"].([]any)
	if !ok || len(tuple) != instructionErrorTupleLen {
		return 0, false
	}
	detail, ok := tuple[1].(map[string]any)
	if !ok {
		return 0, false
	}
	code, ok := asUint64(detail["Custom"])
	if !ok || code > math.MaxUint32 {
		return 0, false
	}
	return uint32(code), true
}

func unitVariant(v any) string {
	switch t := v.(type) {
	case string:
		return t
	case map[string]any:
		if len(t) != 1 {
			return ""
		}
		for name, value := range t {
			if value == nil {
				return name
			}
		}
	}
	return ""
}

// asUint64 reads a JSON number. The RPC client decodes preflight data with UseNumber and
// signature statuses without it, so both json.Number and float64 occur.
func asUint64(v any) (uint64, bool) {
	switch n := v.(type) {
	case json.Number:
		parsed, err := n.Int64()
		if err != nil || parsed < 0 {
			return 0, false
		}
		return uint64(parsed), true
	case float64:
		if n < 0 || n != math.Trunc(n) || n > math.MaxUint32 {
			return 0, false
		}
		return uint64(n), true
	}
	return 0, false
}
