// Configuration and per-program state for the Solana accountant backend.

package accountant

import (
	"errors"
	"fmt"
	"time"

	"github.com/certusone/wormhole/node/pkg/common"
	"github.com/certusone/wormhole/node/pkg/solacctconn"
	"github.com/gagliardetto/solana-go"
)

// solanaConfirmPollInterval is how often the submission worker polls signature statuses.
var solanaConfirmPollInterval = 2 * time.Second

// AccountantSolanaConfig configures the Solana accountant. The zero value disables it.
type AccountantSolanaConfig struct {
	Conn        solacctconn.Conn
	Program     solana.PublicKey
	Noreplay    solana.PublicKey
	CoreBridge  solana.PublicKey // Core Bridge program that owns the GuardianSet accounts
	FeePayer    solana.PrivateKey
	PriorityFee uint64 // micro-lamports per compute unit; 0 omits the price instruction
}

// solanaBackend is one accountant program the guardian talks to.
type solanaBackend struct {
	conn        solacctconn.Conn
	program     solana.PublicKey
	noreplay    solana.PublicKey
	coreBridge  solana.PublicKey
	authority   solana.PublicKey // NoReplay authority PDA of program
	prefix      []byte           // observation signing prefix
	tag         string
	subChan     chan *common.MessagePublication
	feePayer    solana.PrivateKey
	priorityFee uint64
}

// newSolanaBackend builds the Token Bridge Solana backend. A zero configuration returns a
// nil backend, which disables the Solana accountant.
//
// SECURITY: a partially populated configuration is an error. The checks cover the shape of
// the configuration: every field set, program ids distinct. Reachability, deployment, and
// fee payer funds surface at run time as errors and metrics.
func newSolanaBackend(cfg AccountantSolanaConfig) (*solanaBackend, error) {
	if cfg.Conn == nil && cfg.Program.IsZero() && cfg.Noreplay.IsZero() && cfg.CoreBridge.IsZero() && len(cfg.FeePayer) == 0 {
		return nil, nil
	}

	if cfg.Conn == nil {
		return nil, errors.New("solana accountant: the connection is required")
	}
	if cfg.Program.IsZero() {
		return nil, errors.New("solana accountant: the program id is required")
	}
	if cfg.Noreplay.IsZero() {
		return nil, errors.New("solana accountant: the noreplay program id is required")
	}
	if cfg.CoreBridge.IsZero() {
		return nil, errors.New("solana accountant: the core bridge program id is required")
	}
	if cfg.Program.Equals(cfg.Noreplay) {
		return nil, errors.New("solana accountant: the program and noreplay ids are the same")
	}
	if cfg.CoreBridge.Equals(cfg.Program) {
		return nil, errors.New("solana accountant: the core bridge and program ids are the same")
	}
	if cfg.CoreBridge.Equals(cfg.Noreplay) {
		return nil, errors.New("solana accountant: the core bridge and noreplay ids are the same")
	}
	if len(cfg.FeePayer) != solacctconn.FeePayerKeyLen {
		return nil, fmt.Errorf("solana accountant: the fee payer key is %d bytes, want %d", len(cfg.FeePayer), solacctconn.FeePayerKeyLen)
	}

	authority, err := deriveNoreplayAuthorityPDA(cfg.Program)
	if err != nil {
		return nil, fmt.Errorf("solana accountant: %w", err)
	}

	return &solanaBackend{
		conn:        cfg.Conn,
		program:     cfg.Program,
		noreplay:    cfg.Noreplay,
		coreBridge:  cfg.CoreBridge,
		authority:   authority,
		prefix:      SubmitObservationPrefix,
		tag:         "solana-accountant",
		subChan:     make(chan *common.MessagePublication, subChanSize),
		feePayer:    cfg.FeePayer,
		priorityFee: cfg.PriorityFee,
	}, nil
}
