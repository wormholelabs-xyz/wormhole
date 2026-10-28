package accountant

import (
	"context"
	"errors"
	"fmt"
	"slices"
	"sync"
	"sync/atomic"

	"github.com/certusone/wormhole/node/pkg/solacctconn"
	"github.com/gagliardetto/solana-go"
)

type MockGetOwnedAccountsCall struct {
	Owner      solana.PublicKey
	Commitment solacctconn.Commitment
}

type MockGetSignaturesForAddressCall struct {
	Addr   solana.PublicKey
	Before solana.Signature
	Limit  int
}

// MockAccountantSolanaConn is the solacctconn.Conn test double. Set the exported fields
// before the code under test runs.
type MockAccountantSolanaConn struct {
	GetOwnedAccountsErr error
	ProgramAccounts     []solacctconn.ProgramAccount
	ProgramAccountsErr  error
	// ProgramAccountsMaxResults models the RPC response size limit. Zero disables it.
	ProgramAccountsMaxResults int
	ProgramAccountsPartitions []solacctconn.TxIDPartition
	SubscribeLogsErr          error
	LatestBlockhash           solacctconn.Blockhash
	LatestBlockhashErr        error
	BlockHeight               uint64
	BlockHeightErr            error
	SendTransactionErr        error
	// DefaultSignatureStatus answers every signature. A nil status marks them unknown.
	DefaultSignatureStatus *solacctconn.SignatureStatus
	SignatureStatusesErr   error
	Balance                uint64
	BalanceErr             error

	GetOwnedAccountsCalls        []MockGetOwnedAccountsCall
	GetSignaturesForAddressCalls []MockGetSignaturesForAddressCall
	GetTransactionCalls          []solana.Signature
	ProgramAccountsSetIndices    []uint32
	SentTransactions             []*solana.Transaction
	GetSignatureStatusesCalls    [][]solana.Signature
	GetBalanceCalls              []solana.PublicKey

	mu       sync.Mutex
	accounts map[solana.PublicKey]*solacctconn.OwnedAccount
	// Only confirmed reads see these. They take precedence over accounts.
	confirmedAccounts   map[solana.PublicKey]*solacctconn.OwnedAccount
	signatures          map[solana.PublicKey][]solacctconn.SignatureEntry
	transactions        map[solana.Signature]*solacctconn.TransactionResult
	logEvents           chan solacctconn.LogEvent
	blockHeightHook     func()
	sendTransactionHook func(tx *solana.Transaction) error
	closed              atomic.Bool
}

var _ solacctconn.Conn = (*MockAccountantSolanaConn)(nil)

func NewMockAccountantSolanaConn() *MockAccountantSolanaConn {
	return &MockAccountantSolanaConn{
		accounts:          make(map[solana.PublicKey]*solacctconn.OwnedAccount),
		confirmedAccounts: make(map[solana.PublicKey]*solacctconn.OwnedAccount),
		signatures:        make(map[solana.PublicKey][]solacctconn.SignatureEntry),
		transactions:      make(map[solana.Signature]*solacctconn.TransactionResult),
		// Buffered so tests can queue events before the reader starts.
		logEvents: make(chan solacctconn.LogEvent, 16),
	}
}

func (c *MockAccountantSolanaConn) Close() { c.closed.Store(true) }

// A nil result, or an address that was never set, reads as AccountAbsent.
func (c *MockAccountantSolanaConn) SetAccount(addr solana.PublicKey, result *solacctconn.OwnedAccount) {
	c.mu.Lock()
	defer c.mu.Unlock()
	c.accounts[addr] = result
}

// SetConfirmedAccount sets an account that only confirmed reads see.
func (c *MockAccountantSolanaConn) SetConfirmedAccount(addr solana.PublicKey, result *solacctconn.OwnedAccount) {
	c.mu.Lock()
	defer c.mu.Unlock()
	c.confirmedAccounts[addr] = result
}

func (c *MockAccountantSolanaConn) GetOwnedAccounts(ctx context.Context, addrs []solana.PublicKey, owner solana.PublicKey, commitment solacctconn.Commitment) ([]solacctconn.OwnedAccount, error) {
	c.mu.Lock()
	defer c.mu.Unlock()
	c.GetOwnedAccountsCalls = append(c.GetOwnedAccountsCalls, MockGetOwnedAccountsCall{Owner: owner, Commitment: commitment})
	if c.GetOwnedAccountsErr != nil {
		return nil, c.GetOwnedAccountsErr
	}
	results := make([]solacctconn.OwnedAccount, len(addrs))
	for i, addr := range addrs {
		result := c.accounts[addr]
		if confirmed, ok := c.confirmedAccounts[addr]; ok && commitment == solacctconn.CommitmentConfirmed {
			result = confirmed
		}
		results[i] = solacctconn.OwnedAccount{State: solacctconn.AccountAbsent}
		if result != nil {
			results[i] = *result
		}
	}
	return results, nil
}

// GetProgramAccountsByTag returns ProgramAccounts without the set-index filter, so tests can
// exercise the second check on the account's own set index. It applies the tx id partition.
func (c *MockAccountantSolanaConn) GetProgramAccountsByTag(ctx context.Context, program solana.PublicKey, tag byte, dataSize uint64, guardianSetIndex uint32, partition solacctconn.TxIDPartition) ([]solacctconn.ProgramAccount, error) {
	c.mu.Lock()
	defer c.mu.Unlock()
	c.ProgramAccountsSetIndices = append(c.ProgramAccountsSetIndices, guardianSetIndex)
	c.ProgramAccountsPartitions = append(c.ProgramAccountsPartitions, partition)
	if c.ProgramAccountsErr != nil {
		return nil, c.ProgramAccountsErr
	}
	out := c.ProgramAccounts
	if firstByte, partitioned := partition.FirstByte(); partitioned {
		out = nil
		for _, account := range c.ProgramAccounts {
			if len(account.Data) > solacctconn.PendingTxIDOffset && account.Data[solacctconn.PendingTxIDOffset] == firstByte {
				out = append(out, account)
			}
		}
	}
	if c.ProgramAccountsMaxResults > 0 && len(out) > c.ProgramAccountsMaxResults {
		return nil, fmt.Errorf("getProgramAccounts: %w", solacctconn.ErrResponseTooLarge)
	}
	return out, nil
}

// SetSignaturesForAddress sets the history of addr, newest first, with successful entries.
func (c *MockAccountantSolanaConn) SetSignaturesForAddress(addr solana.PublicKey, sigs []solana.Signature) {
	entries := make([]solacctconn.SignatureEntry, 0, len(sigs))
	for _, sig := range sigs {
		entries = append(entries, solacctconn.SignatureEntry{Signature: sig})
	}
	c.SetSignatureEntries(addr, entries)
}

// SetSignatureEntries sets the history of addr, newest first.
func (c *MockAccountantSolanaConn) SetSignatureEntries(addr solana.PublicKey, entries []solacctconn.SignatureEntry) {
	c.mu.Lock()
	defer c.mu.Unlock()
	c.signatures[addr] = entries
}

// GetSignaturesForAddress returns at most limit entries older than before.
func (c *MockAccountantSolanaConn) GetSignaturesForAddress(ctx context.Context, addr solana.PublicKey, before solana.Signature, limit int) ([]solacctconn.SignatureEntry, error) {
	c.mu.Lock()
	defer c.mu.Unlock()
	c.GetSignaturesForAddressCalls = append(c.GetSignaturesForAddressCalls, MockGetSignaturesForAddressCall{
		Addr: addr, Before: before, Limit: limit,
	})
	history := c.signatures[addr]
	start := 0
	if !before.IsZero() {
		start = len(history)
		for idx, entry := range history {
			if entry.Signature == before {
				start = idx + 1
				break
			}
		}
	}
	end := min(start+limit, len(history))
	return slices.Clone(history[start:end]), nil
}

func (c *MockAccountantSolanaConn) SetTransaction(sig solana.Signature, tx *solacctconn.TransactionResult) {
	c.mu.Lock()
	defer c.mu.Unlock()
	c.transactions[sig] = tx
}

func (c *MockAccountantSolanaConn) GetTransaction(ctx context.Context, sig solana.Signature) (*solacctconn.TransactionResult, error) {
	c.mu.Lock()
	defer c.mu.Unlock()
	c.GetTransactionCalls = append(c.GetTransactionCalls, sig)
	tx, ok := c.transactions[sig]
	if !ok {
		return nil, fmt.Errorf("mock accountant solana conn: no transaction set up for signature %s", sig)
	}
	return tx, nil
}

func (c *MockAccountantSolanaConn) SubscribeLogs(ctx context.Context, program solana.PublicKey) (<-chan solacctconn.LogEvent, error) {
	c.mu.Lock()
	defer c.mu.Unlock()
	if c.SubscribeLogsErr != nil {
		return nil, c.SubscribeLogsErr
	}
	return c.logEvents, nil
}

func (c *MockAccountantSolanaConn) PushLogEvent(evt solacctconn.LogEvent) {
	c.logEvents <- evt
}

// CloseLogEvents simulates a subscription disconnect.
func (c *MockAccountantSolanaConn) CloseLogEvents() {
	close(c.logEvents)
}

func (c *MockAccountantSolanaConn) GetLatestBlockhash(ctx context.Context) (solacctconn.Blockhash, error) {
	c.mu.Lock()
	defer c.mu.Unlock()
	if c.LatestBlockhashErr != nil {
		return solacctconn.Blockhash{}, c.LatestBlockhashErr
	}
	return c.LatestBlockhash, nil
}

// SetBlockHeightHook runs hook after each block height read. The mock releases its lock
// first, so the hook can call back into the mock.
func (c *MockAccountantSolanaConn) SetBlockHeightHook(hook func()) {
	c.mu.Lock()
	defer c.mu.Unlock()
	c.blockHeightHook = hook
}

func (c *MockAccountantSolanaConn) GetBlockHeight(ctx context.Context) (uint64, error) {
	c.mu.Lock()
	height, err, hook := c.BlockHeight, c.BlockHeightErr, c.blockHeightHook
	c.mu.Unlock()

	if hook != nil {
		hook()
	}
	if err != nil {
		return 0, err
	}
	return height, nil
}

// SetSendTransactionHook answers each send from hook. The mock releases its lock first,
// so the hook can call back into the mock.
func (c *MockAccountantSolanaConn) SetSendTransactionHook(hook func(tx *solana.Transaction) error) {
	c.mu.Lock()
	defer c.mu.Unlock()
	c.sendTransactionHook = hook
}

func (c *MockAccountantSolanaConn) SendTransaction(ctx context.Context, tx *solana.Transaction) (solana.Signature, error) {
	c.mu.Lock()
	c.SentTransactions = append(c.SentTransactions, tx)
	sendErr := c.SendTransactionErr
	hook := c.sendTransactionHook
	c.mu.Unlock()

	if hook != nil {
		if err := hook(tx); err != nil {
			return solana.Signature{}, err
		}
	}
	if sendErr != nil {
		return solana.Signature{}, sendErr
	}
	if len(tx.Signatures) == 0 {
		return solana.Signature{}, errors.New("mock accountant solana conn: transaction is not signed")
	}
	return tx.Signatures[0], nil
}

func (c *MockAccountantSolanaConn) GetSignatureStatuses(ctx context.Context, sigs []solana.Signature) ([]*solacctconn.SignatureStatus, error) {
	c.mu.Lock()
	defer c.mu.Unlock()
	c.GetSignatureStatusesCalls = append(c.GetSignatureStatusesCalls, sigs)
	if c.SignatureStatusesErr != nil {
		return nil, c.SignatureStatusesErr
	}
	out := make([]*solacctconn.SignatureStatus, len(sigs))
	for i := range sigs {
		out[i] = c.DefaultSignatureStatus
	}
	return out, nil
}

func (c *MockAccountantSolanaConn) GetBalance(ctx context.Context, addr solana.PublicKey) (uint64, error) {
	c.mu.Lock()
	defer c.mu.Unlock()
	c.GetBalanceCalls = append(c.GetBalanceCalls, addr)
	if c.BalanceErr != nil {
		return 0, c.BalanceErr
	}
	return c.Balance, nil
}
