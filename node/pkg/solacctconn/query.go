package solacctconn

import (
	"context"
	"errors"
	"fmt"

	"github.com/gagliardetto/solana-go"
	"github.com/gagliardetto/solana-go/rpc"
)

const (
	// getMultipleAccounts caps a request at 100 keys.
	maxAccountsPerRequest = 100

	// getSignaturesForAddress caps a request at 1000 signatures.
	maxSignaturesPerRequest = 1000

	// MaxLogLinesPerTx bounds the log lines of one transaction. The agave log buffer is
	// 10 KiB per transaction.
	MaxLogLinesPerTx = 2048
)

// GetMultipleAccounts reads accounts at commitment. Results are positional with addrs and
// a nil element marks an absent account. Requests are chunked at maxAccountsPerRequest.
func (c *ClientConn) GetMultipleAccounts(ctx context.Context, addrs []solana.PublicKey, commitment Commitment) ([]*AccountResult, error) {
	if commitment != CommitmentConfirmed && commitment != CommitmentFinalized {
		return nil, fmt.Errorf("getMultipleAccounts: unsupported commitment %q", commitment)
	}
	out := make([]*AccountResult, 0, len(addrs))

	for start := 0; start < len(addrs); start += maxAccountsPerRequest {
		chunk := addrs[start:min(start+maxAccountsPerRequest, len(addrs))]

		resp, err := c.rpc.GetMultipleAccountsWithOpts(ctx, chunk, &rpc.GetMultipleAccountsOpts{
			Encoding:   solana.EncodingBase64,
			Commitment: commitment.level,
		})
		if err != nil {
			return nil, fmt.Errorf("getMultipleAccounts: %w", err)
		}
		if resp == nil || resp.Value == nil {
			return nil, errors.New("getMultipleAccounts: empty response")
		}
		if len(resp.Value) != len(chunk) {
			return nil, fmt.Errorf("getMultipleAccounts: want %d results, got %d", len(chunk), len(resp.Value))
		}

		for _, account := range resp.Value {
			if account == nil {
				out = append(out, nil)
				continue
			}
			out = append(out, &AccountResult{Data: account.Data.GetBinary()})
		}
	}

	if len(out) != len(addrs) {
		return nil, fmt.Errorf("getMultipleAccounts: want %d results, got %d", len(addrs), len(out))
	}
	return out, nil
}

// GetProgramAccountsByTag reads the accounts of program whose first byte is tag and whose
// length is exactly dataSize, at finalized commitment.
func (c *ClientConn) GetProgramAccountsByTag(ctx context.Context, program solana.PublicKey, tag byte, dataSize uint64) ([]ProgramAccount, error) {
	sortResults := true
	res, err := c.rpc.GetProgramAccountsWithOpts(ctx, program, &rpc.GetProgramAccountsOpts{
		Encoding:   solana.EncodingBase64,
		Commitment: rpc.CommitmentFinalized,
		Filters: []rpc.RPCFilter{
			{Memcmp: &rpc.RPCFilterMemcmp{Offset: 0, Bytes: solana.Base58{tag}}},
			{DataSize: dataSize},
		},
		SortResults: &sortResults,
	})
	if err != nil {
		return nil, fmt.Errorf("getProgramAccounts: %w", err)
	}

	out := make([]ProgramAccount, 0, len(res))
	for _, keyed := range res {
		if keyed == nil || keyed.Account == nil {
			return nil, errors.New("getProgramAccounts: empty account in the result")
		}
		data := keyed.Account.Data.GetBinary()
		if uint64(len(data)) != dataSize {
			return nil, fmt.Errorf("getProgramAccounts: account %s is %d bytes, want %d", keyed.Pubkey, len(data), dataSize)
		}
		if keyed.Account.Owner != program {
			return nil, fmt.Errorf("getProgramAccounts: account %s is owned by %s, want %s", keyed.Pubkey, keyed.Account.Owner, program)
		}
		out = append(out, ProgramAccount{Address: keyed.Pubkey, Data: data})
	}
	return out, nil
}

// GetSignaturesForAddress reads signatures that mention addr, newest first.
//
// SECURITY: precondition 0 < limit <= maxSignaturesPerRequest.
func (c *ClientConn) GetSignaturesForAddress(ctx context.Context, addr solana.PublicKey, limit int) ([]solana.Signature, error) {
	if limit <= 0 || limit > maxSignaturesPerRequest {
		return nil, fmt.Errorf("getSignaturesForAddress: limit %d is outside 1..%d", limit, maxSignaturesPerRequest)
	}

	res, err := c.rpc.GetSignaturesForAddressWithOpts(ctx, addr, &rpc.GetSignaturesForAddressOpts{
		Limit:      &limit,
		Commitment: rpc.CommitmentFinalized,
	})
	if err != nil {
		return nil, fmt.Errorf("getSignaturesForAddress: %w", err)
	}
	if len(res) > limit {
		return nil, fmt.Errorf("getSignaturesForAddress: want at most %d results, got %d", limit, len(res))
	}

	out := make([]solana.Signature, 0, len(res))
	for _, sig := range res {
		if sig == nil {
			return nil, errors.New("getSignaturesForAddress: empty entry in the result")
		}
		out = append(out, sig.Signature)
	}
	return out, nil
}

// GetTransaction reads a finalized transaction and flattens its top-level instructions.
func (c *ClientConn) GetTransaction(ctx context.Context, sig solana.Signature) (*TransactionResult, error) {
	maxVersion := uint64(0)
	res, err := c.rpc.GetTransaction(ctx, sig, &rpc.GetTransactionOpts{
		Encoding:                       solana.EncodingBase64,
		Commitment:                     rpc.CommitmentFinalized,
		MaxSupportedTransactionVersion: &maxVersion,
	})
	if err != nil {
		return nil, fmt.Errorf("getTransaction %s: %w", sig, err)
	}
	if res == nil || res.Transaction == nil {
		return nil, fmt.Errorf("getTransaction %s: empty response", sig)
	}
	if res.Meta == nil {
		return nil, fmt.Errorf("getTransaction %s: response carries no metadata", sig)
	}
	if len(res.Meta.LogMessages) > MaxLogLinesPerTx {
		return nil, fmt.Errorf("getTransaction %s: %d log lines is past the %d line limit", sig, len(res.Meta.LogMessages), MaxLogLinesPerTx)
	}

	tx, err := res.Transaction.GetTransaction()
	if err != nil {
		return nil, fmt.Errorf("getTransaction %s: decode: %w", sig, err)
	}
	if tx == nil {
		return nil, fmt.Errorf("getTransaction %s: empty transaction", sig)
	}

	instructions := make([]Instruction, 0, len(tx.Message.Instructions))
	for idx, ix := range tx.Message.Instructions {
		// Program ids are static keys, so Message.Program resolves them directly.
		programID, err := tx.Message.Program(ix.ProgramIDIndex)
		if err != nil {
			return nil, fmt.Errorf("getTransaction %s: instruction %d: %w", sig, idx, err)
		}
		instructions = append(instructions, Instruction{ProgramID: programID, Data: ix.Data})
	}

	return &TransactionResult{
		Instructions: instructions,
		LogMessages:  res.Meta.LogMessages,
		Failed:       res.Meta.Err != nil,
	}, nil
}
