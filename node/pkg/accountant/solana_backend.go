// Configuration and per-program state for the Solana accountant backends.

package accountant

import (
	"errors"
	"fmt"
	"time"

	"github.com/certusone/wormhole/node/pkg/common"
	"github.com/certusone/wormhole/node/pkg/solacctconn"
	"github.com/gagliardetto/solana-go"
	"github.com/prometheus/client_golang/prometheus"
	"github.com/wormhole-foundation/wormhole/sdk/vaa"
)

// solanaConfirmPollInterval is how often the submission worker polls signature statuses.
// Matches batchTimeout. Each poll costs two RPC calls per batch. The block height check
// decides when a transaction is dropped.
var solanaConfirmPollInterval = 2 * time.Second

// AccountantSolanaConfig configures the Solana accountants. The zero value disables them.
// Program and NttProgram each enable one program. They share the connection and fee payer.
type AccountantSolanaConfig struct {
	Conn        solacctconn.Conn
	Program     solana.PublicKey // WTT accountant. Zero disables it.
	NttProgram  solana.PublicKey // NTT accountant. Zero disables it.
	Noreplay    solana.PublicKey
	CoreBridge  solana.PublicKey // Core Bridge program that owns the GuardianSet accounts
	FeePayer    solana.PrivateKey
	PriorityFee uint64 // total lamports per transaction v1
}

// solanaProgramFamily selects the accountant program a backend talks to.
type solanaProgramFamily uint8

const (
	solanaFamilyWTT solanaProgramFamily = iota + 1
	solanaFamilyNTT
)

// String is the metric label and log value of the family.
func (f solanaProgramFamily) String() string {
	switch f {
	case solanaFamilyWTT:
		return "wtt"
	case solanaFamilyNTT:
		return "ntt"
	}
	return fmt.Sprintf("unknown(%d)", uint8(f))
}

// solanaObservationRecord is the hashed record one program family submits for a transfer.
type solanaObservationRecord interface {
	identity() (vaa.ChainID, vaa.Address, uint64)
	// The record in the program's field order. Hashed for both digests.
	pack() ([]byte, error)
	// keccak256(keccak256(pack())), computed now.
	computeContentDigest() ([32]byte, error)
	// keccak256(keccak256(pack())) as cached at construction: the pending-PDA seed and the
	// commit-log digest.
	committedDigest() [32]byte
	// The discriminator and instruction data of submit_observations.
	submitInstructionData(head solanaSubmitHead) ([]byte, error)
}

// solanaBackendMetrics are the family-labelled Solana counters of one backend.
type solanaBackendMetrics struct {
	eventsReceived     prometheus.Counter
	transfersApproved  prometheus.Counter
	transfersSubmitted prometheus.Counter
	submitFailures     prometheus.Counter
	feePayerErrors     prometheus.Counter
	feePayerLamports   prometheus.Gauge
	malformedLogs      prometheus.Counter
	failedTxSkipped    prometheus.Counter
	connectionErrors   prometheus.Counter
	auditErrors        prometheus.Counter
}

func newSolanaBackendMetrics(family solanaProgramFamily) solanaBackendMetrics {
	label := family.String()
	return solanaBackendMetrics{
		eventsReceived:     solanaEventsReceived.WithLabelValues(label),
		transfersApproved:  solanaTransfersApproved.WithLabelValues(label),
		transfersSubmitted: solanaTransfersSubmitted.WithLabelValues(label),
		submitFailures:     solanaSubmitFailures.WithLabelValues(label),
		feePayerErrors:     solanaFeePayerErrors.WithLabelValues(label),
		feePayerLamports:   solanaFeePayerLamports.WithLabelValues(label),
		malformedLogs:      solanaMalformedLogs.WithLabelValues(label),
		failedTxSkipped:    solanaFailedTxSkipped.WithLabelValues(label),
		connectionErrors:   solanaConnectionErrors.WithLabelValues(label),
		auditErrors:        solanaAuditErrors.WithLabelValues(label),
	}
}

// solanaBackend is one accountant program the guardian talks to.
type solanaBackend struct {
	family      solanaProgramFamily
	backend     accountantBackend // slot in pendingEntry.state.submitPending
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
	// The audit goroutine owns it.
	historyCursors solanaHistoryCursors
	// Address after which the next program-account pass starts. The audit goroutine owns it.
	programAuditCursor solana.PublicKey
	// First tx id byte at which the next partitioned read starts. The audit goroutine owns it.
	programAuditPartition uint8
	metrics               solanaBackendMetrics
}

// covers reports whether the backend's program accounts pe.
func (b *solanaBackend) covers(pe *pendingEntry) bool {
	return pe.isNTT == (b.family == solanaFamilyNTT)
}

// newSolanaBackends builds the WTT and NTT Solana backends. A zero configuration returns
// two nil backends, which disables the Solana accountant. A zero program id disables the
// backend of that program.
//
// SECURITY: a partial configuration is an error. The checks cover the shape of the
// configuration. Every shared field must be set. At least one program must be set. The
// program ids must be different from each other. Reachability, deployment and fee payer
// funds show at run time as errors and metrics.
func newSolanaBackends(cfg AccountantSolanaConfig) (wtt *solanaBackend, ntt *solanaBackend, err error) {
	if cfg.Conn == nil && cfg.Program.IsZero() && cfg.NttProgram.IsZero() && cfg.Noreplay.IsZero() && cfg.CoreBridge.IsZero() && len(cfg.FeePayer) == 0 {
		return nil, nil, nil
	}

	if cfg.Conn == nil {
		return nil, nil, errors.New("solana accountant: the connection is required")
	}
	if cfg.Program.IsZero() && cfg.NttProgram.IsZero() {
		return nil, nil, errors.New("solana accountant: a WTT or NTT program id is required")
	}
	if cfg.Noreplay.IsZero() {
		return nil, nil, errors.New("solana accountant: the noreplay program id is required")
	}
	if cfg.CoreBridge.IsZero() {
		return nil, nil, errors.New("solana accountant: the core bridge program id is required")
	}
	if len(cfg.FeePayer) != solacctconn.FeePayerKeyLen {
		return nil, nil, fmt.Errorf("solana accountant: the fee payer key is %d bytes, want %d", len(cfg.FeePayer), solacctconn.FeePayerKeyLen)
	}

	ids := [...]struct {
		name string
		id   solana.PublicKey
	}{
		{"program", cfg.Program},
		{"ntt program", cfg.NttProgram},
		{"noreplay", cfg.Noreplay},
		{"core bridge", cfg.CoreBridge},
	}
	for i := range ids {
		for j := i + 1; j < len(ids); j++ {
			if !ids[i].id.IsZero() && ids[i].id.Equals(ids[j].id) {
				return nil, nil, fmt.Errorf("solana accountant: the %s and %s ids are the same", ids[i].name, ids[j].name)
			}
		}
	}

	if !cfg.Program.IsZero() {
		if wtt, err = newSolanaBackend(cfg, solanaFamilyWTT, cfg.Program); err != nil {
			return nil, nil, err
		}
	}
	if !cfg.NttProgram.IsZero() {
		if ntt, err = newSolanaBackend(cfg, solanaFamilyNTT, cfg.NttProgram); err != nil {
			return nil, nil, err
		}
	}
	return wtt, ntt, nil
}

// newSolanaBackend builds one backend. SECURITY: precondition: newSolanaBackends checked cfg.
func newSolanaBackend(cfg AccountantSolanaConfig, family solanaProgramFamily, program solana.PublicKey) (*solanaBackend, error) {
	authority, err := deriveNoreplayAuthorityPDA(program)
	if err != nil {
		return nil, fmt.Errorf("solana accountant: %w", err)
	}

	b := &solanaBackend{
		family:      family,
		conn:        cfg.Conn,
		program:     program,
		noreplay:    cfg.Noreplay,
		coreBridge:  cfg.CoreBridge,
		authority:   authority,
		subChan:     make(chan *common.MessagePublication, subChanSize),
		feePayer:    cfg.FeePayer,
		priorityFee: cfg.PriorityFee,
		metrics:     newSolanaBackendMetrics(family),
	}
	switch family {
	case solanaFamilyWTT:
		b.backend = backendSolana
		b.prefix = SubmitObservationPrefix
		b.tag = "solana-accountant"
	case solanaFamilyNTT:
		b.backend = backendSolanaNTT
		b.prefix = NttSubmitObservationPrefix
		b.tag = "solana-ntt-accountant"
	default:
		return nil, fmt.Errorf("solana accountant: unknown program family %s", family)
	}
	return b, nil
}
