package accountant

import (
	"context"
	"encoding/binary"
	"errors"
	"testing"
	"time"

	"github.com/certusone/wormhole/node/pkg/common"
	gossipv1 "github.com/certusone/wormhole/node/pkg/proto/gossip/v1"
	"github.com/certusone/wormhole/node/pkg/solacctconn"
	ethCrypto "github.com/ethereum/go-ethereum/crypto"
	"github.com/gagliardetto/solana-go"
	"github.com/prometheus/client_golang/prometheus/testutil"
	"github.com/stretchr/testify/assert"
	"github.com/stretchr/testify/require"
	"github.com/wormhole-foundation/wormhole/sdk/vaa"
)

// Devnet NTT transceiver and Standard Relayer on Ethereum, ntt_config.go and sdk/devnet_consts.go.
const (
	testNttTransceiverHex = "000000000000000000000000855FA758c77D68a04990E992aA4dcdeF899F654A"
	testNttRelayerHex     = "000000000000000000000000b98F46E96cb1F519C333FdFB5CCe0B13E0300ED4"
	testNttTrimmedAmount  = uint64(1_250_000)
)

// testNttDeliveryInstruction wraps payload from sender, in the layout nttParseArPayload reads.
func testNttDeliveryInstruction(sender vaa.Address, payload []byte) []byte {
	out := []byte{PAYLOAD_ID_DELIVERY_INSTRUCTION}
	out = binary.BigEndian.AppendUint16(out, uint16(vaa.ChainIDBSC))
	out = append(out, make([]byte, 32)...)                         // target address
	out = binary.BigEndian.AppendUint32(out, uint32(len(payload))) // #nosec G115 -- test sizes
	out = append(out, payload...)
	out = append(out, make([]byte, 64)...) // requested and extra receiver value
	out = binary.BigEndian.AppendUint32(out, 0)
	out = binary.BigEndian.AppendUint16(out, uint16(vaa.ChainIDEthereum))
	out = append(out, make([]byte, 96)...) // refund address, refund and source delivery provider
	out = append(out, sender[:]...)
	return append(out, 0) // num_message_keys
}

// solanaTestNttTransfer is a devnet NTT transfer Ethereum -> BSC. relayed wraps it in a
// Standard Relayer delivery.
func solanaTestNttTransfer(t *testing.T, sequence uint64, relayed bool) *common.MessagePublication {
	t.Helper()
	transceiver := mustAddress(t, testNttTransceiverHex)
	payload := nttTestTransferPayload(8, testNttTrimmedAmount, vaa.ChainIDBSC, nil, nil)
	emitter := transceiver
	if relayed {
		emitter = mustAddress(t, testNttRelayerHex)
		payload = testNttDeliveryInstruction(transceiver, payload)
	}
	return &common.MessagePublication{
		TxID:             hashToTxID("0x16f541f5ecfc43407c31587aa6ac3a689e8960f36dc23c332db5510dfc6a4063"),
		Timestamp:        time.Unix(int64(1654543099), 0),
		Nonce:            uint32(1),
		Sequence:         sequence,
		EmitterChain:     vaa.ChainIDEthereum,
		EmitterAddress:   emitter,
		ConsistencyLevel: uint8(32),
		Payload:          payload,
	}
}

func TestNttResolveSender(t *testing.T) {
	ctx := context.Background()
	acct, _, _ := newSolanaTestAccountant(t, ctx, solanaTestOpts{wormchainContract: "0xdeadbeef", enforce: true, solanaNtt: true})
	transceiver := mustAddress(t, testNttTransceiverHex)
	relayer := mustAddress(t, testNttRelayerHex)
	transfer := nttTestTransferPayload(8, testNttTrimmedAmount, vaa.ChainIDBSC, nil, nil)

	tests := []struct {
		name       string
		emitter    vaa.Address
		payload    []byte
		wantSender vaa.Address
		wantErr    bool
	}{
		{name: "direct transceiver", emitter: transceiver, payload: transfer, wantSender: transceiver},
		{name: "relayed transceiver", emitter: relayer, payload: testNttDeliveryInstruction(transceiver, transfer), wantSender: transceiver},
		{name: "relayer names itself as the sender", emitter: relayer, payload: testNttDeliveryInstruction(relayer, transfer), wantErr: true},
		{name: "malformed delivery instruction", emitter: relayer, payload: []byte{PAYLOAD_ID_DELIVERY_INSTRUCTION}, wantErr: true},
		{name: "emitter is neither transceiver nor relayer", emitter: vaa.Address{0x01}, payload: transfer, wantErr: true},
	}
	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			msg := &common.MessagePublication{EmitterChain: vaa.ChainIDEthereum, EmitterAddress: tt.emitter, Payload: tt.payload}
			sender, message, err := acct.nttResolveSender(msg)
			if tt.wantErr {
				require.Error(t, err)
				return
			}
			require.NoError(t, err)
			assert.Equal(t, tt.wantSender, sender)
			assert.Equal(t, transfer, message)
		})
	}
}

func TestSolanaNttRecordOnPendingEntry(t *testing.T) {
	ctx := context.Background()
	transceiver := mustAddress(t, testNttTransceiverHex)

	tests := []struct {
		name       string
		relayed    bool
		solanaNtt  bool
		wantRecord bool
	}{
		{name: "direct", solanaNtt: true, wantRecord: true},
		{name: "relayed", relayed: true, solanaNtt: true, wantRecord: true},
		{name: "wormchain ntt only builds no solana record"},
	}
	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			opts := solanaTestOpts{wormchainContract: "0xdeadbeef", enforce: true, solanaNtt: tt.solanaNtt}
			if !tt.solanaNtt {
				opts.nttContract = "0xfeed"
			}
			acct, _, _ := newSolanaTestAccountant(t, ctx, opts)
			msg := solanaTestNttTransfer(t, 3, tt.relayed)
			shouldPub, err := acct.SubmitObservation(msg)
			require.NoError(t, err)
			assert.False(t, shouldPub)

			pe := acct.pendingTransfers[msg.MessageIDString()]
			require.NotNil(t, pe)
			require.True(t, pe.isNTT)
			assert.Nil(t, pe.solanaFields)
			if !tt.wantRecord {
				assert.Nil(t, pe.solanaNttFields)
				assert.Nil(t, pe.solanaRecord(solanaFamilyNTT))
				return
			}

			f := pe.solanaNttFields
			require.NotNil(t, f)
			assert.Equal(t, msg.EmitterChain, f.Chain)
			assert.Equal(t, msg.EmitterAddress, f.Emitter)
			assert.Equal(t, msg.Sequence, f.Sequence)
			assert.Equal(t, transceiver, f.Sender)
			assert.Equal(t, tt.relayed, f.Sender != f.Emitter)
			assert.Equal(t, vaa.ChainIDBSC, f.RecipientChain)
			assert.Equal(t, uint8(8), f.TrimmedDecimals)
			assert.Equal(t, testNttTrimmedAmount, f.TrimmedAmount)
			assert.Equal(t, pe.vaaDigest, f.VaaDigest)
			assert.Equal(t, mustContentDigest(t, f), f.contentDigest)
			assert.Same(t, f, pe.solanaRecord(solanaFamilyNTT))
			assert.Nil(t, pe.solanaRecord(solanaFamilyWTT))
		})
	}
}

func TestMalformedNttTransferLeavesNoEntry(t *testing.T) {
	ctx := context.Background()
	for _, enforce := range []bool{true, false} {
		t.Run(map[bool]string{true: "accountant enforcing", false: "accountant log only"}[enforce], func(t *testing.T) {
			acct, _, _ := newSolanaTestAccountant(t, ctx, solanaTestOpts{wormchainContract: "0xdeadbeef", enforce: enforce, solanaNtt: true})
			msg := solanaTestNttTransfer(t, 4, false)
			// nttIsPayloadNTT still accepts it. The strict parser rejects the trailing byte.
			msg.Payload = append(msg.Payload, 0x99)
			covered, isNTT, _ := acct.isMessageCoveredByAccountant(msg)
			require.True(t, covered)
			require.True(t, isNTT)

			shouldPub, err := acct.SubmitObservation(msg)
			require.NoError(t, err)
			// Log-only emitters publish regardless. The devnet transceivers enforce.
			assert.Equal(t, !acct.nttDirectEmitters[emitterKey{emitterChainId: msg.EmitterChain, emitterAddr: msg.EmitterAddress}], shouldPub)
			assert.NotContains(t, acct.pendingTransfers, msg.MessageIDString())
		})
	}
}

// solanaNttFixture is one accountant with a Solana NTT backend and one pending NTT transfer.
type solanaNttFixture struct {
	acct    *Accountant
	b       *solanaBackend
	conn    *MockAccountantSolanaConn
	obsvReq chan *gossipv1.ObservationRequest
	msgChan chan *common.MessagePublication
	msg     *common.MessagePublication
	pe      *pendingEntry
	fields  *solanaNttObservationFields
	sub     *solanaSubmission

	hub  solanaNttHub
	peer vaa.Address
}

func newSolanaNttFixture(t *testing.T, ctx context.Context, relayed bool) *solanaNttFixture {
	t.Helper()
	obsvReq := make(chan *gossipv1.ObservationRequest, 10)
	acct, conn, msgChan := newSolanaTestAccountantWithObsvReq(t, ctx, solanaTestOpts{wormchainContract: "0xdeadbeef", enforce: true, solanaNtt: true}, obsvReq)
	setSolanaConfirmPollInterval(t, 0)
	acct.submitObservationBatchSize = 1
	conn.Balance, conn.BalanceErr = 1_000_000, nil

	msg := solanaTestNttTransfer(t, 88, relayed)
	_, err := acct.SubmitObservation(msg)
	require.NoError(t, err)
	pe := acct.pendingTransfers[msg.MessageIDString()]
	require.NotNil(t, pe)
	require.NotNil(t, pe.solanaNttFields)

	b := acct.solanaNtt
	sub, err := b.deriveSolanaSubmission(0, msg, pe.solanaNttFields)
	require.NoError(t, err)

	conn.LatestBlockhash = solacctconn.Blockhash{Hash: solana.Hash{9}, LastValidBlockHeight: 1000}
	conn.BlockHeight = 900
	conn.DefaultSignatureStatus = &solacctconn.SignatureStatus{Confirmed: true}

	return &solanaNttFixture{
		acct: acct, b: b, conn: conn, obsvReq: obsvReq, msgChan: msgChan, msg: msg, pe: pe, fields: pe.solanaNttFields, sub: sub,
		hub:  solanaNttHub{Chain: vaa.ChainIDSolana, Address: vaa.Address{0x7B}},
		peer: vaa.Address{0x7A},
	}
}

// transceiverHubAccount is a TransceiverHubLayout image.
func transceiverHubAccount(chain vaa.ChainID, address vaa.Address, hub solanaNttHub) *solacctconn.OwnedAccount {
	return &solacctconn.OwnedAccount{State: solacctconn.AccountInitialised, Data: mustEncodeWire(&transceiverHubWire{
		Tag:        transceiverHubTag,
		Chain:      uint16(chain),
		HubChain:   uint16(hub.Chain),
		Address:    address,
		HubAddress: hub.Address,
	})}
}

// transceiverPeerAccount is a TransceiverPeerLayout image.
func transceiverPeerAccount(chain vaa.ChainID, address vaa.Address, destChain vaa.ChainID, peer vaa.Address) *solacctconn.OwnedAccount {
	return &solacctconn.OwnedAccount{State: solacctconn.AccountInitialised, Data: mustEncodeWire(&transceiverPeerWire{
		Tag:         transceiverPeerTag,
		Chain:       uint16(chain),
		DestChain:   uint16(destChain),
		Address:     address,
		PeerAddress: peer,
	})}
}

// registerRoute seeds the sender's hub and its peer on the recipient chain.
func (f *solanaNttFixture) registerRoute() {
	f.conn.SetAccount(f.sub.nttRoute.hubPDA, transceiverHubAccount(f.fields.Chain, f.fields.Sender, f.hub))
	f.conn.SetAccount(f.sub.nttRoute.peerSrcPDA, transceiverPeerAccount(f.fields.Chain, f.fields.Sender, f.fields.RecipientChain, f.peer))
}

func (f *solanaNttFixture) queue() {
	f.pe.setSubmitPending(backendSolanaNTT, true)
	f.b.subChan <- f.msg
}

func TestSolanaNttSubmitTransaction(t *testing.T) {
	for _, relayed := range []bool{false, true} {
		t.Run(map[bool]string{false: "direct", true: "relayed"}[relayed], func(t *testing.T) {
			ctx := context.Background()
			f := newSolanaNttFixture(t, ctx, relayed)
			f.registerRoute()
			f.queue()

			require.NoError(t, f.acct.handleSolanaBatch(ctx, f.b))
			require.Len(t, f.conn.SentTransactions, 1)
			assert.False(t, f.pe.submitPending(backendSolanaNTT))

			tx := f.conn.SentTransactions[0]
			ix := tx.Message.Instructions[len(tx.Message.Instructions)-1]
			program, err := tx.Message.Program(ix.ProgramIDIndex)
			require.NoError(t, err)
			require.Equal(t, f.b.program, program)

			parsed, err := parseNttSubmitObservationsIxData(ix.Data)
			require.NoError(t, err)
			require.Equal(t, *f.fields, parsed.solanaNttObservationFields)
			require.Equal(t, f.msg.TxID, parsed.TxID.Bytes())

			digest, err := solanaObservationSigningDigest(NttSubmitObservationPrefix, parsed.TxID, &parsed.solanaNttObservationFields)
			require.NoError(t, err)
			pub, err := ethCrypto.SigToPub(digest.Bytes(), parsed.Signature[:])
			require.NoError(t, err)
			require.Equal(t, f.acct.guardianAddr, ethCrypto.PubkeyToAddress(*pub))

			guardianSet, err := deriveGuardianSetPDA(f.b.coreBridge, 0)
			require.NoError(t, err)
			pending, err := derivePendingObservationsPDA(f.b.program, f.fields.Chain, f.fields.Emitter, f.fields.Sequence, 0, f.fields.contentDigest, mustSolanaTxIDBytes(t, f.msg.TxID))
			require.NoError(t, err)
			bucket, err := deriveNoreplayBucketPDA(f.b.noreplay, f.b.authority, f.fields.Chain, f.fields.Emitter, f.fields.Sequence)
			require.NoError(t, err)
			source, err := deriveBalanceAccountPDA(f.b.program, f.fields.Chain, f.hub.Chain, f.hub.Address)
			require.NoError(t, err)
			dest, err := deriveBalanceAccountPDA(f.b.program, f.fields.RecipientChain, f.hub.Chain, f.hub.Address)
			require.NoError(t, err)
			relayer, err := deriveChainRegistrationPDA(f.b.program, f.fields.Chain)
			require.NoError(t, err)
			hubPDA, err := deriveTransceiverHubPDA(f.b.program, f.fields.Chain, f.fields.Sender)
			require.NoError(t, err)
			peerSrc, err := deriveTransceiverPeerPDA(f.b.program, f.fields.Chain, f.fields.Sender, f.fields.RecipientChain)
			require.NoError(t, err)
			peerDst, err := deriveTransceiverPeerPDA(f.b.program, f.fields.RecipientChain, f.peer, f.fields.Chain)
			require.NoError(t, err)

			// Account list of the NTT program's submit_observations.rs and README.
			want := [nttSubmitObservationsAccountCount]struct {
				key      solana.PublicKey
				writable bool
				signer   bool
			}{
				{f.b.feePayer.PublicKey(), true, true},
				{pending, true, false},
				{guardianSet, false, false},
				{bucket, true, false},
				{solana.SystemProgramID, false, false},
				{f.b.noreplay, false, false},
				{f.b.authority, false, false},
				{source, true, false},
				{dest, true, false},
				{f.b.feePayer.PublicKey(), true, false},
				{relayer, false, false},
				{hubPDA, false, false},
				{peerSrc, false, false},
				{peerDst, false, false},
			}
			accounts, err := ix.ResolveInstructionAccounts(&tx.Message)
			require.NoError(t, err)
			require.Len(t, accounts, nttSubmitObservationsAccountCount)
			for idx, w := range want {
				assert.Equal(t, w.key, accounts[idx].PublicKey, "account %d", idx)
				assert.Equal(t, w.writable, accounts[idx].IsWritable, "account %d writable", idx)
				// The fee payer is also slot 9, so the message marks both slots as the signer.
				if idx != 9 {
					assert.Equal(t, w.signer, accounts[idx].IsSigner, "account %d signer", idx)
				}
			}
		})
	}
}

func TestHandleSolanaNttBatchRoutes(t *testing.T) {
	tests := []struct {
		name         string
		setup        func(t *testing.T, f *solanaNttFixture)
		wantSent     int
		wantFailures float64
	}{
		{name: "routable", setup: func(t *testing.T, f *solanaNttFixture) { f.registerRoute() }, wantSent: 1},
		{
			name: "absent hub is unroutable",
			setup: func(t *testing.T, f *solanaNttFixture) {
				f.registerRoute()
				f.conn.SetAccount(f.sub.nttRoute.hubPDA, nil)
			},
			wantFailures: 1,
		},
		{
			name: "absent source peer is unroutable",
			setup: func(t *testing.T, f *solanaNttFixture) {
				f.registerRoute()
				f.conn.SetAccount(f.sub.nttRoute.peerSrcPDA, nil)
			},
			wantFailures: 1,
		},
		{
			name: "hub with a peer tag is unroutable",
			setup: func(t *testing.T, f *solanaNttFixture) {
				f.registerRoute()
				f.conn.SetAccount(f.sub.nttRoute.hubPDA, &solacctconn.OwnedAccount{State: solacctconn.AccountInitialised, Data: mustEncodeWire(&transceiverHubWire{Tag: transceiverPeerTag, Chain: uint16(f.fields.Chain), HubChain: uint16(f.hub.Chain), Address: f.fields.Sender, HubAddress: f.hub.Address})})
			},
			wantFailures: 1,
		},
		{
			name: "peer for another dest chain is unroutable",
			setup: func(t *testing.T, f *solanaNttFixture) {
				f.registerRoute()
				f.conn.SetAccount(f.sub.nttRoute.peerSrcPDA, transceiverPeerAccount(f.fields.Chain, f.fields.Sender, f.fields.Chain, f.peer))
			},
			wantFailures: 1,
		},
		{
			name: "route read failure abandons the batch",
			setup: func(t *testing.T, f *solanaNttFixture) {
				f.conn.GetOwnedAccountsErr = errors.New("rpc down")
			},
			wantFailures: 1,
		},
		{
			name: "recorded payer race retries once",
			setup: func(t *testing.T, f *solanaNttFixture) {
				f.registerRoute()
				f.conn.SendTransactionErr = customTxError(solanaErrPayerMismatch)
			},
			wantSent:     2,
			wantFailures: 1,
		},
		{
			name: "missing destination peer fails",
			setup: func(t *testing.T, f *solanaNttFixture) {
				f.registerRoute()
				f.conn.SendTransactionErr = customTxError(solanaErrMissingDestinationPeer)
			},
			wantSent:     1,
			wantFailures: 1,
		},
	}
	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			ctx := context.Background()
			f := newSolanaNttFixture(t, ctx, false)
			tt.setup(t, f)
			f.queue()

			failures := solanaSubmitFailures.WithLabelValues("ntt")
			wttFailures := solanaSubmitFailures.WithLabelValues("wtt")
			before, wttBefore := testutil.ToFloat64(failures), testutil.ToFloat64(wttFailures)
			require.NoError(t, f.acct.handleSolanaBatch(ctx, f.b))
			assert.Len(t, f.conn.SentTransactions, tt.wantSent)
			assert.Equal(t, tt.wantFailures, testutil.ToFloat64(failures)-before)
			assert.Equal(t, float64(0), testutil.ToFloat64(wttFailures)-wttBefore)
			assert.Contains(t, f.acct.pendingTransfers, f.msg.MessageIDString())
			assert.False(t, f.pe.submitPending(backendSolanaNTT))
		})
	}
}

func TestSolanaNttSubmissionRejectsTheOtherFamily(t *testing.T) {
	ctx := context.Background()
	f := newSolanaNttFixture(t, ctx, false)
	wttFields := fixtureTransferFields(t)

	_, err := f.b.deriveSolanaSubmission(0, f.msg, wttFields)
	require.Error(t, err)
	_, err = f.acct.solana.deriveSolanaSubmission(0, f.msg, f.fields)
	require.Error(t, err)

	// submitAccountMetas requires a resolved route.
	_, err = f.b.submitAccountMetas(solanaGuardianIdentity{}, f.sub)
	require.Error(t, err)
}

func TestClassifySolanaNttTxError(t *testing.T) {
	tests := []struct {
		name       string
		code       uint32
		wantReason string
	}{
		{name: "invalid instruction data", code: solanaErrInvalidInstructionData, wantReason: "the program rejected the instruction data"},
		{name: "unregistered emitter", code: solanaErrUnregisteredEmitter, wantReason: "the emitter is not registered"},
		{name: "malformed ntt message", code: solanaErrMalformedNttMessage, wantReason: "the program rejected the NTT message"},
		{name: "missing hub", code: solanaErrMissingTransceiverHub, wantReason: "the sender has no transceiver hub"},
		{name: "missing source peer", code: solanaErrMissingSourcePeer, wantReason: "the sender has no peer on the recipient chain"},
		{name: "missing destination peer", code: solanaErrMissingDestinationPeer, wantReason: "the peer has no entry for the source chain"},
		{name: "not cross-registered", code: solanaErrPeersNotCrossRegistered, wantReason: "the peers are not cross-registered"},
	}
	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			disposition, reason := classifySolanaTxError(customTxError(tt.code))
			assert.Equal(t, solanaTxFailed, disposition)
			assert.Equal(t, tt.wantReason, reason)
		})
	}
}

func TestSolanaNttCommitIsKeyedOnFamily(t *testing.T) {
	ctx := context.Background()

	tests := []struct {
		name          string
		ntt           bool // the pending entry is NTT
		viaNtt        bool // the commit comes from the NTT program
		useContent    bool
		wantPublished int
		wantPending   int
	}{
		{name: "ntt content digest releases an ntt entry", ntt: true, viaNtt: true, useContent: true, wantPublished: 1},
		{name: "ntt vaa digest from submit_vaas releases an ntt entry", ntt: true, viaNtt: true, wantPublished: 1},
		{name: "wtt commit leaves an ntt entry", ntt: true, wantPending: 1},
		{name: "wtt commit with the ntt content digest leaves an ntt entry", ntt: true, useContent: true, wantPending: 1},
		{name: "ntt commit leaves a wtt entry", viaNtt: true, wantPending: 1},
		{name: "ntt commit with the wtt content digest leaves a wtt entry", viaNtt: true, useContent: true, wantPending: 1},
	}
	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			acct, _, msgChan := newSolanaTestAccountant(t, ctx, solanaTestOpts{wormchainContract: "0xdeadbeef", enforce: true, solanaNtt: true})
			msg := solanaTestTransfer(t, 21)
			if tt.ntt {
				msg = solanaTestNttTransfer(t, 21, false)
			}
			_, err := acct.SubmitObservation(msg)
			require.NoError(t, err)
			pe := acct.pendingTransfers[msg.MessageIDString()]
			require.NotNil(t, pe)

			digest := pe.vaaDigest
			if tt.useContent {
				if tt.ntt {
					digest = pe.solanaNttFields.contentDigest
				} else {
					digest = pe.solanaFields.contentDigest
				}
			}
			b := acct.solana
			if tt.viaNtt {
				b = acct.solanaNtt
			}
			commit := newSolanaCommitEvent(msg.EmitterChain, msg.EmitterAddress, msg.Sequence, digest, 0)
			acct.handleSolanaLogEvent(solacctconn.LogEvent{Logs: commitLogs(b.program, commit)}, b)

			assert.Len(t, msgChan, tt.wantPublished)
			assert.Len(t, acct.pendingTransfers, tt.wantPending)
		})
	}
}

func TestAuditSolanaNttOwnPendingTransfers(t *testing.T) {
	tests := []struct {
		name          string
		pending       func(f *solanaNttFixture) *solacctconn.OwnedAccount
		accounted     bool
		wantResubmit  int
		wantPublished int
	}{
		{name: "absent and unaccounted resubmits", pending: func(f *solanaNttFixture) *solacctconn.OwnedAccount { return nil }, wantResubmit: 1},
		{name: "lacks own signature resubmits", pending: func(f *solanaNttFixture) *solacctconn.OwnedAccount { return f.pendingAccount(t, nil) }, wantResubmit: 1},
		{name: "has own signature waits", pending: func(f *solanaNttFixture) *solacctconn.OwnedAccount { return f.pendingAccount(t, []uint8{0}) }},
		{name: "accounted finds the commit", pending: func(f *solanaNttFixture) *solacctconn.OwnedAccount { return nil }, accounted: true, wantPublished: 1},
	}
	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			ctx := context.Background()
			f := newSolanaNttFixture(t, ctx, true)
			// A WTT transfer shares the map and stays out of the NTT audit.
			wtt := solanaTestTransfer(t, 5)
			_, err := f.acct.SubmitObservation(wtt)
			require.NoError(t, err)

			own := f.acct.snapshotSolanaOwnPendingTransfers(f.b, 0)
			require.Len(t, own, 1)
			transfer, exists := own[f.sub.pendingPDA]
			require.True(t, exists)
			require.Same(t, f.pe, transfer.pe)

			f.conn.SetAccount(f.sub.pendingPDA, tt.pending(f))
			if tt.accounted {
				f.conn.SetAccount(f.sub.noreplayBucket, &solacctconn.OwnedAccount{State: solacctconn.AccountInitialised, Data: solanaNoreplayBucket(t, f.fields.Sequence)})
				sig := solana.Signature{0x42}
				f.conn.SetSignaturesForAddress(f.sub.pendingPDA, []solana.Signature{sig})
				commit := newSolanaCommitEvent(f.fields.Chain, f.fields.Emitter, f.fields.Sequence, f.fields.contentDigest, 0)
				f.conn.SetTransaction(sig, &solacctconn.TransactionResult{LogMessages: commitLogs(f.b.program, commit)})
			}

			f.acct.auditSolanaOwnPendingTransfers(ctx, f.b, 0, 0, own)
			assert.Len(t, f.b.subChan, tt.wantResubmit)
			assert.Empty(t, f.acct.solana.subChan)
			assert.Len(t, f.msgChan, tt.wantPublished)
		})
	}
}

// pendingAccount is the live-set NTT pending account of the fixture transfer at set index 0.
func (f *solanaNttFixture) pendingAccount(t *testing.T, signedBy []uint8) *solacctconn.OwnedAccount {
	t.Helper()
	return &solacctconn.OwnedAccount{State: solacctconn.AccountInitialised, Data: solanaPendingAccountDataWithTxID(t, f.fields.Chain, 0, f.fields.contentDigest, f.b.feePayer.PublicKey(), mustSolanaTxIDBytes(t, f.msg.TxID), signedBy)}
}

func TestReobserveUnknownSolanaNttPendingAccount(t *testing.T) {
	ctx := context.Background()
	f := newSolanaNttFixture(t, ctx, false)

	unknown := *f.fields
	unknown.Sequence = 9_999
	unknown.setContentDigest()
	signatureTxID := make([]byte, signatureTxIDLen)
	for i := range signatureTxID {
		signatureTxID[i] = 0xA0 + byte(i)
	}
	txID := mustSolanaTxIDBytes(t, signatureTxID)
	pda, err := derivePendingObservationsPDA(f.b.program, unknown.Chain, unknown.Emitter, unknown.Sequence, 0, unknown.contentDigest, txID)
	require.NoError(t, err)

	f.conn.ProgramAccounts = []solacctconn.ProgramAccount{{Address: pda, Data: solanaPendingAccountDataWithTxID(t, unknown.Chain, 0, unknown.contentDigest, solana.PublicKey{0x01}, txID, nil)}}
	own := f.acct.snapshotSolanaOwnPendingTransfers(f.b, 0)
	f.acct.auditSolanaProgramPendingAccounts(ctx, f.b, 0, 0, own, map[solana.PublicKey]struct{}{})

	require.Len(t, f.obsvReq, 1)
	req := <-f.obsvReq
	assert.Equal(t, uint32(unknown.Chain), req.ChainId)
	assert.Equal(t, signatureTxID, req.TxHash)
	assert.Empty(t, f.conn.GetSignaturesForAddressCalls)
}
