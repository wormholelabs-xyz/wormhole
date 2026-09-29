//go:build surfpool

// End-to-end test of the Solana accountant against a live surfpool. In the first scenario,
// the submission worker signs and sends an observation. The program reaches quorum. The
// watcher releases the transfer from the commit log. In the second scenario, the audit
// resolves the same commit from the closed pending account.

package accountant

import (
	"context"
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
	"go.uber.org/zap/zaptest"
)

const (
	// The guardian set the program reads. One key, so quorum is one observation.
	surfpoolGuardianSetIndex = 0

	// Core Bridge id in the program build, TEST_BRIDGE_ADDRESS in svm/accountant/justfile.
	surfpoolCoreBridgeProgramID = "worm2ZoG2kUd4vFXhvjh93UUH596ayRfgQ2MgjNMTth"

	surfpoolSequence = 0xe2e01

	surfpoolFeePayerLamports      = 20_000_000_000
	surfpoolSeededAccountLamports = 1_000_000_000
	surfpoolAccountantBatchSize   = 1

	// Budget from the submission to the commit log, covering send, confirm and the log
	// subscription. The worker polls signature statuses every solanaConfirmPollInterval.
	surfpoolCommitTimeout = 60 * time.Second

	// Budget for a step that only reads chain state.
	surfpoolReadTimeout = 20 * time.Second

	surfpoolStartTimeout = 20 * time.Second

	// Time for the supervisor tree to stop logging after the context ends.
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

// trackSolanaCounters returns an assertion on the confirmed submissions and applied
// commits since the call.
func trackSolanaCounters(t *testing.T) func(wantSubmitted float64, wantApproved float64) {
	submitted := testutil.ToFloat64(solanaTransfersSubmitted.WithLabelValues("wtt"))
	approved := testutil.ToFloat64(solanaTransfersApproved.WithLabelValues("wtt"))
	return func(wantSubmitted float64, wantApproved float64) {
		t.Helper()
		require.Equal(t, wantSubmitted, testutil.ToFloat64(solanaTransfersSubmitted.WithLabelValues("wtt"))-submitted, "confirmed submissions")
		require.Equal(t, wantApproved, testutil.ToFloat64(solanaTransfersApproved.WithLabelValues("wtt"))-approved, "applied commits")
	}
}

// TestSurfpoolSolanaAccountant drives the Solana accountant against a freshly started surfpool.
func TestSurfpoolSolanaAccountant(t *testing.T) {
	accountantELF := readProgram(t, surfpoolAccountantSOPath, "`just build-devnet` in svm/accountant")
	noreplayELF := readProgram(t, surfpoolNoreplaySOPath, "")

	h := startSurfpool(t)
	program := solana.MustPublicKeyFromBase58(surfpoolAccountantProgramID)
	noreplay := solana.MustPublicKeyFromBase58(surfpoolNoreplayProgramID)
	h.writeProgram(program, accountantELF)
	h.writeProgram(noreplay, noreplayELF)

	// The program checks the GuardianSet PDA against its build-time Core Bridge id.
	coreBridge := solana.MustPublicKeyFromBase58(surfpoolCoreBridgeProgramID)

	guardianKey := devnet.InsecureDeterministicEcdsaKeyByIndex(uint64(0))
	guardianSigner, err := guardiansigner.GenerateSignerWithPrivatekeyUnsafe(guardianKey)
	require.NoError(t, err)
	guardianAddr := ethCrypto.PubkeyToAddress(guardianKey.PublicKey)

	guardianSetPDA, err := deriveGuardianSetPDA(coreBridge, surfpoolGuardianSetIndex)
	require.NoError(t, err)
	h.setAccount(guardianSetPDA, surfpoolSeededAccountLamports, coreBridge,
		guardianSetAccountData(t, surfpoolGuardianSetIndex, [][20]byte{guardianAddr}, 0, 0))

	msg := solanaTestTransfer(t, surfpoolSequence)
	registrationPDA, err := deriveChainRegistrationPDA(program, msg.EmitterChain)
	require.NoError(t, err)
	h.setAccount(registrationPDA, surfpoolSeededAccountLamports, program,
		chainRegistrationAccountData(t, msg.EmitterChain, msg.EmitterAddress, 0))

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

	t.Run("the worker submits and the watcher releases the transfer", func(t *testing.T) {
		requireCounterDeltas := trackSolanaCounters(t)

		canPublish, err := acct.SubmitObservation(msg)
		require.NoError(t, err)
		require.False(t, canPublish, "an enforcing accountant holds the transfer until it commits")

		requirePublication(t, msgChan, msg, surfpoolCommitTimeout)
		requireCounterDeltas(1, 1)
	})

	t.Run("the audit resolves the commit from the closed pending account", func(t *testing.T) {
		requireCounterDeltas := trackSolanaCounters(t)

		// The transfer is accounted and its pending account is closed. Thus the audit must
		// resolve the digest from the signatures of the closed account.
		insertPendingTransfer(t, acct, msg)
		acct.runAudit(rootCtx)

		requirePublication(t, msgChan, msg, surfpoolReadTimeout)
		requireCounterDeltas(0, 1)
	})
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
