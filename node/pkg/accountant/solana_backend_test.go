package accountant

import (
	"bytes"
	"context"
	"testing"
	"time"

	"github.com/certusone/wormhole/node/pkg/common"
	guardianDB "github.com/certusone/wormhole/node/pkg/db"
	"github.com/certusone/wormhole/node/pkg/devnet"
	"github.com/certusone/wormhole/node/pkg/guardiansigner"
	gossipv1 "github.com/certusone/wormhole/node/pkg/proto/gossip/v1"
	ethCommon "github.com/ethereum/go-ethereum/common"
	"github.com/gagliardetto/solana-go"
	"github.com/stretchr/testify/assert"
	"github.com/stretchr/testify/require"
	"github.com/wormhole-foundation/wormhole/sdk/vaa"
	"go.uber.org/zap"
	"go.uber.org/zap/zaptest"
)

// The devnet Token Bridge emitter that the existing accountant tests use.
const testTokenBridgeEmitterHex = "0000000000000000000000000290fb167208af455bb137780163b7b7a9a10c16"

func filledKey(b byte) solana.PublicKey {
	return solana.PublicKey(bytes.Repeat([]byte{b}, solana.PublicKeyLength))
}

func solanaTestProgram() solana.PublicKey    { return filledKey(0x11) }
func solanaTestNoreplay() solana.PublicKey   { return filledKey(0x22) }
func solanaTestCoreBridge() solana.PublicKey { return filledKey(0x33) }

func solanaTestFeePayer(t *testing.T) solana.PrivateKey {
	t.Helper()
	key, err := solana.NewRandomPrivateKey()
	require.NoError(t, err)
	return key
}

// fakeAccountantDB serves a fixed set of pending transfers on reload.
type fakeAccountantDB struct {
	data []*common.MessagePublication
}

func (d *fakeAccountantDB) AcctStorePendingTransfer(msg *common.MessagePublication) error { return nil }
func (d *fakeAccountantDB) AcctDeletePendingTransfer(msgId string) error                  { return nil }
func (d *fakeAccountantDB) AcctGetData(logger *zap.Logger) ([]*common.MessagePublication, error) {
	return d.data, nil
}

type solanaTestOpts struct {
	wormchainContract string
	nttContract       string
	enforce           bool
	disableSolana     bool
	// solanaConfig replaces the complete test config when set.
	solanaConfig *AccountantSolanaConfig
	// solanaNtt enables the Solana NTT program beside the WTT one.
	solanaNtt bool
	db        guardianDB.AccountantDB
}

// newSolanaTestAccountant builds a started accountant in the GoTest environment. The test
// drives the code paths directly.
func newSolanaTestAccountant(t *testing.T, ctx context.Context, opts solanaTestOpts) (*Accountant, *MockAccountantSolanaConn, chan *common.MessagePublication) {
	t.Helper()
	return newSolanaTestAccountantWithObsvReq(t, ctx, opts, make(chan *gossipv1.ObservationRequest, 10))
}

// newSolanaTestAccountantWithObsvReq is newSolanaTestAccountant with a reobservation
// channel the caller can read.
func newSolanaTestAccountantWithObsvReq(t *testing.T, ctx context.Context, opts solanaTestOpts, obsvReqC chan *gossipv1.ObservationRequest) (*Accountant, *MockAccountantSolanaConn, chan *common.MessagePublication) {
	t.Helper()
	acct, conn, msgChan := newUnstartedSolanaTestAccountant(t, ctx, opts, obsvReqC)
	require.NoError(t, acct.Start(ctx))
	return acct, conn, msgChan
}

// newUnstartedSolanaTestAccountant is newSolanaTestAccountantWithObsvReq without Start.
func newUnstartedSolanaTestAccountant(t *testing.T, ctx context.Context, opts solanaTestOpts, obsvReqC chan *gossipv1.ObservationRequest) (*Accountant, *MockAccountantSolanaConn, chan *common.MessagePublication) {
	t.Helper()

	pk := devnet.InsecureDeterministicEcdsaKeyByIndex(uint64(0))
	guardianSigner, err := guardiansigner.GenerateSignerWithPrivatekeyUnsafe(pk)
	require.NoError(t, err)

	gst := common.NewGuardianSetState(nil)
	gst.Set(&common.GuardianSet{Index: 0, Keys: []ethCommon.Address{ethCommon.HexToAddress("0xbeFA429d57cD18b7F8A4d91A2da9AB4AF05d0FBe")}})

	db := opts.db
	if db == nil {
		db = &guardianDB.MockAccountantDB{}
	}

	conn := NewMockAccountantSolanaConn()
	cfg := AccountantSolanaConfig{
		Conn:       conn,
		Program:    solanaTestProgram(),
		Noreplay:   solanaTestNoreplay(),
		CoreBridge: solanaTestCoreBridge(),
		FeePayer:   solanaTestFeePayer(t),
	}
	if opts.solanaNtt {
		cfg.NttProgram = solanaTestNttProgram()
	}
	switch {
	case opts.disableSolana:
		cfg = AccountantSolanaConfig{}
	case opts.solanaConfig != nil:
		cfg = *opts.solanaConfig
	}

	msgChan := make(chan *common.MessagePublication, MsgChannelCapacity)
	acct := NewAccountant(
		ctx,
		zaptest.NewLogger(t),
		db,
		obsvReqC,
		opts.wormchainContract,
		"none",
		nil,
		opts.enforce,
		opts.nttContract,
		nil,
		cfg,
		guardianSigner,
		gst,
		msgChan,
		DefaultSubmitObservationBatchSize,
		common.GoTest,
	)
	return acct, conn, msgChan
}

// solanaTestTransfer returns a Token Bridge transfer from a devnet emitter.
func solanaTestTransfer(t *testing.T, sequence uint64) *common.MessagePublication {
	t.Helper()
	emitterAddr, err := vaa.StringToAddress(testTokenBridgeEmitterHex)
	require.NoError(t, err)

	return &common.MessagePublication{
		TxID:             hashToTxID("0x06f541f5ecfc43407c31587aa6ac3a689e8960f36dc23c332db5510dfc6a4063"),
		Timestamp:        time.Unix(int64(1654543099), 0),
		Nonce:            uint32(1),
		Sequence:         sequence,
		EmitterChain:     vaa.ChainIDEthereum,
		EmitterAddress:   emitterAddr,
		ConsistencyLevel: uint8(32),
		// buildMockTransferPayloadBytes stops at the recipient chain. The 32-byte fee
		// completes the 133-byte TokenBridgeTransfer that the Solana program parses.
		Payload: append(buildMockTransferPayloadBytes(1,
			vaa.ChainIDEthereum,
			"0x707f9118e33a9b8998bea41dd0d46f38bb963fc8",
			vaa.ChainIDPolygon,
			"0x707f9118e33a9b8998bea41dd0d46f38bb963fc8",
			1.25,
		), make([]byte, 32)...),
	}
}

func solanaTestNttProgram() solana.PublicKey {
	var pk [32]byte
	for i := range pk {
		pk[i] = 0x44
	}
	return pk
}

func TestNewSolanaBackends(t *testing.T) {
	conn := NewMockAccountantSolanaConn()
	feePayer := solanaTestFeePayer(t)
	full := AccountantSolanaConfig{Conn: conn, Program: solanaTestProgram(), Noreplay: solanaTestNoreplay(), CoreBridge: solanaTestCoreBridge(), FeePayer: feePayer, PriorityFee: 7}
	with := func(edit func(*AccountantSolanaConfig)) AccountantSolanaConfig {
		cfg := full
		edit(&cfg)
		return cfg
	}

	tests := []struct {
		name    string
		cfg     AccountantSolanaConfig
		wantWTT bool
		wantNTT bool
		wantErr bool
	}{
		{name: "zero config disables", cfg: AccountantSolanaConfig{}},
		{name: "conn without program", cfg: AccountantSolanaConfig{Conn: conn}, wantErr: true},
		{name: "program without conn", cfg: AccountantSolanaConfig{Program: solanaTestProgram()}, wantErr: true},
		{name: "ntt program without conn", cfg: AccountantSolanaConfig{NttProgram: solanaTestNttProgram()}, wantErr: true},
		{name: "neither program", cfg: with(func(c *AccountantSolanaConfig) { c.Program = solana.PublicKey{} }), wantErr: true},
		{name: "missing noreplay", cfg: with(func(c *AccountantSolanaConfig) { c.Noreplay = solana.PublicKey{} }), wantErr: true},
		{name: "missing core bridge", cfg: with(func(c *AccountantSolanaConfig) { c.CoreBridge = solana.PublicKey{} }), wantErr: true},
		{name: "missing fee payer", cfg: with(func(c *AccountantSolanaConfig) { c.FeePayer = nil }), wantErr: true},
		{name: "program equals noreplay", cfg: with(func(c *AccountantSolanaConfig) { c.Noreplay = solanaTestProgram() }), wantErr: true},
		{name: "core bridge equals program", cfg: with(func(c *AccountantSolanaConfig) { c.CoreBridge = solanaTestProgram() }), wantErr: true},
		{name: "core bridge equals noreplay", cfg: with(func(c *AccountantSolanaConfig) { c.CoreBridge = solanaTestNoreplay() }), wantErr: true},
		{name: "ntt program equals program", cfg: with(func(c *AccountantSolanaConfig) { c.NttProgram = solanaTestProgram() }), wantErr: true},
		{name: "ntt program equals noreplay", cfg: with(func(c *AccountantSolanaConfig) { c.NttProgram = solanaTestNoreplay() }), wantErr: true},
		{name: "ntt program equals core bridge", cfg: with(func(c *AccountantSolanaConfig) { c.NttProgram = solanaTestCoreBridge() }), wantErr: true},
		{name: "wtt only", cfg: full, wantWTT: true},
		{name: "ntt only", cfg: with(func(c *AccountantSolanaConfig) { c.Program = solana.PublicKey{}; c.NttProgram = solanaTestNttProgram() }), wantNTT: true},
		{name: "both programs", cfg: with(func(c *AccountantSolanaConfig) { c.NttProgram = solanaTestNttProgram() }), wantWTT: true, wantNTT: true},
	}

	check := func(t *testing.T, b *solanaBackend, family solanaProgramFamily, program solana.PublicKey, backend accountantBackend, prefix []byte, tag string) {
		t.Helper()
		require.NotNil(t, b)
		assert.Equal(t, family, b.family)
		assert.Equal(t, backend, b.backend)
		assert.Equal(t, program, b.program)
		assert.Equal(t, solanaTestNoreplay(), b.noreplay)
		assert.Equal(t, solanaTestCoreBridge(), b.coreBridge)
		assert.Equal(t, prefix, b.prefix)
		assert.Equal(t, tag, b.tag)
		assert.Equal(t, uint64(7), b.priorityFee)
		wantAuthority, err := deriveNoreplayAuthorityPDA(program)
		require.NoError(t, err)
		assert.Equal(t, wantAuthority, b.authority)
		assert.Equal(t, subChanSize, cap(b.subChan))
	}

	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			wtt, ntt, err := newSolanaBackends(tt.cfg)
			if tt.wantErr {
				require.Error(t, err)
				assert.Nil(t, wtt)
				assert.Nil(t, ntt)
				return
			}
			require.NoError(t, err)
			if tt.wantWTT {
				check(t, wtt, solanaFamilyWTT, solanaTestProgram(), backendSolana, SubmitObservationPrefix, "solana-accountant")
			} else {
				assert.Nil(t, wtt)
			}
			if tt.wantNTT {
				check(t, ntt, solanaFamilyNTT, solanaTestNttProgram(), backendSolanaNTT, NttSubmitObservationPrefix, "solana-ntt-accountant")
			} else {
				assert.Nil(t, ntt)
			}
			if tt.wantWTT && tt.wantNTT {
				assert.Same(t, wtt.conn, ntt.conn)
				assert.Equal(t, wtt.feePayer, ntt.feePayer)
			}
		})
	}
}

func TestStartRejectsPartialSolanaConfig(t *testing.T) {
	ctx := context.Background()
	acct, _, _ := newUnstartedSolanaTestAccountant(t, ctx, solanaTestOpts{
		wormchainContract: "0xdeadbeef",
		enforce:           true,
		solanaConfig:      &AccountantSolanaConfig{Conn: NewMockAccountantSolanaConn()},
	}, make(chan *gossipv1.ObservationRequest, 10))
	require.Error(t, acct.Start(ctx))
	assert.False(t, acct.solanaEnabled())
}

// The p2p options read the feature string before Start, so each case is an unstarted accountant.
func TestFeatureString(t *testing.T) {
	wtt := AccountantSolanaConfig{Program: solanaTestProgram()}
	ntt := AccountantSolanaConfig{NttProgram: solanaTestNttProgram()}
	both := AccountantSolanaConfig{Program: solanaTestProgram(), NttProgram: solanaTestNttProgram()}
	tests := []struct {
		name string
		acct *Accountant
		want string
	}{
		{name: "wormchain log only", acct: &Accountant{}, want: "acct-logonly"},
		{name: "solana enforcing", acct: &Accountant{enforceFlag: true, solanaCfg: wtt}, want: "acct:sol-acct"},
		{name: "solana log only", acct: &Accountant{solanaCfg: wtt}, want: "acct-logonly:sol-acct-logonly"},
		{name: "solana ntt enforcing", acct: &Accountant{enforceFlag: true, solanaCfg: ntt}, want: "acct:sol-ntt-acct"},
		{name: "solana ntt log only", acct: &Accountant{solanaCfg: ntt}, want: "acct-logonly:sol-ntt-acct-logonly"},
		{name: "all four", acct: &Accountant{enforceFlag: true, nttContract: "x", solanaCfg: both}, want: "acct:ntt-acct:sol-acct:sol-ntt-acct"},
	}

	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			assert.Equal(t, tt.want, tt.acct.FeatureString())
		})
	}
}

func TestSubmitObservationFanOut(t *testing.T) {
	ctx := context.Background()

	tests := []struct {
		name          string
		wormchain     string
		nttContract   string
		solanaNtt     bool
		disableSolana bool
		isNTT         bool
		wantWormchain int
		wantSolana    int
		wantNtt       int
		wantSolanaNtt int
	}{
		{name: "wormchain only", wormchain: "0xdeadbeef", disableSolana: true, wantWormchain: 1},
		{name: "solana only", wormchain: "", wantSolana: 1},
		{name: "both backends", wormchain: "0xdeadbeef", wantWormchain: 1, wantSolana: 1},
		{name: "token bridge entry skips solana ntt", wormchain: "0xdeadbeef", solanaNtt: true, wantWormchain: 1, wantSolana: 1},
		{name: "ntt entry stays on the ntt channel", wormchain: "0xdeadbeef", nttContract: "0xfeed", isNTT: true, wantNtt: 1},
		{name: "ntt entry without an ntt backend", wormchain: "0xdeadbeef", isNTT: true},
		{name: "ntt entry on solana ntt only", wormchain: "0xdeadbeef", solanaNtt: true, isNTT: true, wantSolanaNtt: 1},
		{name: "ntt entry on both ntt backends", wormchain: "0xdeadbeef", nttContract: "0xfeed", solanaNtt: true, isNTT: true, wantNtt: 1, wantSolanaNtt: 1},
	}

	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			acct, _, _ := newSolanaTestAccountant(t, ctx, solanaTestOpts{wormchainContract: tt.wormchain, nttContract: tt.nttContract, solanaNtt: tt.solanaNtt, disableSolana: tt.disableSolana, enforce: true})

			msg := solanaTestTransfer(t, 1)
			_, err := acct.SubmitObservation(msg)
			require.NoError(t, err)

			pe, exists := acct.pendingTransfers[msg.MessageIDString()]
			require.True(t, exists)
			pe.isNTT = tt.isNTT

			for backend := range numAccountantBackends {
				acct.submitObservation(ctx, pe, backend, false)
			}

			assert.Equal(t, tt.wantWormchain, len(acct.subChan))
			assert.Equal(t, tt.wantNtt, len(acct.nttSubChan))
			if acct.solanaEnabled() {
				assert.Equal(t, tt.wantSolana, len(acct.solana.subChan))
			}
			if acct.solanaNttEnabled() {
				assert.Equal(t, tt.wantSolanaNtt, len(acct.solanaNtt.subChan))
			} else {
				assert.Zero(t, tt.wantSolanaNtt)
			}
		})
	}
}

// TestSubmitObservationSiblingTxIDs covers a reobservation that returns another tx id for a
// pending transfer. The Solana backend submits each checked tx id, up to the bound. The
// wormchain backend submits the first tx id only.
func TestSubmitObservationSiblingTxIDs(t *testing.T) {
	ctx := context.Background()
	accountTxID := bytes.Repeat([]byte{0xA1}, 32)
	closeTxID := bytes.Repeat([]byte{0xC1}, 64)
	shimTxID := bytes.Repeat([]byte{0xD1}, 64)

	type observation struct {
		txID         []byte
		changeDigest bool
	}
	tests := []struct {
		name          string
		wormchain     string
		observations  []observation
		wantSolana    [][]byte
		wantWormchain [][]byte
	}{
		{name: "one tx id", observations: []observation{{txID: accountTxID}}, wantSolana: [][]byte{accountTxID}},
		{name: "same tx id twice", observations: []observation{{txID: accountTxID}, {txID: accountTxID}}, wantSolana: [][]byte{accountTxID}},
		{name: "sibling tx id", observations: []observation{{txID: accountTxID}, {txID: closeTxID}}, wantSolana: [][]byte{accountTxID, closeTxID}},
		{name: "third tx id exceeds the bound", observations: []observation{{txID: accountTxID}, {txID: closeTxID}, {txID: shimTxID}}, wantSolana: [][]byte{accountTxID, closeTxID}},
		{name: "changed digest adds no tx id", observations: []observation{{txID: accountTxID}, {txID: closeTxID, changeDigest: true}}, wantSolana: [][]byte{accountTxID}},
		{name: "wormchain keeps the first tx id", wormchain: "0xdeadbeef", observations: []observation{{txID: accountTxID}, {txID: closeTxID}}, wantSolana: [][]byte{accountTxID, closeTxID}, wantWormchain: [][]byte{accountTxID}},
	}

	drain := func(ch chan *common.MessagePublication) [][]byte {
		var got [][]byte
		for len(ch) > 0 {
			got = append(got, (<-ch).TxID)
		}
		return got
	}

	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			require.NotEmpty(t, tt.observations)
			acct, _, _ := newSolanaTestAccountant(t, ctx, solanaTestOpts{wormchainContract: tt.wormchain, enforce: true})

			var msgId string
			for _, o := range tt.observations {
				msg := solanaTestTransfer(t, 1)
				msg.TxID = o.txID
				if o.changeDigest {
					msg.Nonce++
				}
				_, err := acct.SubmitObservation(msg)
				require.NoError(t, err)
				msgId = msg.MessageIDString()
			}

			pe, exists := acct.pendingTransfers[msgId]
			require.True(t, exists)
			for backend := range numAccountantBackends {
				acct.submitObservation(ctx, pe, backend, false)
			}

			assert.Equal(t, tt.wantSolana, drain(acct.solana.subChan))
			assert.Equal(t, tt.wantWormchain, drain(acct.subChan))
		})
	}
}

// TestSubmitObservationAcceptsLargeTransferPayload holds a TransferWithPayload of any
// length. submit_observations carries the body digest, so payload length is unbounded.
func TestSubmitObservationAcceptsLargeTransferPayload(t *testing.T) {
	ctx := context.Background()
	tests := []struct {
		name  string
		extra int
	}{
		{name: "2000 extra bytes", extra: 2000},
		{name: "2001 extra bytes", extra: 2001},
		{name: "10000 extra bytes", extra: 10_000},
	}

	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			acct, _, _ := newSolanaTestAccountant(t, ctx, solanaTestOpts{enforce: true, wormchainContract: "0xdeadbeef"})
			msg := solanaTestTransfer(t, 7)
			msg.Payload[0] = 3
			msg.Payload = append(msg.Payload, make([]byte, tt.extra)...)

			shouldPub, err := acct.SubmitObservation(msg)
			require.NoError(t, err)
			assert.False(t, shouldPub)

			pe, exists := acct.pendingTransfers[msg.MessageIDString()]
			require.True(t, exists)
			require.NotNil(t, pe.solanaFields)
			assert.Equal(t, uint8(3), pe.solanaFields.Action)
		})
	}
}

func TestUnbuildableSolanaFieldsLeaveNoEntry(t *testing.T) {
	ctx := context.Background()
	msg := solanaTestTransfer(t, 7)
	msg.Payload = msg.Payload[:tokenBridgeTransferLen-1]

	tests := []struct {
		name          string
		reload        bool
		disableSolana bool
		wantEntry     bool
	}{
		{name: "submit observation"},
		{name: "reload", reload: true},
		{name: "solana disabled keeps the entry", disableSolana: true, wantEntry: true},
	}

	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			opts := solanaTestOpts{enforce: true, disableSolana: tt.disableSolana}
			if tt.disableSolana {
				opts.wormchainContract = "0xdeadbeef"
			}
			if tt.reload {
				opts.db = &fakeAccountantDB{data: []*common.MessagePublication{msg}}
			}
			acct, _, _ := newSolanaTestAccountant(t, ctx, opts)
			if !tt.reload {
				shouldPub, err := acct.SubmitObservation(msg)
				require.NoError(t, err)
				assert.False(t, shouldPub)
			}

			pe, exists := acct.pendingTransfers[msg.MessageIDString()]
			require.Equal(t, tt.wantEntry, exists)
			if exists {
				assert.Nil(t, pe.solanaFields)
			}
		})
	}
}
