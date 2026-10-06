// The accountant package manages the interface to the accountant smart contract on wormchain. It is passed all VAAs before
// they are signed and published. It determines if the VAA is for a token bridge transfer, and if it is, it submits an observation
// request to the accountant contract. When that happens, the VAA is queued up until the accountant contract responds indicating
// that the VAA has been approved. If the VAA is approved, this module will forward the VAA back to the processor loop to be signed
// and published.

package accountant

import (
	"bytes"
	"context"
	"encoding/hex"
	"fmt"
	"slices"
	"sync"
	"time"

	"github.com/certusone/wormhole/node/pkg/common"
	guardianDB "github.com/certusone/wormhole/node/pkg/db"
	"github.com/certusone/wormhole/node/pkg/guardiansigner"
	gossipv1 "github.com/certusone/wormhole/node/pkg/proto/gossip/v1"
	"github.com/certusone/wormhole/node/pkg/supervisor"
	sdktypes "github.com/cosmos/cosmos-sdk/types"
	sdktx "github.com/cosmos/cosmos-sdk/types/tx"
	"github.com/wormhole-foundation/wormhole/sdk"
	"github.com/wormhole-foundation/wormhole/sdk/vaa"

	ethCommon "github.com/ethereum/go-ethereum/common"
	ethCrypto "github.com/ethereum/go-ethereum/crypto"

	"go.uber.org/zap"
)

// MsgChannelCapacity specifies the capacity of the message channel used to publish messages released from the accountant.
// This channel should not back up, but if it does, the accountant will start dropping messages, which would require reobservations.
const MsgChannelCapacity = 5 * DefaultSubmitObservationBatchSize

type (
	AccountantWormchainConn interface {
		Close()
		SenderAddress() string
		SubmitQuery(ctx context.Context, contractAddress string, query []byte) ([]byte, error)
		SignAndBroadcastTx(ctx context.Context, msg sdktypes.Msg) (*sdktx.BroadcastTxResponse, error)
		BroadcastTxResponseToString(txResp *sdktx.BroadcastTxResponse) string
	}

	// emitterKey is the key to a map of emitters to be monitored
	emitterKey struct {
		emitterChainId vaa.ChainID
		emitterAddr    vaa.Address
	}

	// validEmitters is a set of supported emitter chain / address pairs. The payload is the enforcement flag.
	validEmitters map[emitterKey]bool

	// pendingEntry is the payload for each pending transfer
	pendingEntry struct {
		msg         *common.MessagePublication
		msgId       string
		digest      string
		isNTT       bool
		enforceFlag bool

		// vaaDigest is the decoded digest. newPendingEntry sets it while the Solana backend is enabled.
		vaaDigest [digestLen]byte
		// solanaFields is the record the Solana WTT accountant program hashes. newPendingEntry sets
		// it on Token Bridge entries while the Solana WTT backend is enabled.
		solanaFields *solanaObservationFields

		// solanaNttFields is the record the Solana NTT accountant program hashes. It is set
		// for NTT entries while the Solana NTT backend is enabled.
		solanaNttFields *solanaNttObservationFields

		// stateLock is used to protect the contents of the state struct.
		stateLock sync.Mutex

		// The state struct contains anything that can be modified. It is protected by the state lock.
		state struct {
			// updTime is the time that the state struct was last updated.
			updTime time.Time

			// submitPending shows, per backend, that the observation waits in the channel or is in an outstanding transaction.
			// The audit must not resubmit to a backend whose flag is set.
			submitPending [numAccountantBackends]bool

			// solanaSiblingTxIDs are reobserved source transaction ids of msg other than
			// msg.TxID. Each one seeds its own Solana pending account. At most
			// maxSolanaSiblingTxIDs entries.
			solanaSiblingTxIDs []solanaTxID
		}
	}
)

// maxSolanaSiblingTxIDs bounds the extra tx ids of one transfer. A Core Bridge message has
// at most two ids: its message account and the signature of the transaction that closes it.
const maxSolanaSiblingTxIDs = 1

// accountantBackend indexes the per-backend submission state of a pending entry.
type accountantBackend uint8

const (
	// The wormchain contract. NTT entries use this slot for the NTT contract.
	backendWormchain accountantBackend = iota
	// The Solana WTT program. It accounts Token Bridge entries.
	backendSolana
	// The Solana NTT program. It accounts NTT entries.
	backendSolanaNTT
	numAccountantBackends
)

// solanaRecord returns the record pe submits to family. It returns nil if that family
// accounts a different kind of entry.
func (pe *pendingEntry) solanaRecord(family solanaProgramFamily) solanaObservationRecord {
	switch family {
	case solanaFamilyWTT:
		if !pe.isNTT && pe.solanaFields != nil {
			return pe.solanaFields
		}
	case solanaFamilyNTT:
		if pe.isNTT && pe.solanaNttFields != nil {
			return pe.solanaNttFields
		}
	}
	return nil
}

// Accountant is the object that manages the interface to the wormchain accountant smart contract.
type Accountant struct {
	ctx                        context.Context
	logger                     *zap.Logger
	db                         guardianDB.AccountantDB
	obsvReqWriteC              chan<- *gossipv1.ObservationRequest
	contract                   string
	wsUrl                      string
	wormchainConn              AccountantWormchainConn
	enforceFlag                bool
	guardianSigner             guardiansigner.GuardianSigner
	gst                        *common.GuardianSetState
	guardianAddr               ethCommon.Address
	msgChan                    chan<- *common.MessagePublication
	tokenBridges               validEmitters
	pendingTransfersLock       sync.Mutex
	pendingTransfers           map[string]*pendingEntry // Key is the message ID (emitterChain/emitterAddr/seqNo)
	subChan                    chan *common.MessagePublication
	submitObservationBatchSize int
	env                        common.Environment

	nttContract       string
	nttWormchainConn  AccountantWormchainConn
	nttDirectEmitters validEmitters
	nttArEmitters     validEmitters
	nttSubChan        chan *common.MessagePublication

	solanaCfg AccountantSolanaConfig
	// Start builds solana and solanaNtt from solanaCfg. A nil value disables that Solana backend.
	solana    *solanaBackend
	solanaNtt *solanaBackend
	// Capacity 1, so requests that arrive during an audit coalesce into one more audit.
	solanaAuditRequests chan struct{}
}

// On startup, there can be a large number of re-submission requests.
const subChanSize = 5000

// auditSubmitTimeout is the timeout for blocking channel writes during audit.
const auditSubmitTimeout = 30 * time.Second

// wormchainBaseEnabled returns true if the wormchain base accountant is enabled.
func (acct *Accountant) wormchainBaseEnabled() bool {
	return acct.contract != ""
}

// solanaEnabled returns true if the Solana base accountant is enabled.
func (acct *Accountant) solanaEnabled() bool {
	return acct.solana != nil
}

// solanaNttEnabled returns true if the Solana NTT accountant is enabled.
func (acct *Accountant) solanaNttEnabled() bool {
	return acct.solanaNtt != nil
}

// solanaBackends returns the enabled Solana backends.
func (acct *Accountant) solanaBackends() []*solanaBackend {
	out := make([]*solanaBackend, 0, 2)
	if acct.solana != nil {
		out = append(out, acct.solana)
	}
	if acct.solanaNtt != nil {
		out = append(out, acct.solanaNtt)
	}
	return out
}

// baseEnabled returns true if any backend covers Token Bridge transfers.
func (acct *Accountant) baseEnabled() bool {
	return acct.wormchainBaseEnabled() || acct.solanaEnabled()
}

// NewAccountant creates a new instance of the Accountant object.
func NewAccountant(
	ctx context.Context,
	logger *zap.Logger,
	db guardianDB.AccountantDB,
	obsvReqWriteC chan<- *gossipv1.ObservationRequest,
	contract string, // the address of the smart contract on wormchain
	wsUrl string, // the URL of the wormchain websocket interface
	wormchainConn AccountantWormchainConn, // used for communicating with the smart contract
	enforceFlag bool, // whether or not accountant should be enforced
	nttContract string, // the address of the NTT smart contract on wormchain
	nttWormchainConn AccountantWormchainConn, // used for communicating with the NTT smart contract
	solanaCfg AccountantSolanaConfig, // the Solana accountant backend; the zero value disables it
	guardianSigner guardiansigner.GuardianSigner, // the guardian signer used for signing observation requests
	gst *common.GuardianSetState, // used to get the current guardian set index when sending observation requests
	msgChan chan<- *common.MessagePublication, // the channel where transfers received by the accountant runnable should be published
	submitObservationBatchSize int, // maximum number of observations to submit to the contract in one transaction
	env common.Environment, // Controls the set of token bridges to be monitored
) *Accountant {
	if submitObservationBatchSize <= 0 {
		submitObservationBatchSize = DefaultSubmitObservationBatchSize
	}

	return &Accountant{
		ctx:                        ctx,
		logger:                     logger.With(zap.String("component", "gacct")),
		db:                         db,
		obsvReqWriteC:              obsvReqWriteC,
		contract:                   contract,
		wsUrl:                      wsUrl,
		wormchainConn:              wormchainConn,
		enforceFlag:                enforceFlag,
		guardianSigner:             guardianSigner,
		gst:                        gst,
		guardianAddr:               ethCrypto.PubkeyToAddress(guardianSigner.PublicKey(ctx)),
		msgChan:                    msgChan,
		tokenBridges:               make(validEmitters),
		pendingTransfers:           make(map[string]*pendingEntry),
		subChan:                    make(chan *common.MessagePublication, subChanSize),
		submitObservationBatchSize: submitObservationBatchSize,
		env:                        env,

		nttContract:       nttContract,
		nttWormchainConn:  nttWormchainConn,
		nttDirectEmitters: make(validEmitters),
		nttArEmitters:     make(validEmitters),
		nttSubChan:        make(chan *common.MessagePublication, subChanSize),

		solanaCfg:           solanaCfg,
		solanaAuditRequests: make(chan struct{}, 1),
	}
}

// Start initializes the accountant and starts the worker and watcher runnables.
func (acct *Accountant) Start(ctx context.Context) error {
	acct.logger.Debug("entering Start", zap.Bool("enforceFlag", acct.enforceFlag), zap.Bool("baseEnabled", acct.baseEnabled()), zap.Bool("nttEnabled", acct.nttEnabled()), zap.Int("submitObservationBatchSize", acct.submitObservationBatchSize))
	acct.pendingTransfersLock.Lock()
	defer acct.pendingTransfersLock.Unlock()

	solBackend, solNttBackend, err := newSolanaBackends(acct.solanaCfg)
	if err != nil {
		return fmt.Errorf("failed to configure the solana accountant: %w", err)
	}
	acct.solana = solBackend
	acct.solanaNtt = solNttBackend
	if len(acct.solanaBackends()) != 0 {
		acct.logger.Debug("solana accountant enabled", zap.Bool("baseEnabled", acct.baseEnabled()), zap.Bool("solanaNttEnabled", acct.solanaNttEnabled()))
	}

	if !acct.baseEnabled() && !acct.nttEnabled() {
		return fmt.Errorf("start should not be called when neither base nor NTT accountant are enabled")
	}

	if acct.baseEnabled() {
		emitterMap := sdk.KnownTokenbridgeEmitters
		if acct.env == common.TestNet {
			emitterMap = sdk.KnownTestnetTokenbridgeEmitters
		} else if acct.env == common.UnsafeDevNet || acct.env == common.GoTest || acct.env == common.AccountantMock {
			emitterMap = sdk.KnownDevnetTokenbridgeEmitters
		}

		// Build the map of token bridges to be monitored.
		for chainId, emitterAddrBytes := range emitterMap {
			emitterAddr, err := vaa.BytesToAddress(emitterAddrBytes)
			if err != nil {
				return fmt.Errorf("failed to convert emitter address for chain: %v", chainId)
			}

			tbk := emitterKey{emitterChainId: chainId, emitterAddr: emitterAddr}
			_, exists := acct.tokenBridges[tbk]
			if exists {
				return fmt.Errorf("detected duplicate token bridge for chain: %v", chainId)
			}

			acct.tokenBridges[tbk] = acct.enforceFlag
			acct.logger.Info("will monitor token bridge:", zap.Stringer("emitterChainId", tbk.emitterChainId), zap.Stringer("emitterAddr", tbk.emitterAddr))
		}
	}

	// The NTT data structures should be set up before we reload from the db.
	if acct.nttEnabled() {
		if err := acct.nttStart(ctx); err != nil {
			return fmt.Errorf("failed to start ntt accountant: %w", err)
		}
	}

	// Load any existing pending transfers from the db.
	if err := acct.loadPendingTransfers(); err != nil {
		return fmt.Errorf("failed to load pending transfers from the db: %w", err)
	}

	// Start the watcher to listen to transfer events from the smart contract.
	if acct.wormchainBaseEnabled() {
		if acct.env == common.AccountantMock {
			// We're not in a runnable context, so we can't use supervisor.
			go func() {
				_ = acct.baseWorker(ctx)
			}()
		} else if acct.env != common.GoTest {
			if err := supervisor.Run(ctx, "acctworker", common.WrapWithScissors(acct.baseWorker, "acctworker")); err != nil {
				return fmt.Errorf("failed to start submit observation worker: %w", err)
			}

			if err := supervisor.Run(ctx, "acctwatcher", common.WrapWithScissors(acct.baseWatcher, "acctwatcher")); err != nil {
				return fmt.Errorf("failed to start watcher: %w", err)
			}

		}
	}

	if acct.solanaEnabled() && acct.env != common.AccountantMock && acct.env != common.GoTest {
		if err := supervisor.Run(ctx, "acctsolworker", common.WrapWithScissors(acct.solanaBaseWorker, "acctsolworker")); err != nil {
			return fmt.Errorf("failed to start solana submit observation worker: %w", err)
		}

		if err := supervisor.Run(ctx, "acctsolwatcher", common.WrapWithScissors(acct.solanaBaseWatcher, "acctsolwatcher")); err != nil {
			return fmt.Errorf("failed to start solana watcher: %w", err)
		}
	}

	if acct.solanaNttEnabled() && acct.env != common.AccountantMock && acct.env != common.GoTest {
		if err := supervisor.Run(ctx, "acctsolnttworker", common.WrapWithScissors(acct.solanaNttWorker, "acctsolnttworker")); err != nil {
			return fmt.Errorf("failed to start solana NTT submit observation worker: %w", err)
		}

		if err := supervisor.Run(ctx, "acctsolnttwatcher", common.WrapWithScissors(acct.solanaNttWatcher, "acctsolnttwatcher")); err != nil {
			return fmt.Errorf("failed to start solana NTT watcher: %w", err)
		}
	}

	// Start the audit worker if not mocking/testing and either Global or NTT accountant are enabled
	if acct.env != common.AccountantMock && acct.env != common.GoTest && (acct.baseEnabled() || acct.nttEnabled()) {
		if err := supervisor.Run(ctx, "acctaudit", common.WrapWithScissors(acct.audit, "acctaudit")); err != nil {
			return fmt.Errorf("failed to start audit worker: %w", err)
		}
	}

	return nil
}

func (acct *Accountant) Close() {
	if acct.wormchainConn != nil {
		acct.wormchainConn.Close()
		acct.wormchainConn = nil
	}
	if acct.nttWormchainConn != nil {
		acct.nttWormchainConn.Close()
		acct.nttWormchainConn = nil
	}
	// Both Solana backends share one connection.
	if backends := acct.solanaBackends(); len(backends) != 0 {
		backends[0].conn.Close()
		acct.solana = nil
		acct.solanaNtt = nil
	}
}

// FeatureString reads the static configuration. The p2p options call it before Start builds the backends.
func (acct *Accountant) FeatureString() string {
	var ret string
	if !acct.enforceFlag {
		ret = "acct-logonly"
	} else {
		ret = "acct"
	}
	if acct.wormchainNttEnabled() {
		if ret != "" {
			ret += ":"
		}
		ret += "ntt-acct"
	}
	if !acct.solanaCfg.Program.IsZero() {
		if ret != "" {
			ret += ":"
		}
		if !acct.enforceFlag {
			ret += "sol-acct-logonly"
		} else {
			ret += "sol-acct"
		}
	}
	if !acct.solanaCfg.NttProgram.IsZero() {
		if ret != "" {
			ret += ":"
		}
		if !acct.enforceFlag {
			ret += "sol-ntt-acct-logonly"
		} else {
			ret += "sol-ntt-acct"
		}
	}

	return ret
}

// IsMessageCoveredByAccountant returns `true` if a message should be processed by the Global Accountant, `false` if not.
func (acct *Accountant) IsMessageCoveredByAccountant(msg *common.MessagePublication) bool {
	ret, _, _ := acct.isMessageCoveredByAccountant(msg)
	return ret
}

// isMessageCoveredByAccountant returns true if a message should be processed by the Global Accountant, false if not.
// It also returns whether or not it is a Native Token Transfer and whether or not accounting is being enforced for this emitter.
func (acct *Accountant) isMessageCoveredByAccountant(msg *common.MessagePublication) (bool, bool, bool) {
	isTBT, enforceFlag := acct.isTokenBridgeTransfer(msg)
	if isTBT {
		return true, false, enforceFlag
	}

	isNTT, enforceFlag := nttIsMsgDirectNTT(msg, acct.nttDirectEmitters)
	if isNTT {
		return true, true, enforceFlag
	}

	isNTT, enforceFlag = nttIsMsgArNTT(msg, acct.nttArEmitters, acct.nttDirectEmitters)
	if isNTT {
		return true, true, enforceFlag
	}

	return false, false, false
}

// isTokenBridgeTransfer returns true if a message is a token bridge transfer and whether or not accounting is being enforced for this emitter.
func (acct *Accountant) isTokenBridgeTransfer(msg *common.MessagePublication) (bool, bool) {
	msgId := msg.MessageIDString()

	// We only care about token bridges.
	enforceFlag, exists := acct.tokenBridges[emitterKey{emitterChainId: msg.EmitterChain, emitterAddr: msg.EmitterAddress}]
	if !exists {
		return false, false
	}

	// We only care about transfers.
	if !vaa.IsTransfer(msg.Payload) {
		acct.logger.Info("ignoring vaa because it is not a transfer", zap.String("msgID", msgId))
		return false, false
	}

	return true, enforceFlag
}

// SubmitObservation will submit token bridge transfers to the accountant smart contract. This is called from the processor
// loop when a local observation is received from a watcher. It returns true if the observation can be published immediately,
// false if not (because it has been submitted to the accountant).
func (acct *Accountant) SubmitObservation(msg *common.MessagePublication) (bool, error) {
	msgId := msg.MessageIDString()
	acct.logger.Debug("in SubmitObservation", zap.String("msgID", msgId))

	coveredByAcct, isNTT, enforceFlag := acct.isMessageCoveredByAccountant(msg)
	if !coveredByAcct {
		return true, nil
	}

	digest := msg.CreateDigest()

	acct.pendingTransfersLock.Lock()
	defer acct.pendingTransfersLock.Unlock()

	var pe *pendingEntry

	// If there is a digest mismatch, don't send it again.
	// Otherwise resubmit it and rely on the submitPending flag to prevent duplicate submissions to the contract.
	// This allows manual reobservations to proceed.
	if oldEntry, exists := acct.pendingTransfers[msgId]; exists {
		if oldEntry.digest != digest {
			digestMismatches.Inc()
			acct.logger.Error("digest in pending transfer has changed, dropping it",
				zap.String("msgID", msgId),
				zap.String("oldDigest", oldEntry.digest),
				zap.String("newDigest", digest),
				zap.Bool("enforcing", enforceFlag),
			)

			return !enforceFlag, nil
		}
		pe = oldEntry
		if acct.solanaEnabled() || acct.solanaNttEnabled() {
			added, err := pe.addSolanaSiblingTxID(msg.TxID)
			if err != nil {
				acct.logger.Error("unable to add a sibling tx id to a pending transfer", zap.String("msgID", msgId), zap.String("txID", msg.TxIDString()), zap.Error(err))
			} else if added {
				acct.logger.Info("a reobservation added a sibling tx id to a pending transfer", zap.String("msgID", msgId), zap.String("txID", msg.TxIDString()))
			}
		}
	} else {
		// Add it to the pending map and the database.
		// We only add it if it is not already present.
		// SECURITY: an error here comes from the emitted message, so it must not restart the processor.
		var err error
		if pe, err = acct.newPendingEntry(msg, msgId, digest, isNTT, enforceFlag); err != nil {
			acct.logger.Error("failed to build the pending transfer, dropping it", zap.String("msgID", msgId), zap.Bool("enforcing", enforceFlag), zap.Error(err))
			return !enforceFlag, nil
		}
		if err := acct.addPendingTransferAlreadyLocked(pe); err != nil {
			acct.logger.Error("failed to persist pending transfer, blocking publishing", zap.String("msgID", msgId), zap.Error(err))
			return false, err
		}
	}

	// This transaction may take a while. Pass it off to the worker so we don't block the processor.
	if acct.env != common.GoTest {
		tag := "accountant"
		if isNTT {
			tag = "ntt-accountant"
		}
		acct.logger.Info(fmt.Sprintf("submitting transfer to %s for approval", tag), zap.String("msgID", msgId), zap.Bool("canPublish", !enforceFlag))
		for backend := range numAccountantBackends {
			_ = acct.submitObservation(acct.ctx, pe, backend, false) // Non-blocking from processor
		}
	}

	// If we are not enforcing accountant, the event can be published. Otherwise we have to wait to hear back from the contract.
	return !enforceFlag, nil
}

// publishTransferAlreadyLocked publishes a pending transfer to the accountant channel and deletes it from the pending map. It assumes the caller holds the lock.
func (acct *Accountant) publishTransferAlreadyLocked(pe *pendingEntry) {
	if pe.enforceFlag {
		select {
		case acct.msgChan <- pe.msg:
			acct.logger.Debug("published transfer to channel", zap.String("msgId", pe.msgId))
		default:
			acct.logger.Error("unable to publish transfer because the channel is full", zap.String("msgId", pe.msgId))
		}
	}

	acct.deletePendingTransferAlreadyLocked(pe.msgId)
}

// digestLen is the length of a VAA digest and of a Solana content digest.
const digestLen = 32

// digestBytes decodes the hex digest recorded on a pending entry.
//
// SECURITY: a digest is exactly digestLen bytes.
func digestBytes(digest string) ([digestLen]byte, error) {
	var out [digestLen]byte
	raw, err := hex.DecodeString(digest)
	if err != nil {
		return out, fmt.Errorf("digest is not hex: %w", err)
	}
	if len(raw) != digestLen {
		return out, fmt.Errorf("digest: want %d bytes, got %d", digestLen, len(raw))
	}
	return [digestLen]byte(raw), nil
}

// processCommittedDigest publishes or drops a transfer that a Solana backend reports as
// committed under digest got. The VAA digest always matches. The content digest of the
// record of the reporting program family also matches. submit_observations commits that
// digest. It returns true when it publishes the transfer.
//
// SECURITY: the caller holds pendingTransfersLock.
// SECURITY: precondition: a Solana backend is enabled, so newPendingEntry set vaaDigest and the record of the family.
// SECURITY: precondition msgId != "". A violation leaves the transfer pending.
// SECURITY: a commit for an entry that family does not account is ignored. Thus a WTT commit
// cannot release or drop an NTT entry, and an NTT commit cannot release or drop a WTT entry.
func (acct *Accountant) processCommittedDigest(msgId string, got [32]byte, family solanaProgramFamily, source string) bool {
	if msgId == "" {
		acct.logger.Error("acctwatch: committed digest with an empty message id", zap.String("source", source))
		return false
	}

	pe, exists := acct.pendingTransfers[msgId]
	if !exists {
		// Guardians that submit to the Accountant after the transfer confirms emit this log.
		// submit_obs.go already processed these transfers. Thus the watcher event finds no entry in the pendingTransfers map.
		acct.logger.Info("acctwatch: unknown transfer has been approved, ignoring it", zap.String("msgId", msgId), zap.String("source", source))
		return false
	}

	record := pe.solanaRecord(family)
	if record == nil {
		acct.logger.Error("acctwatch: a solana commit names a transfer its program does not account, ignoring it", zap.String("msgId", msgId), zap.String("source", source), zap.Stringer("program", family), zap.Bool("isNTT", pe.isNTT))
		return false
	}

	if got == pe.vaaDigest || got == record.committedDigest() {
		acct.logger.Info("acctwatch: pending transfer has been approved", zap.String("msgId", msgId), zap.String("source", source))
		acct.publishTransferAlreadyLocked(pe)
		return true
	}

	digestMismatches.Inc()
	acct.logger.Error("acctwatch: digest mismatch, dropping transfer",
		zap.String("msgID", msgId),
		zap.String("source", source),
		zap.String("oldDigest", pe.digest),
		zap.String("newDigest", hex.EncodeToString(got[:])),
	)
	acct.deletePendingTransferAlreadyLocked(msgId)
	return false
}

// newPendingEntry builds a pending transfer. While a Solana backend is enabled, it decodes
// the VAA digest of every entry and builds the Solana observation record of each enabled
// backend that accounts the entry.
//
// SECURITY: postcondition: while a Solana backend is enabled, every entry has vaaDigest.
// While Solana WTT is enabled, a Token Bridge entry has solanaFields. While Solana NTT is
// enabled, an NTT entry has solanaNttFields. processCommittedDigest compares against both.
// An error comes from the message itself and occurs only while a Solana backend is enabled.
func (acct *Accountant) newPendingEntry(msg *common.MessagePublication, msgId string, digest string, isNTT bool, enforceFlag bool) (*pendingEntry, error) {
	if len(acct.solanaBackends()) == 0 {
		return &pendingEntry{msg: msg, msgId: msgId, digest: digest, isNTT: isNTT, enforceFlag: enforceFlag}, nil
	}

	vaaDigest, err := digestBytes(digest)
	if err != nil {
		return nil, err
	}
	pe := &pendingEntry{msg: msg, msgId: msgId, digest: digest, vaaDigest: vaaDigest, isNTT: isNTT, enforceFlag: enforceFlag}

	if isNTT {
		if acct.solanaNttEnabled() {
			if pe.solanaNttFields, err = acct.solanaNttObservationFieldsFromMessage(msg, vaaDigest); err != nil {
				return nil, err
			}
		}
		return pe, nil
	}

	if acct.solanaEnabled() {
		if pe.solanaFields, err = solanaObservationFieldsFromPayload(msg.EmitterChain, msg.EmitterAddress, msg.Sequence, msg.Payload, vaaDigest); err != nil {
			return nil, err
		}
	}
	return pe, nil
}

// addPendingTransferAlreadyLocked adds a pending transfer to both the map and the database. It assumes the caller holds the lock.
func (acct *Accountant) addPendingTransferAlreadyLocked(pe *pendingEntry) error {
	pe.setUpdTime()
	if err := acct.db.AcctStorePendingTransfer(pe.msg); err != nil {
		return err
	}

	acct.pendingTransfers[pe.msgId] = pe
	transfersOutstanding.Set(float64(len(acct.pendingTransfers)))
	return nil
}

// deletePendingTransfer deletes the transfer from both the map and the database. It accquires the lock.
func (acct *Accountant) deletePendingTransfer(msgId string) {
	acct.pendingTransfersLock.Lock()
	defer acct.pendingTransfersLock.Unlock()
	acct.deletePendingTransferAlreadyLocked(msgId)
}

// deletePendingTransferAlreadyLocked deletes the transfer from both the map and the database. It assumes the caller holds the lock.
func (acct *Accountant) deletePendingTransferAlreadyLocked(msgId string) {
	acct.logger.Debug("deletePendingTransfer", zap.String("msgId", msgId))
	if _, exists := acct.pendingTransfers[msgId]; exists {
		delete(acct.pendingTransfers, msgId)
		transfersOutstanding.Set(float64(len(acct.pendingTransfers)))
	}
	if err := acct.db.AcctDeletePendingTransfer(msgId); err != nil {
		acct.logger.Error("failed to delete pending transfer from the db", zap.String("msgId", msgId), zap.Error(err))
		// Ignore this error and keep going.
	}
}

// loadPendingTransfers loads any pending transfers that are present in the database. This method assumes the caller holds the lock.
func (acct *Accountant) loadPendingTransfers() error {
	pendingTransfers, err := acct.db.AcctGetData(acct.logger)
	if err != nil {
		return err
	}

	for _, msg := range pendingTransfers {
		msgId := msg.MessageIDString()
		coveredByAcct, isNTT, enforceFlag := acct.isMessageCoveredByAccountant(msg)
		if !coveredByAcct {
			acct.logger.Error("dropping reloaded pending transfer because it is not covered by the accountant", zap.String("msgID", msgId))
			if err := acct.db.AcctDeletePendingTransfer(msgId); err != nil {
				acct.logger.Error("failed to delete pending transfer from the db", zap.String("msgId", msgId), zap.Error(err))
				// Ignore this error and keep going.
			}
			continue
		}
		acct.logger.Info("reloaded pending transfer", zap.String("msgID", msgId))

		digest := msg.CreateDigest()
		pe, err := acct.newPendingEntry(msg, msgId, digest, isNTT, enforceFlag)
		if err != nil {
			acct.logger.Error("dropping reloaded pending transfer because it cannot be built", zap.String("msgID", msgId), zap.Error(err))
			if err := acct.db.AcctDeletePendingTransfer(msgId); err != nil {
				acct.logger.Error("failed to delete pending transfer from the db", zap.String("msgId", msgId), zap.Error(err))
			}
			continue
		}
		pe.setUpdTime()
		acct.pendingTransfers[msgId] = pe
	}

	transfersOutstanding.Set(float64(len(acct.pendingTransfers)))
	if len(acct.pendingTransfers) != 0 {
		acct.logger.Info("reloaded pending transfers", zap.Int("total", len(acct.pendingTransfers)))
	} else {
		acct.logger.Info("no pending transfers to be reloaded")
	}

	return nil
}

// submitObservation sends an observation request to the worker of one backend, which submits it to the contract.
// If the backend does not cover the transfer, this function returns false and does nothing.
// If the transfer already has the "submit pending" mark for that backend, this function returns false and does nothing.
// Otherwise it returns true. Use the return value to avoid unnecessary error logs.
// If blocking is false and a channel write would block, this function returns. The next audit interval handles the transfer.
// If blocking is true, it blocks until the channel has space, a timeout occurs, or the context ends.
// This function grabs the state lock.
func (acct *Accountant) submitObservation(ctx context.Context, pe *pendingEntry, backend accountantBackend, blocking bool) bool {
	subChan, tag, covered := acct.backendChannel(pe, backend)
	if !covered {
		return false
	}

	pe.stateLock.Lock()
	if pe.state.submitPending[backend] {
		pe.stateLock.Unlock()
		return false
	}
	pe.state.submitPending[backend] = true
	pe.state.updTime = time.Now()
	pe.stateLock.Unlock()

	timeout := time.Duration(0)
	if blocking {
		timeout = auditSubmitTimeout
	}
	acct.submitToChannel(ctx, pe, backend, subChan, tag, blocking, timeout)
	return true
}

// backendChannel returns the submission channel and log tag of backend for pe. The final
// return is false when the backend does not cover pe.
func (acct *Accountant) backendChannel(pe *pendingEntry, backend accountantBackend) (chan *common.MessagePublication, string, bool) {
	switch backend {
	case backendWormchain:
		if pe.isNTT {
			if acct.wormchainNttEnabled() {
				return acct.nttSubChan, "ntt-accountant", true
			}
			return nil, "", false
		}
		if acct.wormchainBaseEnabled() {
			return acct.subChan, "accountant", true
		}
	case backendSolana:
		if !pe.isNTT && acct.solanaEnabled() {
			return acct.solana.subChan, acct.solana.tag, true
		}
	case backendSolanaNTT:
		if pe.isNTT && acct.solanaNttEnabled() {
			return acct.solanaNtt.subChan, acct.solanaNtt.tag, true
		}
	}
	return nil, "", false
}

// submitToChannel submits an observation to the specified channel. If blocking is false and the channel is full,
// it clears the pending mark of the transfer for backend, so the audit resubmits it. If blocking is true, it will
// block until the channel has space, a timeout occurs, or the context is cancelled.
func (acct *Accountant) submitToChannel(ctx context.Context, pe *pendingEntry, backend accountantBackend, subChan chan *common.MessagePublication, tag string, blocking bool, timeout time.Duration) {
	msgs := []*common.MessagePublication{pe.msg}
	if backend == backendSolana || backend == backendSolanaNTT {
		// One observation for each tx id, because each tx id seeds its own pending account.
		msgs = pe.solanaSubmissionMsgs()
	}
	for _, msg := range msgs {
		if !acct.submitMsgToChannel(ctx, pe, backend, msg, subChan, tag, blocking, timeout) {
			return
		}
	}
}

// submitMsgToChannel writes one observation of pe to subChan. It returns false, and clears the
// pending mark of pe for backend, when the write fails.
func (acct *Accountant) submitMsgToChannel(ctx context.Context, pe *pendingEntry, backend accountantBackend, msg *common.MessagePublication, subChan chan *common.MessagePublication, tag string, blocking bool, timeout time.Duration) bool {
	if blocking {
		select {
		case subChan <- msg:
			acct.logger.Debug(fmt.Sprintf("submitted observation to channel for %s", tag), zap.String("msgId", pe.msgId))
			return true
		case <-time.After(timeout):
			channelSubmitTimeouts.Inc()
			acct.logger.Warn(fmt.Sprintf("timeout submitting observation to %s channel, will retry next audit", tag),
				zap.String("msgId", pe.msgId),
				zap.Duration("timeout", timeout))
			pe.setSubmitPending(backend, false)
			return false
		case <-ctx.Done():
			acct.logger.Warn(fmt.Sprintf("context cancelled while submitting to %s channel", tag), zap.String("msgId", pe.msgId))
			pe.setSubmitPending(backend, false)
			return false
		}
	}
	select {
	case subChan <- msg:
		acct.logger.Debug(fmt.Sprintf("submitted observation to channel for %s", tag), zap.String("msgId", pe.msgId))
		return true
	default:
		acct.logger.Error(fmt.Sprintf("unable to submit observation to %s because the channel is full, will try next interval", tag), zap.String("msgId", pe.msgId))
		pe.setSubmitPending(backend, false)
		return false
	}
}

// clearSubmitPendingFlags runs after the submission of a batch to backend ends, with success or failure. It clears the
// backend's submit pending flag for each entry in the batch. It grabs the pending transfer lock and the state locks.
func (acct *Accountant) clearSubmitPendingFlags(msgs []*common.MessagePublication, backend accountantBackend) {
	acct.pendingTransfersLock.Lock()
	defer acct.pendingTransfersLock.Unlock()
	for _, msg := range msgs {
		if pe, exists := acct.pendingTransfers[msg.MessageIDString()]; exists {
			pe.setSubmitPending(backend, false)
		}
	}
}

// setSubmitPending sets the submit pending flag on the pending transfer object to the specified value. It grabs the state lock.
func (pe *pendingEntry) setSubmitPending(backend accountantBackend, val bool) {
	pe.stateLock.Lock()
	defer pe.stateLock.Unlock()
	pe.state.submitPending[backend] = val
	pe.state.updTime = time.Now()
}

// submitPending returns the "submit pending" flag from the pending transfer object. It grabs the state lock.
func (pe *pendingEntry) submitPending(backend accountantBackend) bool {
	pe.stateLock.Lock()
	defer pe.stateLock.Unlock()
	return pe.state.submitPending[backend]
}

// setUpdTime sets the last update time on the pending transfer object to the current time. It grabs the state lock.
func (pe *pendingEntry) setUpdTime() {
	pe.stateLock.Lock()
	defer pe.stateLock.Unlock()
	pe.state.updTime = time.Now()
}

// updTime returns the last update time from the pending transfer object. It grabs the state lock.
func (pe *pendingEntry) updTime() time.Time {
	pe.stateLock.Lock()
	defer pe.stateLock.Unlock()
	return pe.state.updTime
}

// addSolanaSiblingTxID records txID, from a reobservation of pe.msg, as a sibling tx id. It
// returns true when it adds txID. It grabs the state lock.
//
// SECURITY: precondition: the watcher observed pe.msg in txID, and the digests match.
// SECURITY: postcondition: at most maxSolanaSiblingTxIDs siblings. pe.msg.TxID and the
// siblings are pairwise distinct.
func (pe *pendingEntry) addSolanaSiblingTxID(txID []byte) (bool, error) {
	pe.stateLock.Lock()
	defer pe.stateLock.Unlock()
	// Raw comparison first: a repeat of msg.TxID is routine, whatever its length.
	if bytes.Equal(txID, pe.msg.TxID) {
		return false, nil
	}
	id, err := newSolanaTxID(txID)
	if err != nil {
		return false, err
	}
	if slices.Contains(pe.state.solanaSiblingTxIDs, id) {
		return false, nil
	}
	if len(pe.state.solanaSiblingTxIDs) == maxSolanaSiblingTxIDs {
		return false, fmt.Errorf("the transfer already has %d sibling tx ids", maxSolanaSiblingTxIDs)
	}
	pe.state.solanaSiblingTxIDs = append(pe.state.solanaSiblingTxIDs, id)
	return true, nil
}

// solanaTxIDs returns pe.msg.TxID, then the sibling tx ids. It grabs the state lock.
func (pe *pendingEntry) solanaTxIDs() ([]solanaTxID, error) {
	first, err := newSolanaTxID(pe.msg.TxID)
	if err != nil {
		return nil, err
	}
	pe.stateLock.Lock()
	defer pe.stateLock.Unlock()
	return append([]solanaTxID{first}, pe.state.solanaSiblingTxIDs...), nil
}

// solanaSubmissionMsgs returns pe.msg, then one copy of pe.msg for each sibling tx id. It
// grabs the state lock.
func (pe *pendingEntry) solanaSubmissionMsgs() []*common.MessagePublication {
	pe.stateLock.Lock()
	defer pe.stateLock.Unlock()
	msgs := make([]*common.MessagePublication, 0, 1+len(pe.state.solanaSiblingTxIDs))
	msgs = append(msgs, pe.msg)
	for _, txID := range pe.state.solanaSiblingTxIDs {
		sibling := *pe.msg
		sibling.TxID = txID.Bytes()
		msgs = append(msgs, &sibling)
	}
	return msgs
}
