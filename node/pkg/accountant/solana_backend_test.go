package accountant

import (
	"context"
	"testing"
	"time"

	"github.com/certusone/wormhole/node/pkg/common"
	guardianDB "github.com/certusone/wormhole/node/pkg/db"
	"github.com/certusone/wormhole/node/pkg/devnet"
	"github.com/certusone/wormhole/node/pkg/guardiansigner"
	gossipv1 "github.com/certusone/wormhole/node/pkg/proto/gossip/v1"
	"github.com/certusone/wormhole/node/pkg/solacctconn"
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

func solanaTestProgram() solana.PublicKey {
	var pk [32]byte
	for i := range pk {
		pk[i] = 0x11
	}
	return pk
}

func solanaTestNoreplay() solana.PublicKey {
	var pk [32]byte
	for i := range pk {
		pk[i] = 0x22
	}
	return pk
}

func solanaTestCoreBridge() solana.PublicKey {
	var pk [32]byte
	for i := range pk {
		pk[i] = 0x33
	}
	return pk
}

func solanaTestFeePayer(t *testing.T) solana.PrivateKey {
	t.Helper()
	key, err := solana.NewRandomPrivateKey()
	require.NoError(t, err)
	require.Len(t, key, solacctconn.FeePayerKeyLen)
	return key
}

func solanaTestConfig(t *testing.T, conn solacctconn.Conn) AccountantSolanaConfig {
	t.Helper()
	return AccountantSolanaConfig{
		Conn:       conn,
		Program:    solanaTestProgram(),
		Noreplay:   solanaTestNoreplay(),
		CoreBridge: solanaTestCoreBridge(),
		FeePayer:   solanaTestFeePayer(t),
	}
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
	enforce           bool
	disableSolana     bool
	db                guardianDB.AccountantDB
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
	cfg := solanaTestConfig(t, conn)
	if opts.disableSolana {
		cfg = AccountantSolanaConfig{}
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
		"",
		nil,
		cfg,
		guardianSigner,
		gst,
		msgChan,
		DefaultSubmitObservationBatchSize,
		common.GoTest,
	)
	require.NoError(t, acct.Start(ctx))
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

func TestNewSolanaBackend(t *testing.T) {
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
		wantNil bool
		wantErr bool
	}{
		{name: "zero config disables", cfg: AccountantSolanaConfig{}, wantNil: true},
		{name: "conn without program", cfg: AccountantSolanaConfig{Conn: conn}, wantErr: true},
		{name: "program without conn", cfg: AccountantSolanaConfig{Program: solanaTestProgram()}, wantErr: true},
		{name: "core bridge alone", cfg: AccountantSolanaConfig{CoreBridge: solanaTestCoreBridge()}, wantErr: true},
		{name: "missing noreplay", cfg: with(func(c *AccountantSolanaConfig) { c.Noreplay = solana.PublicKey{} }), wantErr: true},
		{name: "missing core bridge", cfg: with(func(c *AccountantSolanaConfig) { c.CoreBridge = solana.PublicKey{} }), wantErr: true},
		{name: "missing fee payer", cfg: with(func(c *AccountantSolanaConfig) { c.FeePayer = nil }), wantErr: true},
		{name: "short fee payer", cfg: with(func(c *AccountantSolanaConfig) { c.FeePayer = solana.PrivateKey(make([]byte, 32)) }), wantErr: true},
		{name: "program equals noreplay", cfg: with(func(c *AccountantSolanaConfig) { c.Noreplay = solanaTestProgram() }), wantErr: true},
		{name: "core bridge equals program", cfg: with(func(c *AccountantSolanaConfig) { c.CoreBridge = solanaTestProgram() }), wantErr: true},
		{name: "core bridge equals noreplay", cfg: with(func(c *AccountantSolanaConfig) { c.CoreBridge = solanaTestNoreplay() }), wantErr: true},
		{name: "complete config", cfg: full},
	}

	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			b, err := newSolanaBackend(tt.cfg)
			if tt.wantErr {
				require.Error(t, err)
				assert.Nil(t, b)
				return
			}
			require.NoError(t, err)
			if tt.wantNil {
				assert.Nil(t, b)
				return
			}
			require.NotNil(t, b)
			assert.Equal(t, solanaTestProgram(), b.program)
			assert.Equal(t, solanaTestNoreplay(), b.noreplay)
			assert.Equal(t, solanaTestCoreBridge(), b.coreBridge)
			assert.Equal(t, SubmitObservationPrefix, b.prefix)
			assert.Equal(t, "solana-accountant", b.tag)
			assert.Equal(t, uint64(7), b.priorityFee)
			wantAuthority, err := deriveNoreplayAuthorityPDA(solanaTestProgram())
			require.NoError(t, err)
			assert.Equal(t, wantAuthority, b.authority)
			assert.Equal(t, subChanSize, cap(b.subChan))
		})
	}
}

func TestStartRejectsPartialSolanaConfig(t *testing.T) {
	ctx := context.Background()
	pk := devnet.InsecureDeterministicEcdsaKeyByIndex(uint64(0))
	guardianSigner, err := guardiansigner.GenerateSignerWithPrivatekeyUnsafe(pk)
	require.NoError(t, err)
	gst := common.NewGuardianSetState(nil)
	gst.Set(&common.GuardianSet{Keys: []ethCommon.Address{ethCommon.HexToAddress("0xbeFA429d57cD18b7F8A4d91A2da9AB4AF05d0FBe")}})

	acct := NewAccountant(
		ctx,
		zaptest.NewLogger(t),
		&guardianDB.MockAccountantDB{},
		make(chan *gossipv1.ObservationRequest, 10),
		"0xdeadbeef",
		"none",
		nil,
		true,
		"",
		nil,
		AccountantSolanaConfig{Conn: NewMockAccountantSolanaConn()},
		guardianSigner,
		gst,
		make(chan *common.MessagePublication, MsgChannelCapacity),
		DefaultSubmitObservationBatchSize,
		common.GoTest,
	)
	require.Error(t, acct.Start(ctx))
	assert.False(t, acct.solanaEnabled())
}

func TestSolanaOnlyGuardianCoversTokenBridge(t *testing.T) {
	ctx := context.Background()
	acct, _, _ := newSolanaTestAccountant(t, ctx, solanaTestOpts{wormchainContract: ""})

	assert.False(t, acct.wormchainBaseEnabled())
	assert.True(t, acct.solanaEnabled())
	assert.True(t, acct.baseEnabled())
	assert.False(t, acct.nttEnabled())
	assert.NotEmpty(t, acct.tokenBridges)
}

func TestFeatureString(t *testing.T) {
	backend := &solanaBackend{}
	tests := []struct {
		name string
		acct *Accountant
		want string
	}{
		{name: "wormchain enforcing", acct: &Accountant{enforceFlag: true}, want: "acct"},
		{name: "wormchain log only", acct: &Accountant{}, want: "acct-logonly"},
		{name: "wormchain and ntt", acct: &Accountant{enforceFlag: true, nttContract: "x"}, want: "acct:ntt-acct"},
		{name: "solana enforcing", acct: &Accountant{enforceFlag: true, solana: backend}, want: "acct:sol-acct"},
		{name: "solana log only", acct: &Accountant{solana: backend}, want: "acct-logonly:sol-acct-logonly"},
		{name: "all three", acct: &Accountant{enforceFlag: true, nttContract: "x", solana: backend}, want: "acct:ntt-acct:sol-acct"},
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
		disableSolana bool
		isNTT         bool
		wantWormchain int
		wantSolana    int
		wantNtt       int
	}{
		{name: "wormchain only", wormchain: "0xdeadbeef", disableSolana: true, wantWormchain: 1},
		{name: "solana only", wormchain: "", wantSolana: 1},
		{name: "both backends", wormchain: "0xdeadbeef", wantWormchain: 1, wantSolana: 1},
		{name: "ntt entry stays on the ntt channel", wormchain: "0xdeadbeef", isNTT: true, wantNtt: 1},
	}

	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			acct, _, _ := newSolanaTestAccountant(t, ctx, solanaTestOpts{wormchainContract: tt.wormchain, disableSolana: tt.disableSolana, enforce: true})

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
		})
	}
}

func TestSubmitPendingIsPerBackend(t *testing.T) {
	ctx := context.Background()
	acct, _, _ := newSolanaTestAccountant(t, ctx, solanaTestOpts{wormchainContract: "0xdeadbeef", enforce: true})

	msg := solanaTestTransfer(t, 1)
	_, err := acct.SubmitObservation(msg)
	require.NoError(t, err)
	pe := acct.pendingTransfers[msg.MessageIDString()]
	require.NotNil(t, pe)

	require.True(t, acct.submitObservation(ctx, pe, backendWormchain, false))
	require.True(t, acct.submitObservation(ctx, pe, backendSolana, false))
	require.Equal(t, 1, len(acct.subChan))
	acct.clearSubmitPendingFlags([]*common.MessagePublication{<-acct.solana.subChan}, backendSolana)

	// The Solana batch leaves the wormchain submission pending. A Solana resubmit
	// queues to Solana only.
	assert.True(t, pe.submitPending(backendWormchain))
	require.True(t, acct.submitObservation(ctx, pe, backendSolana, false))
	assert.Equal(t, 1, len(acct.subChan))
	assert.Equal(t, 1, len(acct.solana.subChan))
}

func TestUnbuildableSolanaFieldsLeaveNoEntry(t *testing.T) {
	ctx := context.Background()
	msg := solanaTestTransfer(t, 7)
	msg.Payload = msg.Payload[:tokenBridgeTransferLen-1]

	t.Run("submit observation", func(t *testing.T) {
		acct, _, _ := newSolanaTestAccountant(t, ctx, solanaTestOpts{enforce: true})
		shouldPub, err := acct.SubmitObservation(msg)
		require.NoError(t, err)
		assert.False(t, shouldPub)
		assert.NotContains(t, acct.pendingTransfers, msg.MessageIDString())
	})

	t.Run("reload", func(t *testing.T) {
		acct, _, _ := newSolanaTestAccountant(t, ctx, solanaTestOpts{
			enforce: true,
			db:      &fakeAccountantDB{data: []*common.MessagePublication{msg}},
		})
		assert.NotContains(t, acct.pendingTransfers, msg.MessageIDString())
	})
}

func TestSolanaFieldsOnPendingTransfers(t *testing.T) {
	ctx := context.Background()
	msg := solanaTestTransfer(t, 42)

	t.Run("submit observation sets the record", func(t *testing.T) {
		acct, _, _ := newSolanaTestAccountant(t, ctx, solanaTestOpts{wormchainContract: "0xdeadbeef", enforce: true})
		_, err := acct.SubmitObservation(msg)
		require.NoError(t, err)

		pe := acct.pendingTransfers[msg.MessageIDString()]
		require.NotNil(t, pe)
		require.NotNil(t, pe.solanaFields)
		wantDigest, err := digestBytes(pe.digest)
		require.NoError(t, err)
		assert.Equal(t, wantDigest, pe.solanaFields.VaaDigest)
		assert.Equal(t, msg.EmitterChain, pe.solanaFields.Chain)
		assert.Equal(t, msg.Sequence, pe.solanaFields.Sequence)
	})

	t.Run("reload sets the record", func(t *testing.T) {
		acct, _, _ := newSolanaTestAccountant(t, ctx, solanaTestOpts{
			wormchainContract: "0xdeadbeef",
			enforce:           true,
			db:                &fakeAccountantDB{data: []*common.MessagePublication{msg}},
		})
		pe := acct.pendingTransfers[msg.MessageIDString()]
		require.NotNil(t, pe)
		assert.NotNil(t, pe.solanaFields)
	})

	t.Run("no record while solana is disabled", func(t *testing.T) {
		acct, _, _ := newSolanaTestAccountant(t, ctx, solanaTestOpts{
			wormchainContract: "0xdeadbeef",
			enforce:           true,
			disableSolana:     true,
			db:                &fakeAccountantDB{data: []*common.MessagePublication{msg}},
		})
		pe := acct.pendingTransfers[msg.MessageIDString()]
		require.NotNil(t, pe)
		assert.Nil(t, pe.solanaFields)
	})
}
