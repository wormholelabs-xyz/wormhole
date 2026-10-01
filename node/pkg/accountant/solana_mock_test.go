package accountant

import (
	"context"
	"errors"
	"fmt"
	"sync"

	"github.com/certusone/wormhole/node/pkg/solacctconn"
	"github.com/gagliardetto/solana-go"
)

type MockGetSignaturesForAddressCall struct {
	Addr  solana.PublicKey
	Limit int
}

// MockAccountantSolanaConn is the solacctconn.Conn test double. Set the exported fields
// before the code under test runs.
type MockAccountantSolanaConn struct {
	GetMultipleAccountsErr error
	ProgramAccounts        []solacctconn.ProgramAccount
	ProgramAccountsErr     error
	SubscribeLogsErr       error
	LatestBlockhash        solacctconn.Blockhash
	LatestBlockhashErr     error
	BlockHeight            uint64
	BlockHeightErr         error
	SendTransactionErr     error
	// DefaultSignatureStatus answers every signature. A nil status marks them unknown.
	DefaultSignatureStatus *solacctconn.SignatureStatus
	SignatureStatusesErr   error
	Balance                uint64
	BalanceErr             error

	GetMultipleAccountsCommitments []solacctconn.Commitment
	GetSignaturesForAddressCalls   []MockGetSignaturesForAddressCall
	SentTransactions               []*solana.Transaction
	GetSignatureStatusesCalls      [][]solana.Signature
	GetBalanceCalls                []solana.PublicKey

	mu       sync.Mutex
	accounts map[solana.PublicKey]*solacctconn.AccountResult
	// Only confirmed reads see these. They take precedence over accounts.
	confirmedAccounts   map[solana.PublicKey]*solacctconn.AccountResult
	signatures          map[solana.PublicKey][]solana.Signature
	transactions        map[solana.Signature]*solacctconn.TransactionResult
	logEvents           chan solacctconn.LogEvent
	blockHeightHook     func()
	sendTransactionHook func(tx *solana.Transaction) error
}

var _ solacctconn.Conn = (*MockAccountantSolanaConn)(nil)

func NewMockAccountantSolanaConn() *MockAccountantSolanaConn {
	return &MockAccountantSolanaConn{
		accounts:          make(map[solana.PublicKey]*solacctconn.AccountResult),
		confirmedAccounts: make(map[solana.PublicKey]*solacctconn.AccountResult),
		signatures:        make(map[solana.PublicKey][]solana.Signature),
		transactions:      make(map[solana.Signature]*solacctconn.TransactionResult),
		// Buffered so tests can queue events before the reader starts.
		logEvents: make(chan solacctconn.LogEvent, 16),
	}
}

func (c *MockAccountantSolanaConn) Close() {}

// A nil result marks an absent account.
func (c *MockAccountantSolanaConn) SetAccount(addr solana.PublicKey, result *solacctconn.AccountResult) {
	c.mu.Lock()
	defer c.mu.Unlock()
	c.accounts[addr] = result
}

// SetConfirmedAccount sets an account that only confirmed reads see.
func (c *MockAccountantSolanaConn) SetConfirmedAccount(addr solana.PublicKey, result *solacctconn.AccountResult) {
	c.mu.Lock()
	defer c.mu.Unlock()
	c.confirmedAccounts[addr] = result
}

func (c *MockAccountantSolanaConn) GetMultipleAccounts(ctx context.Context, addrs []solana.PublicKey, commitment solacctconn.Commitment) ([]*solacctconn.AccountResult, error) {
	c.mu.Lock()
	defer c.mu.Unlock()
	c.GetMultipleAccountsCommitments = append(c.GetMultipleAccountsCommitments, commitment)
	if c.GetMultipleAccountsErr != nil {
		return nil, c.GetMultipleAccountsErr
	}
	results := make([]*solacctconn.AccountResult, len(addrs))
	for i, addr := range addrs {
		results[i] = c.accounts[addr]
		if confirmed, ok := c.confirmedAccounts[addr]; ok && commitment == solacctconn.CommitmentConfirmed {
			results[i] = confirmed
		}
	}
	return results, nil
}

func (c *MockAccountantSolanaConn) GetProgramAccountsByTag(ctx context.Context, program solana.PublicKey, tag byte, dataSize uint64) ([]solacctconn.ProgramAccount, error) {
	c.mu.Lock()
	defer c.mu.Unlock()
	if c.ProgramAccountsErr != nil {
		return nil, c.ProgramAccountsErr
	}
	return c.ProgramAccounts, nil
}

func (c *MockAccountantSolanaConn) SetSignaturesForAddress(addr solana.PublicKey, sigs []solana.Signature) {
	c.mu.Lock()
	defer c.mu.Unlock()
	c.signatures[addr] = sigs
}

func (c *MockAccountantSolanaConn) GetSignaturesForAddress(ctx context.Context, addr solana.PublicKey, limit int) ([]solana.Signature, error) {
	c.mu.Lock()
	defer c.mu.Unlock()
	c.GetSignaturesForAddressCalls = append(c.GetSignaturesForAddressCalls, MockGetSignaturesForAddressCall{
		Addr: addr, Limit: limit,
	})
	return c.signatures[addr], nil
}

func (c *MockAccountantSolanaConn) SetTransaction(sig solana.Signature, tx *solacctconn.TransactionResult) {
	c.mu.Lock()
	defer c.mu.Unlock()
	c.transactions[sig] = tx
}

func (c *MockAccountantSolanaConn) GetTransaction(ctx context.Context, sig solana.Signature) (*solacctconn.TransactionResult, error) {
	c.mu.Lock()
	defer c.mu.Unlock()
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
