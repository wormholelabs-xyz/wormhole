//go:build surfpool

// End-to-end test of the Solana NTT accountant against a live surfpool: the submission
// worker resolves the NTT route, signs and sends an observation, the program reaches
// quorum, and the watcher releases the transfer from the commit log. The second scenario
// drives the audit.

package accountant

import (
	"context"
	"encoding/binary"
	"math/big"
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
	"github.com/stretchr/testify/require"
	"github.com/wormhole-foundation/wormhole/sdk/vaa"
	"go.uber.org/zap/zaptest"
)

const (
	// TEST_NTT_GLOBAL_ACCOUNTANT_PROGRAM_ID in svm/accountant/justfile.
	surfpoolNttAccountantProgramID = "cGfHiC6Kgg3FpFZvgwGcswsCRtp4aBP2fzuXRQPizuN"
	// Relative to node/pkg/accountant.
	surfpoolNttAccountantSOPath = "../../../svm/accountant/target/deploy/ntt_global_accountant.so"

	// Devnet NTT transceivers, ntt_config.go. The Ethereum one is the locking hub.
	surfpoolNttEthereumTransceiver = "000000000000000000000000855FA758c77D68a04990E992aA4dcdeF899F654A"
	surfpoolNttBscTransceiver      = "000000000000000000000000fA2435Eacf10Ca62ae6787ba2fB044f8733Ee843"

	// Both land in one NoReplay bucket, as in the WTT test.
	surfpoolNttWorkerScenarioSequence = 0xe3e01
	surfpoolNttAuditScenarioSequence  = 0xe3e02

	// Eight trimmed decimals, so the program books the amount unscaled.
	surfpoolNttTrimmedDecimals = 8
	surfpoolNttTrimmedAmount   = 1_250_000
)

// transceiverHubData is a TransceiverHubLayout body.
func transceiverHubData(chain vaa.ChainID, address vaa.Address, hub solanaNttHub) []byte {
	return mustEncodeWire(&transceiverHubWire{Tag: transceiverHubTag, Chain: uint16(chain), HubChain: uint16(hub.Chain), Address: address, HubAddress: hub.Address})
}

// transceiverPeerData is a TransceiverPeerLayout body.
func transceiverPeerData(chain vaa.ChainID, address vaa.Address, destChain vaa.ChainID, peer vaa.Address) []byte {
	return mustEncodeWire(&transceiverPeerWire{Tag: transceiverPeerTag, Chain: uint16(chain), DestChain: uint16(destChain), Address: address, PeerAddress: peer})
}

// TestSurfpoolSolanaNttAccountant drives the Solana NTT accountant against a freshly started surfpool.
func TestSurfpoolSolanaNttAccountant(t *testing.T) {
	accountantELF := readProgram(t, surfpoolNttAccountantSOPath, "`just build-devnet` in svm/accountant")
	noreplayELF := readProgram(t, surfpoolNoreplaySOPath, "")

	h := startSurfpool(t)
	program := solana.MustPublicKeyFromBase58(surfpoolNttAccountantProgramID)
	noreplay := solana.MustPublicKeyFromBase58(surfpoolNoreplayProgramID)
	h.writeProgram(program, accountantELF)
	h.writeProgram(noreplay, noreplayELF)
	coreBridge := solana.MustPublicKeyFromBase58(surfpoolCoreBridgeProgramID)

	guardianKey := devnet.InsecureDeterministicEcdsaKeyByIndex(uint64(0))
	guardianSigner, err := guardiansigner.GenerateSignerWithPrivatekeyUnsafe(guardianKey)
	require.NoError(t, err)
	guardianAddr := ethCrypto.PubkeyToAddress(guardianKey.PublicKey)

	guardianSetPDA, err := deriveGuardianSetPDA(coreBridge, surfpoolGuardianSetIndex)
	require.NoError(t, err)
	h.setAccount(guardianSetPDA, surfpoolSeededAccountLamports, coreBridge,
		guardianSetAccountData(t, surfpoolGuardianSetIndex, [][20]byte{guardianAddr}, 0, 0))

	// The Ethereum transceiver is a hub that names itself, cross-registered with the BSC one.
	// Balances key on the sender's hub, so only the Ethereum side carries one.
	ethereum, err := vaa.StringToAddress(surfpoolNttEthereumTransceiver)
	require.NoError(t, err)
	bsc, err := vaa.StringToAddress(surfpoolNttBscTransceiver)
	require.NoError(t, err)
	hubPDA, err := deriveTransceiverHubPDA(program, vaa.ChainIDEthereum, ethereum)
	require.NoError(t, err)
	h.setAccount(hubPDA, surfpoolSeededAccountLamports, program,
		transceiverHubData(vaa.ChainIDEthereum, ethereum, solanaNttHub{Chain: vaa.ChainIDEthereum, Address: ethereum}))
	peerSrcPDA, err := deriveTransceiverPeerPDA(program, vaa.ChainIDEthereum, ethereum, vaa.ChainIDBSC)
	require.NoError(t, err)
	h.setAccount(peerSrcPDA, surfpoolSeededAccountLamports, program,
		transceiverPeerData(vaa.ChainIDEthereum, ethereum, vaa.ChainIDBSC, bsc))
	peerDstPDA, err := deriveTransceiverPeerPDA(program, vaa.ChainIDBSC, bsc, vaa.ChainIDEthereum)
	require.NoError(t, err)
	h.setAccount(peerDstPDA, surfpoolSeededAccountLamports, program,
		transceiverPeerData(vaa.ChainIDBSC, bsc, vaa.ChainIDEthereum, ethereum))

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
		"",   // wormchain NTT contract
		nil,
		AccountantSolanaConfig{
			Conn:       conn,
			NttProgram: program,
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
		t.Fatalf("acctsolnttwatcher did not subscribe to the program logs within %v", surfpoolStartTimeout)
	}
	require.True(t, acct.solanaNttEnabled())
	require.False(t, acct.solanaEnabled())

	authority, err := deriveNoreplayAuthorityPDA(program)
	require.NoError(t, err)
	hub := solanaNttHub{Chain: vaa.ChainIDEthereum, Address: ethereum}

	t.Run("the worker submits and the watcher releases the transfer", func(t *testing.T) {
		msg := surfpoolNttTransfer(t, ethereum, surfpoolNttWorkerScenarioSequence)
		requireCounterDeltas := trackSolanaCounters(t, solanaFamilyNTT)

		canPublish, err := acct.SubmitObservation(msg)
		require.NoError(t, err)
		require.False(t, canPublish, "an enforcing accountant holds the transfer until it commits")

		requirePublication(t, msgChan, msg, surfpoolCommitTimeout)
		requireNttCommitOnChain(t, rootCtx, acct, conn, program, noreplay, authority, msg)
		requireNttBalances(t, rootCtx, conn, program, hub, 1)
		requireCounterDeltas(1, 1)
	})

	t.Run("the audit resubmits and then resolves the commit", func(t *testing.T) {
		msg := surfpoolNttTransfer(t, ethereum, surfpoolNttAuditScenarioSequence)
		requireCounterDeltas := trackSolanaCounters(t, solanaFamilyNTT)

		// The audit is the only path that reaches the submission channel here.
		insertNttPendingTransfer(t, acct, msg)
		acct.runAudit(rootCtx)

		requirePublication(t, msgChan, msg, surfpoolCommitTimeout)
		requireNttCommitOnChain(t, rootCtx, acct, conn, program, noreplay, authority, msg)
		requireNttBalances(t, rootCtx, conn, program, hub, 2)
		requireCounterDeltas(1, 1)

		// The transfer is accounted and its pending account is closed, so this cycle has
		// to resolve the digest from the closed account's signatures.
		requireCounterDeltas = trackSolanaCounters(t, solanaFamilyNTT)
		insertNttPendingTransfer(t, acct, msg)
		acct.runAudit(rootCtx)

		requirePublication(t, msgChan, msg, surfpoolReadTimeout)
		requireCounterDeltas(0, 1)
	})
}

// surfpoolNttTransfer is a direct NTT transfer from the Ethereum transceiver to BSC.
func surfpoolNttTransfer(t *testing.T, transceiver vaa.Address, sequence uint64) *common.MessagePublication {
	t.Helper()
	txID := make([]byte, digestLen)
	binary.BigEndian.PutUint64(txID[digestLen-8:], sequence)

	return &common.MessagePublication{
		TxID:             txID,
		Timestamp:        time.Unix(int64(1654543099), 0),
		Nonce:            uint32(1),
		Sequence:         sequence,
		EmitterChain:     vaa.ChainIDEthereum,
		EmitterAddress:   transceiver,
		ConsistencyLevel: uint8(32),
		Payload:          nttTestTransferPayload(surfpoolNttTrimmedDecimals, surfpoolNttTrimmedAmount, vaa.ChainIDBSC, nil, nil),
	}
}

// insertNttPendingTransfer adds one NTT pending entry straight to the map.
func insertNttPendingTransfer(t *testing.T, acct *Accountant, msg *common.MessagePublication) {
	t.Helper()
	pe, err := acct.newPendingEntry(msg, msg.MessageIDString(), msg.CreateDigest(), true, true)
	require.NoError(t, err)
	require.NotNil(t, pe.solanaNttFields)

	acct.pendingTransfersLock.Lock()
	defer acct.pendingTransfersLock.Unlock()
	require.NoError(t, acct.addPendingTransferAlreadyLocked(pe))
}

// requireNttCommitOnChain asserts the NoReplay bit is set and one transaction created and
// closed the NTT pending account.
func requireNttCommitOnChain(t *testing.T, ctx context.Context, acct *Accountant, conn solacctconn.Conn, program solana.PublicKey, noreplay solana.PublicKey, authority solana.PublicKey, msg *common.MessagePublication) {
	t.Helper()
	vaaDigest, err := digestBytes(msg.CreateDigest())
	require.NoError(t, err)
	fields, err := acct.solanaNttObservationFieldsFromMessage(msg, vaaDigest)
	require.NoError(t, err)

	bucket, err := deriveNoreplayBucketPDA(noreplay, authority, fields.Chain, fields.Emitter, fields.Sequence)
	require.NoError(t, err)
	accounts, err := conn.GetOwnedAccounts(ctx, []solana.PublicKey{bucket}, noreplay, solacctconn.CommitmentFinalized)
	require.NoError(t, err)
	require.Len(t, accounts, 1)
	require.Equal(t, solacctconn.AccountInitialised, accounts[0].State, "the noreplay bucket exists after quorum")
	marked, err := noreplayBitSet(accounts[0].Data, fields.Sequence)
	require.NoError(t, err)
	require.True(t, marked, "the noreplay bit of sequence %d is set", fields.Sequence)

	pending, err := derivePendingObservationsPDA(program, fields.Chain, fields.Emitter, fields.Sequence, surfpoolGuardianSetIndex, fields.contentDigest)
	require.NoError(t, err)
	var sigs []solacctconn.SignatureEntry
	require.Eventually(t, func() bool {
		var err error
		sigs, err = conn.GetSignaturesForAddress(ctx, pending, solana.Signature{}, getSignaturesForAddressPageLength)
		return err == nil && len(sigs) != 0
	}, surfpoolReadTimeout, surfpoolReadyPoll, "the commit transaction is indexed")
	// One transaction created the pending account, reached quorum and closed it.
	require.Len(t, sigs, 1)
	t.Logf("commit transaction for %s: %s", msg.MessageIDString(), sigs[0].Signature)
}

// requireNttBalances asserts the hub token balances after `transfers` committed transfers
// Ethereum -> BSC: the hub chain locks and BSC mints the same normalized amount.
func requireNttBalances(t *testing.T, ctx context.Context, conn solacctconn.Conn, program solana.PublicKey, hub solanaNttHub, transfers uint64) {
	t.Helper()
	source, err := deriveBalanceAccountPDA(program, vaa.ChainIDEthereum, hub.Chain, hub.Address)
	require.NoError(t, err)
	dest, err := deriveBalanceAccountPDA(program, vaa.ChainIDBSC, hub.Chain, hub.Address)
	require.NoError(t, err)
	accounts, err := conn.GetOwnedAccounts(ctx, []solana.PublicKey{source, dest}, program, solacctconn.CommitmentConfirmed)
	require.NoError(t, err)
	require.Len(t, accounts, 2)

	want := new(big.Int).SetUint64(transfers * surfpoolNttTrimmedAmount)
	for idx, chain := range []vaa.ChainID{vaa.ChainIDEthereum, vaa.ChainIDBSC} {
		require.Equal(t, solacctconn.AccountInitialised, accounts[idx].State, "balance account on %s", chain)
		var balance balanceAccountWire
		require.NoError(t, decodeWire(accounts[idx].Data, &balance))
		require.Equal(t, uint8(balanceAccountTag), balance.Tag)
		require.Equal(t, uint16(chain), balance.Chain)
		require.Equal(t, uint16(hub.Chain), balance.TokenChain)
		require.Equal(t, hub.Address, vaa.Address(balance.TokenAddress))
		got := new(big.Int).SetBytes(balance.Balance[:])
		require.Zero(t, want.Cmp(got), "balance on %s: want %s, got %s", chain, want, got)
	}
}
