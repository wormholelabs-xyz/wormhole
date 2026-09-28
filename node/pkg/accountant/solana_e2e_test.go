//go:build surfpool

// End-to-end test of the Solana accountant against a live surfpool: the submission
// worker signs and sends an observation, the program reaches quorum, and the watcher
// releases the transfer from the commit log. The second scenario drives the audit.

package accountant

import (
	"context"
	"encoding/binary"
	"sync"
	"testing"
	"time"

	"github.com/certusone/wormhole/node/pkg/common"
	guardianDB "github.com/certusone/wormhole/node/pkg/db"
	"github.com/certusone/wormhole/node/pkg/devnet"
	"github.com/certusone/wormhole/node/pkg/guardiansigner"
	gossipv1 "github.com/certusone/wormhole/node/pkg/proto/gossip/v1"
	"github.com/certusone/wormhole/node/pkg/solacctconn"
	"github.com/certusone/wormhole/node/pkg/supervisor"
	ethCommon "github.com/ethereum/go-ethereum/common"
	ethCrypto "github.com/ethereum/go-ethereum/crypto"
	"github.com/gagliardetto/solana-go"
	"github.com/prometheus/client_golang/prometheus/testutil"
	"github.com/stretchr/testify/require"
	"github.com/wormhole-foundation/wormhole/sdk/vaa"
	"go.uber.org/zap/zaptest"
)

const (
	// The guardian set the program reads. One key, so quorum is one observation.
	surfpoolGuardianSetIndex = 0

	// Devnet Token Bridge emitter on Ethereum, sdk/devnet_consts.go.
	surfpoolEmitterChain       = vaa.ChainIDEthereum
	surfpoolTokenBridgeEmitter = "0000000000000000000000000290fb167208af455bb137780163b7b7a9a10c16"

	// Core Bridge the program is built against, TEST_BRIDGE_ADDRESS in svm/accountant/justfile.
	surfpoolCoreBridgeProgramID = "worm2ZoG2kUd4vFXhvjh93UUH596ayRfgQ2MgjNMTth"

	// Both land in one NoReplay bucket, so the audit scenario reads an existing bucket
	// with its own bit clear.
	surfpoolWorkerScenarioSequence = 0xe2e01
	surfpoolAuditScenarioSequence  = 0xe2e02

	surfpoolFeePayerLamports      = 20_000_000_000
	surfpoolSeededAccountLamports = 1_000_000_000
	surfpoolAccountantBatchSize   = 1

	// Budget from the submission to the commit log, covering send, confirm and the log
	// subscription. The worker polls signature statuses every solanaConfirmPollInterval.
	surfpoolCommitTimeout = 60 * time.Second

	// Budget for a step that only reads chain state.
	surfpoolReadTimeout = 20 * time.Second

	surfpoolStartTimeout = 20 * time.Second

	surfpoolWaitPoll     = 250 * time.Millisecond
	surfpoolMaxWaitPolls = 1000

	// Time for the supervisor tree to stop logging after the context is cancelled.
	surfpoolShutdownDrain = 500 * time.Millisecond
)

// subscriptionSignalingConn closes subscribed after the first SubscribeLogs call the
// server acknowledges.
type subscriptionSignalingConn struct {
	solacctconn.Conn
	once       sync.Once
	subscribed chan struct{}
}

func (c *subscriptionSignalingConn) SubscribeLogs(ctx context.Context, program solana.PublicKey) (<-chan solacctconn.LogEvent, error) {
	events, err := c.Conn.SubscribeLogs(ctx, program)
	if err == nil {
		c.once.Do(func() { close(c.subscribed) })
	}
	return events, err
}

// solanaCounters is a snapshot of the counters that tell the release paths apart.
type solanaCounters struct {
	submitted float64
	approved  float64
}

func readSolanaCounters() solanaCounters {
	return solanaCounters{
		submitted: testutil.ToFloat64(solanaTransfersSubmitted),
		approved:  testutil.ToFloat64(solanaTransfersApproved),
	}
}

// requireCounterDeltas asserts how many confirmed submissions and applied commits
// happened since before.
func requireCounterDeltas(t *testing.T, before solanaCounters, wantSubmitted float64, wantApproved float64) {
	t.Helper()
	after := readSolanaCounters()
	require.Equal(t, wantSubmitted, after.submitted-before.submitted, "confirmed submissions")
	require.Equal(t, wantApproved, after.approved-before.approved, "applied commits")
}

// TestSurfpoolSolanaAccountant drives the Solana accountant against a freshly started surfpool.
func TestSurfpoolSolanaAccountant(t *testing.T) {
	accountantELF := readBuiltProgram(t, surfpoolAccountantSOPath, "`just build-devnet` in svm/accountant")
	noreplayELF := readCommittedProgram(t, surfpoolNoreplaySOPath)

	h := startSurfpool(t)
	program := solana.MustPublicKeyFromBase58(surfpoolAccountantProgramID)
	noreplay := solana.MustPublicKeyFromBase58(surfpoolNoreplayProgramID)
	h.writeProgram(program, accountantELF)
	h.writeProgram(noreplay, noreplayELF)

	// The program verifies the GuardianSet PDA against its build-time Core Bridge id.
	coreBridge := solana.MustPublicKeyFromBase58(surfpoolCoreBridgeProgramID)

	guardianKey := devnet.InsecureDeterministicEcdsaKeyByIndex(uint64(0))
	guardianSigner, err := guardiansigner.GenerateSignerWithPrivatekeyUnsafe(guardianKey)
	require.NoError(t, err)
	guardianAddr := ethCrypto.PubkeyToAddress(guardianKey.PublicKey)

	guardianSetPDA, err := deriveGuardianSetPDA(coreBridge, surfpoolGuardianSetIndex)
	require.NoError(t, err)
	h.setAccount(guardianSetPDA, surfpoolSeededAccountLamports, coreBridge,
		guardianSetAccountData(t, surfpoolGuardianSetIndex, [][20]byte{guardianAddr}, 0, 0))

	emitter, err := vaa.StringToAddress(surfpoolTokenBridgeEmitter)
	require.NoError(t, err)
	registrationPDA, err := deriveChainRegistrationPDA(program, surfpoolEmitterChain)
	require.NoError(t, err)
	h.setAccount(registrationPDA, surfpoolSeededAccountLamports, program,
		chainRegistrationAccountData(t, surfpoolEmitterChain, emitter, 0))

	feePayer, err := solana.NewRandomPrivateKey()
	require.NoError(t, err)
	h.setAccount(feePayer.PublicKey(), surfpoolFeePayerLamports, solana.SystemProgramID, nil)

	clientConn, err := solacctconn.NewConn(h.rpcURL, h.wsURL)
	require.NoError(t, err)
	t.Cleanup(clientConn.Close)
	conn := &subscriptionSignalingConn{Conn: clientConn, subscribed: make(chan struct{})}

	gst := common.NewGuardianSetState(nil)
	gst.Set(&common.GuardianSet{Index: surfpoolGuardianSetIndex, Keys: []ethCommon.Address{guardianAddr}})

	msgChan := make(chan *common.MessagePublication, MsgChannelCapacity)
	obsvReqC := make(chan *gossipv1.ObservationRequest, 10)
	logger := zaptest.NewLogger(t)

	rootCtx, cancel := context.WithCancel(context.Background())
	defer func() {
		cancel()
		time.Sleep(surfpoolShutdownDrain)
	}()

	acct := NewAccountant(
		rootCtx,
		logger,
		&guardianDB.MockAccountantDB{},
		obsvReqC,
		"", // wormchain contract
		"", // wormchain websocket
		nil,
		true, // enforcing
		"",   // NTT contract
		nil,
		AccountantSolanaConfig{
			Conn:       conn,
			Program:    program,
			Noreplay:   noreplay,
			CoreBridge: coreBridge,
			FeePayer:   feePayer,
		},
		guardianSigner,
		gst,
		msgChan,
		surfpoolAccountantBatchSize,
		common.UnsafeDevNet,
	)

	startErr := make(chan error, 1)
	supervisor.New(rootCtx, logger, func(ctx context.Context) error {
		if err := acct.Start(ctx); err != nil {
			select {
			case startErr <- err:
			default:
			}
			return err
		}
		supervisor.Signal(ctx, supervisor.SignalHealthy)
		<-ctx.Done()
		return nil
	})

	select {
	case <-conn.subscribed:
	case err := <-startErr:
		t.Fatalf("the accountant failed to start: %v", err)
	case <-time.After(surfpoolStartTimeout):
		t.Fatalf("acctsolwatcher did not subscribe to the program logs within %v", surfpoolStartTimeout)
	}

	authority, err := deriveNoreplayAuthorityPDA(program)
	require.NoError(t, err)

	t.Run("the worker submits and the watcher releases the transfer", func(t *testing.T) {
		msg := surfpoolTokenBridgeTransfer(t, surfpoolWorkerScenarioSequence)
		before := readSolanaCounters()

		canPublish, err := acct.SubmitObservation(msg)
		require.NoError(t, err)
		require.False(t, canPublish, "an enforcing accountant holds the transfer until it commits")

		requirePublication(t, msgChan, msg, surfpoolCommitTimeout)
		requirePendingEmpty(t, acct)
		requireCommitOnChain(t, rootCtx, conn, program, noreplay, authority, msg)
		requireCounterDeltas(t, before, 1, 1)
	})

	t.Run("the audit resubmits and then resolves the commit", func(t *testing.T) {
		msg := surfpoolTokenBridgeTransfer(t, surfpoolAuditScenarioSequence)
		before := readSolanaCounters()

		// The audit is the only path that reaches the submission channel here.
		insertPendingTransfer(t, acct, msg)
		acct.runAudit(rootCtx)

		requirePublication(t, msgChan, msg, surfpoolCommitTimeout)
		requirePendingEmpty(t, acct)
		requireCommitOnChain(t, rootCtx, conn, program, noreplay, authority, msg)
		requireCounterDeltas(t, before, 1, 1)

		// The transfer is accounted and its pending account is closed, so this cycle has
		// to resolve the digest from the closed account's signatures.
		before = readSolanaCounters()
		insertPendingTransfer(t, acct, msg)
		acct.runAudit(rootCtx)

		requirePublication(t, msgChan, msg, surfpoolReadTimeout)
		requirePendingEmpty(t, acct)
		requireCounterDeltas(t, before, 0, 1)
	})
}

// surfpoolTokenBridgeTransfer is a Token Bridge transfer of an Ethereum-native token from
// Ethereum to Polygon. The source balance rises and the destination balance rises, so
// both balance accounts start from zero.
func surfpoolTokenBridgeTransfer(t *testing.T, sequence uint64) *common.MessagePublication {
	t.Helper()
	emitterAddr, err := vaa.StringToAddress(surfpoolTokenBridgeEmitter)
	require.NoError(t, err)

	txID := make([]byte, digestLen)
	binary.BigEndian.PutUint64(txID[digestLen-8:], sequence)

	// buildMockTransferPayloadBytes stops at the recipient chain; the 32-byte fee
	// completes the 133-byte TokenBridgeTransfer the Solana program parses.
	payload := append(buildMockTransferPayloadBytes(1,
		vaa.ChainIDEthereum,
		"0x707f9118e33a9b8998bea41dd0d46f38bb963fc8",
		vaa.ChainIDPolygon,
		"0x707f9118e33a9b8998bea41dd0d46f38bb963fc8",
		1.25,
	), make([]byte, 32)...)
	require.Len(t, payload, tokenBridgeTransferLen)

	return &common.MessagePublication{
		TxID:             txID,
		Timestamp:        time.Unix(int64(1654543099), 0),
		Nonce:            uint32(1),
		Sequence:         sequence,
		EmitterChain:     surfpoolEmitterChain,
		EmitterAddress:   emitterAddr,
		ConsistencyLevel: uint8(32),
		Payload:          payload,
	}
}

// surfpoolObservationFields is the record the program hashes for msg.
func surfpoolObservationFields(t *testing.T, msg *common.MessagePublication) *solanaObservationFields {
	t.Helper()
	vaaDigest, err := digestBytes(msg.CreateDigest())
	require.NoError(t, err)
	fields, err := solanaObservationFieldsFromPayload(msg.EmitterChain, msg.EmitterAddress, msg.Sequence, msg.Payload, vaaDigest)
	require.NoError(t, err)
	require.NotNil(t, fields)
	return fields
}

// insertPendingTransfer adds one pending entry straight to the map.
func insertPendingTransfer(t *testing.T, acct *Accountant, msg *common.MessagePublication) {
	t.Helper()
	pe, err := acct.newPendingEntry(msg, msg.MessageIDString(), msg.CreateDigest(), false, true)
	require.NoError(t, err)
	require.NotNil(t, pe.solanaFields)

	acct.pendingTransfersLock.Lock()
	defer acct.pendingTransfersLock.Unlock()
	require.NoError(t, acct.addPendingTransferAlreadyLocked(pe))
}

// requirePublication waits for msg on the accountant's publication channel.
func requirePublication(t *testing.T, msgChan chan *common.MessagePublication, msg *common.MessagePublication, timeout time.Duration) {
	t.Helper()
	select {
	case published := <-msgChan:
		require.Equal(t, msg.MessageIDString(), published.MessageIDString())
	case <-time.After(timeout):
		t.Fatalf("no publication for %s within %v", msg.MessageIDString(), timeout)
	}
}

// requirePendingEmpty waits for the pending transfer map to drain.
func requirePendingEmpty(t *testing.T, acct *Accountant) {
	t.Helper()
	waitUntil(t, "the pending transfer map to empty", surfpoolReadTimeout, func() bool {
		acct.pendingTransfersLock.Lock()
		defer acct.pendingTransfersLock.Unlock()
		return len(acct.pendingTransfers) == 0
	})
}

// requireCommitOnChain asserts the chain state the quorum-closing transaction leaves: the
// NoReplay bit is set and the pending account was created and closed by exactly one
// transaction.
func requireCommitOnChain(t *testing.T, ctx context.Context, conn solacctconn.Conn, program solana.PublicKey, noreplay solana.PublicKey, authority solana.PublicKey, msg *common.MessagePublication) {
	t.Helper()
	fields := surfpoolObservationFields(t, msg)

	bucket, err := deriveNoreplayBucketPDA(noreplay, authority, fields.Chain, fields.Emitter, fields.Sequence)
	require.NoError(t, err)
	accounts, err := conn.GetMultipleAccounts(ctx, []solana.PublicKey{bucket}, solacctconn.CommitmentFinalized)
	require.NoError(t, err)
	require.Len(t, accounts, 1)
	require.NotNil(t, accounts[0], "the noreplay bucket exists after quorum")
	marked, err := noreplayBitSet(accounts[0].Data, fields.Sequence)
	require.NoError(t, err)
	require.True(t, marked, "the noreplay bit of sequence %d is set", fields.Sequence)

	pending, err := derivePendingObservationsPDA(program, fields.Chain, fields.Emitter, fields.Sequence, surfpoolGuardianSetIndex, fields.contentDigest)
	require.NoError(t, err)

	var sigs []solana.Signature
	waitUntil(t, "the commit transaction to be indexed", surfpoolReadTimeout, func() bool {
		var err error
		sigs, err = conn.GetSignaturesForAddress(ctx, pending, maxSolanaCommitSearchSignatures)
		return err == nil && len(sigs) != 0
	})
	// One transaction created the pending account, reached quorum and closed it.
	require.Len(t, sigs, 1)
	t.Logf("commit transaction for %s: %s", msg.MessageIDString(), sigs[0])
}

// waitUntil polls cond until it holds, the timeout passes, or the poll budget runs out.
func waitUntil(t *testing.T, what string, timeout time.Duration, cond func() bool) {
	t.Helper()
	deadline := time.Now().Add(timeout)
	for poll := 0; poll < surfpoolMaxWaitPolls; poll++ {
		if cond() {
			return
		}
		if time.Now().After(deadline) {
			break
		}
		time.Sleep(surfpoolWaitPoll)
	}
	t.Fatalf("timed out after %v waiting for %s", timeout, what)
}
