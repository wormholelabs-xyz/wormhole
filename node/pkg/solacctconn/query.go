package solacctconn

import (
	"context"
	"encoding/binary"
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

	// offset_of!(PendingObservationsLayout, guardian_set_index), state.rs. Little-endian u32.
	pendingGuardianSetIndexOffset = 4

	// offset_of!(PendingObservationsLayout, tx_id), state.rs.
	PendingTxIDOffset = 88

	// MaxLogLinesPerTx bounds the log lines of one transaction. The agave log buffer is
	// 10 KiB per transaction.
	MaxLogLinesPerTx = 2048
)

// GetOwnedAccounts reads accounts at commitment and classifies each against owner. Results
// are positional with addrs. Each request holds at most maxAccountsPerRequest keys.
//
// SECURITY: precondition owner != the system program, whose zero-data account is the
// uninitialised state. A classification error fails the whole read.
func (c *ClientConn) GetOwnedAccounts(ctx context.Context, addrs []solana.PublicKey, owner solana.PublicKey, commitment Commitment) ([]OwnedAccount, error) {
	if commitment != CommitmentConfirmed && commitment != CommitmentFinalized {
		return nil, fmt.Errorf("getMultipleAccounts: unsupported commitment %q", commitment)
	}
	if owner == solana.SystemProgramID {
		return nil, errors.New("getMultipleAccounts: owner is the system program")
	}
	out := make([]OwnedAccount, 0, len(addrs))

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

		for idx, account := range resp.Value {
			owned, err := classifyOwnedAccount(account, owner)
			if err != nil {
				return nil, fmt.Errorf("getMultipleAccounts: account %s: %w", chunk[idx], err)
			}
			out = append(out, owned)
		}
	}

	if len(out) != len(addrs) {
		return nil, fmt.Errorf("getMultipleAccounts: want %d results, got %d", len(addrs), len(out))
	}
	return out, nil
}

// classifyOwnedAccount mirrors pda.rs is_initialised and quorum.rs decide_pending_action.
// A system-owned zero-data account is a prefunded PDA that the program creates over.
func classifyOwnedAccount(account *rpc.Account, owner solana.PublicKey) (OwnedAccount, error) {
	if account == nil {
		return OwnedAccount{State: AccountAbsent}, nil
	}
	data := account.Data.GetBinary()
	switch account.Owner {
	case solana.SystemProgramID:
		if len(data) != 0 {
			return OwnedAccount{}, fmt.Errorf("system-owned account holds %d bytes, want 0", len(data))
		}
		return OwnedAccount{State: AccountUninitialised}, nil
	case owner:
		if account.Executable {
			return OwnedAccount{}, errors.New("owned account is executable")
		}
		return OwnedAccount{State: AccountInitialised, Data: data}, nil
	}
	return OwnedAccount{}, fmt.Errorf("owner %s, want %s or the system program", account.Owner, owner)
}

// GetProgramAccountsByTag reads the accounts of program whose first byte is tag and whose
// length is exactly dataSize, at finalized commitment. A set partition also matches the first
// tx id byte.
//
// SECURITY: precondition: a set partition requires dataSize > PendingTxIDOffset.
func (c *ClientConn) GetProgramAccountsByTag(ctx context.Context, program solana.PublicKey, tag byte, dataSize uint64, guardianSetIndex uint32, partition TxIDPartition) ([]ProgramAccount, error) {
	sortResults := true
	setIndex := binary.LittleEndian.AppendUint32(nil, guardianSetIndex)
	filters := []rpc.RPCFilter{
		{Memcmp: &rpc.RPCFilterMemcmp{Offset: 0, Bytes: solana.Base58{tag}}},
		{Memcmp: &rpc.RPCFilterMemcmp{Offset: pendingGuardianSetIndexOffset, Bytes: solana.Base58(setIndex)}},
		{DataSize: dataSize},
	}
	if firstByte, partitioned := partition.FirstByte(); partitioned {
		if dataSize <= PendingTxIDOffset {
			return nil, fmt.Errorf("getProgramAccounts: data size %d holds no tx id at offset %d", dataSize, PendingTxIDOffset)
		}
		filters = append(filters, rpc.RPCFilter{Memcmp: &rpc.RPCFilterMemcmp{Offset: PendingTxIDOffset, Bytes: solana.Base58{firstByte}}})
	}
	res, err := c.rpc.GetProgramAccountsWithOpts(ctx, program, &rpc.GetProgramAccountsOpts{
		Encoding:    solana.EncodingBase64,
		Commitment:  rpc.CommitmentFinalized,
		Filters:     filters,
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

// GetSignaturesForAddress reads the signatures that mention addr, newest first. A non-zero
// before starts the page below that signature.
//
// SECURITY: precondition 0 < limit <= maxSignaturesPerRequest.
func (c *ClientConn) GetSignaturesForAddress(ctx context.Context, addr solana.PublicKey, before solana.Signature, limit int) ([]SignatureEntry, error) {
	if limit <= 0 || limit > maxSignaturesPerRequest {
		return nil, fmt.Errorf("getSignaturesForAddress: limit %d is outside 1..%d", limit, maxSignaturesPerRequest)
	}

	opts := &rpc.GetSignaturesForAddressOpts{
		Limit:      &limit,
		Commitment: rpc.CommitmentFinalized,
	}
	if !before.IsZero() {
		opts.Before = before
	}
	res, err := c.rpc.GetSignaturesForAddressWithOpts(ctx, addr, opts)
	if err != nil {
		return nil, fmt.Errorf("getSignaturesForAddress: %w", err)
	}
	if len(res) > limit {
		return nil, fmt.Errorf("getSignaturesForAddress: want at most %d results, got %d", limit, len(res))
	}

	out := make([]SignatureEntry, 0, len(res))
	for _, sig := range res {
		if sig == nil {
			return nil, errors.New("getSignaturesForAddress: empty entry in the result")
		}
		if sig.Signature.IsZero() {
			return nil, errors.New("getSignaturesForAddress: zero signature in the result")
		}
		entry := SignatureEntry{Signature: sig.Signature, Failed: sig.Err != nil}
		if sig.BlockTime != nil {
			entry.BlockTime = sig.BlockTime.Time()
		}
		out = append(out, entry)
	}
	return out, nil
}

// GetTransaction reads a finalized transaction and flattens its top-level instructions.
// submit_observations fails with CpiInvocation below the top level, so top-level
// instructions hold every observation.
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
