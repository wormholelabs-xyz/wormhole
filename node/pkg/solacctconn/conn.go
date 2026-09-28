package solacctconn

import (
	"context"

	"github.com/gagliardetto/solana-go"
	"github.com/gagliardetto/solana-go/rpc"
)

// Conn is the RPC surface of the Solana accountant program.
type Conn interface {
	Close()

	// Results are positional with addrs. A nil element marks an absent account.
	GetMultipleAccounts(ctx context.Context, addrs []solana.PublicKey, commitment Commitment) ([]*AccountResult, error)

	// Filters: memcmp(offset 0, tag) and dataSize.
	GetProgramAccountsByTag(ctx context.Context, program solana.PublicKey, tag byte, dataSize uint64) ([]ProgramAccount, error)

	// Newest first.
	GetSignaturesForAddress(ctx context.Context, addr solana.PublicKey, limit int) ([]solana.Signature, error)

	GetTransaction(ctx context.Context, sig solana.Signature) (*TransactionResult, error)

	// The channel closes on disconnect.
	SubscribeLogs(ctx context.Context, program solana.PublicKey) (<-chan LogEvent, error)

	// At confirmed commitment.
	GetLatestBlockhash(ctx context.Context) (Blockhash, error)

	// At confirmed commitment.
	GetBlockHeight(ctx context.Context) (uint64, error)

	// Preflight runs at confirmed commitment. A preflight failure is a *TxError.
	SendTransaction(ctx context.Context, tx *solana.Transaction) (solana.Signature, error)

	// Results are positional with sigs. A nil element marks an unknown signature.
	GetSignatureStatuses(ctx context.Context, sigs []solana.Signature) ([]*SignatureStatus, error)

	// Lamport balance at confirmed commitment.
	GetBalance(ctx context.Context, addr solana.PublicKey) (uint64, error)
}

// Commitment is the commitment level of an account read. Use CommitmentConfirmed or
// CommitmentFinalized; the zero value is rejected.
type Commitment struct {
	level rpc.CommitmentType
}

var (
	CommitmentConfirmed = Commitment{level: rpc.CommitmentConfirmed}
	CommitmentFinalized = Commitment{level: rpc.CommitmentFinalized}
)

func (c Commitment) String() string {
	return string(c.level)
}

type Blockhash struct {
	Hash                 solana.Hash
	LastValidBlockHeight uint64
}

type SignatureStatus struct {
	Confirmed bool
	Err       *TxError
}

type AccountResult struct {
	Data []byte
}

// ProgramAccount is owned by the queried program.
type ProgramAccount struct {
	Address solana.PublicKey
	Data    []byte
}

type Instruction struct {
	ProgramID solana.PublicKey
	Data      []byte
}

type TransactionResult struct {
	Instructions []Instruction
	LogMessages  []string
	// Do not parse LogMessages when Failed is true.
	Failed bool
}

type LogEvent struct {
	Signature solana.Signature
	Logs      []string
	Failed    bool
}
