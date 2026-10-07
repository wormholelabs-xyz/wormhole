package solacctconn

import (
	"context"
	"errors"
	"fmt"

	"github.com/gagliardetto/solana-go"
	"github.com/gagliardetto/solana-go/rpc"
)

const (
	// getSignatureStatuses caps a request at 256 signatures.
	maxStatusesPerRequest = 256

	// MaxStatusesPerCall bounds one GetSignatureStatuses call, which pages through requests.
	MaxStatusesPerCall = 1000
)

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
// signature the cluster does not know. Each request holds at most maxStatusesPerRequest
// signatures.
//
// SECURITY: precondition len(sigs) <= MaxStatusesPerCall.
func (c *ClientConn) GetSignatureStatuses(ctx context.Context, sigs []solana.Signature) ([]*SignatureStatus, error) {
	if len(sigs) == 0 {
		return nil, nil
	}
	if len(sigs) > MaxStatusesPerCall {
		return nil, fmt.Errorf("getSignatureStatuses: %d signatures is past the limit of %d", len(sigs), MaxStatusesPerCall)
	}

	out := make([]*SignatureStatus, 0, len(sigs))
	for start := 0; start < len(sigs); start += maxStatusesPerRequest {
		chunk := sigs[start:min(start+maxStatusesPerRequest, len(sigs))]

		res, err := c.rpc.GetSignatureStatuses(ctx, false, chunk...)
		if err != nil {
			return nil, fmt.Errorf("getSignatureStatuses: %w", err)
		}
		if res == nil || res.Value == nil {
			return nil, errors.New("getSignatureStatuses: empty response")
		}
		if len(res.Value) != len(chunk) {
			return nil, fmt.Errorf("getSignatureStatuses: want %d results, got %d", len(chunk), len(res.Value))
		}

		for _, status := range res.Value {
			if status == nil {
				out = append(out, nil)
				continue
			}
			out = append(out, &SignatureStatus{
				Confirmed: status.ConfirmationStatus == rpc.ConfirmationStatusConfirmed || status.ConfirmationStatus == rpc.ConfirmationStatusFinalized,
				Err:       parseTxError(status.Err),
			})
		}
	}

	if len(out) != len(sigs) {
		return nil, fmt.Errorf("getSignatureStatuses: want %d results, got %d", len(sigs), len(out))
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

// GetGenesisHash reads the genesis hash of the RPC cluster.
func (c *ClientConn) GetGenesisHash(ctx context.Context) (solana.Hash, error) {
	hash, err := c.rpc.GetGenesisHash(ctx)
	if err != nil {
		return solana.Hash{}, fmt.Errorf("getGenesisHash: %w", err)
	}
	if hash.IsZero() {
		return solana.Hash{}, errors.New("getGenesisHash: zero hash")
	}
	return hash, nil
}
