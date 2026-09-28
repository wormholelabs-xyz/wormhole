package solacctconn

import (
	"context"
	"errors"
	"fmt"

	"github.com/gagliardetto/solana-go"
	"github.com/gagliardetto/solana-go/rpc"
)

// getSignatureStatuses caps a request at 256 signatures.
const maxStatusesPerRequest = 256

// GetLatestBlockhash reads the latest blockhash at confirmed commitment.
func (c *ClientConn) GetLatestBlockhash(ctx context.Context) (Blockhash, error) {
	res, err := c.rpc.GetLatestBlockhash(ctx, rpc.CommitmentConfirmed)
	if err != nil {
		return Blockhash{}, fmt.Errorf("getLatestBlockhash: %w", err)
	}
	if res == nil || res.Value == nil {
		return Blockhash{}, errors.New("getLatestBlockhash: empty response")
	}
	if res.Value.Blockhash.IsZero() {
		return Blockhash{}, errors.New("getLatestBlockhash: zero blockhash")
	}
	if res.Value.LastValidBlockHeight == 0 {
		return Blockhash{}, errors.New("getLatestBlockhash: zero last valid block height")
	}

	return Blockhash{
		Hash:                 res.Value.Blockhash,
		LastValidBlockHeight: res.Value.LastValidBlockHeight,
	}, nil
}

// GetBlockHeight reads the block height at confirmed commitment.
func (c *ClientConn) GetBlockHeight(ctx context.Context) (uint64, error) {
	height, err := c.rpc.GetBlockHeight(ctx, rpc.CommitmentConfirmed)
	if err != nil {
		return 0, fmt.Errorf("getBlockHeight: %w", err)
	}
	return height, nil
}

// SendTransaction submits a signed transaction with preflight at confirmed commitment. A
// preflight failure is a *TxError.
func (c *ClientConn) SendTransaction(ctx context.Context, tx *solana.Transaction) (solana.Signature, error) {
	if tx == nil {
		return solana.Signature{}, errors.New("sendTransaction: no transaction")
	}
	if len(tx.Signatures) == 0 {
		return solana.Signature{}, errors.New("sendTransaction: transaction is unsigned")
	}

	sig, err := c.rpc.SendTransactionWithOpts(ctx, tx, rpc.TransactionOpts{
		SkipPreflight:       false,
		PreflightCommitment: rpc.CommitmentConfirmed,
	})
	if err != nil {
		if txErr := preflightTxError(err); txErr != nil {
			return solana.Signature{}, txErr
		}
		return solana.Signature{}, fmt.Errorf("sendTransaction: %w", err)
	}
	if sig != tx.Signatures[0] {
		return solana.Signature{}, fmt.Errorf("sendTransaction: the cluster returned signature %s, want %s", sig, tx.Signatures[0])
	}
	return sig, nil
}

// GetSignatureStatuses reads statuses positionally with sigs. A nil element marks a
// signature the cluster does not know.
//
// SECURITY: precondition len(sigs) <= maxStatusesPerRequest.
func (c *ClientConn) GetSignatureStatuses(ctx context.Context, sigs []solana.Signature) ([]*SignatureStatus, error) {
	if len(sigs) == 0 {
		return nil, nil
	}
	if len(sigs) > maxStatusesPerRequest {
		return nil, fmt.Errorf("getSignatureStatuses: %d signatures is past the limit of %d", len(sigs), maxStatusesPerRequest)
	}

	res, err := c.rpc.GetSignatureStatuses(ctx, false, sigs...)
	if err != nil {
		return nil, fmt.Errorf("getSignatureStatuses: %w", err)
	}
	if res == nil || res.Value == nil {
		return nil, errors.New("getSignatureStatuses: empty response")
	}
	if len(res.Value) != len(sigs) {
		return nil, fmt.Errorf("getSignatureStatuses: want %d results, got %d", len(sigs), len(res.Value))
	}

	out := make([]*SignatureStatus, len(sigs))
	for idx, status := range res.Value {
		if status == nil {
			continue
		}
		out[idx] = &SignatureStatus{
			Confirmed: status.ConfirmationStatus == rpc.ConfirmationStatusConfirmed || status.ConfirmationStatus == rpc.ConfirmationStatusFinalized,
			Err:       parseTxError(status.Err),
		}
	}
	return out, nil
}

// GetBalance reads the lamport balance of addr at confirmed commitment.
func (c *ClientConn) GetBalance(ctx context.Context, addr solana.PublicKey) (uint64, error) {
	res, err := c.rpc.GetBalance(ctx, addr, rpc.CommitmentConfirmed)
	if err != nil {
		return 0, fmt.Errorf("getBalance %s: %w", addr, err)
	}
	if res == nil {
		return 0, fmt.Errorf("getBalance %s: empty response", addr)
	}
	return res.Value, nil
}
