package accountant

import (
	"encoding/hex"
	"testing"

	"github.com/gagliardetto/solana-go"
	"github.com/stretchr/testify/assert"
	"github.com/stretchr/testify/require"
	"github.com/wormhole-foundation/wormhole/sdk/vaa"
)

// The fixture constants are in solana_layout_fixtures_gen_test.go.
func mustHexDecode(t *testing.T, s string) []byte {
	t.Helper()
	b, err := hex.DecodeString(s)
	require.NoError(t, err)
	return b
}

func mustHexDecode32(t *testing.T, s string) [32]byte {
	t.Helper()
	b := mustHexDecode(t, s)
	require.Len(t, b, 32)
	return [32]byte(b)
}

func fixtureProgramID() solana.PublicKey {
	var pk [32]byte
	for i := range pk {
		pk[i] = byte(i)
	}
	return pk
}

func fixtureEmitter() vaa.Address {
	var a vaa.Address
	for i := range a {
		a[i] = 0x40 + byte(i)
	}
	return a
}

func fixtureDigest() [32]byte {
	var d [32]byte
	for i := range d {
		d[i] = 0x80 + byte(i)
	}
	return d
}

// fixturePayload returns the payload of a mainnet VAA body fixture.
func fixturePayload(t *testing.T, bodyHex string) []byte {
	t.Helper()
	body := mustHexDecode(t, bodyHex)
	require.Greater(t, len(body), fixtureVaaBodyHeaderLen)
	return body[fixtureVaaBodyHeaderLen:]
}

func TestParseAccountantDigestLog(t *testing.T) {
	valid := mustHexDecode(t, fixtureACCDGSTLogHex)
	require.Len(t, valid, accountantDigestLogLen)

	tests := []struct {
		name    string
		data    []byte
		wantErr bool
	}{
		{name: "valid", data: valid, wantErr: false},
		{name: "wrong tag", data: append([]byte{0}, valid[1:]...), wantErr: true},
		{name: "85 bytes", data: valid[:85], wantErr: true},
		{name: "87 bytes", data: append(append([]byte{}, valid...), 0x00), wantErr: true},
	}

	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			evt, err := parseAccountantDigestLog(tt.data)
			if tt.wantErr {
				assert.Error(t, err)
				assert.Nil(t, evt)
				return
			}
			require.NoError(t, err)
			require.NotNil(t, evt)
			assert.Equal(t, vaa.ChainID(2), evt.Chain)
			assert.Equal(t, fixtureEmitter(), evt.Emitter)
			assert.Equal(t, uint64(100_000), evt.Sequence)
			assert.Equal(t, fixtureDigest(), evt.Digest)
			assert.Equal(t, uint32(4), evt.GuardianSetIndex)
		})
	}
}

func TestParsePendingObservationsAccount(t *testing.T) {
	valid := mustHexDecode(t, fixturePendingObservationsAccountHex)
	require.Len(t, valid, pendingObservationsLen)

	wrongTag := append([]byte{}, valid...)
	wrongTag[0] = 2 // AccountTag::Balance

	tests := []struct {
		name    string
		data    []byte
		wantErr bool
	}{
		{name: "valid", data: valid, wantErr: false},
		{name: "wrong tag", data: wrongTag, wantErr: true},
		{name: "87 bytes", data: valid[:len(valid)-1], wantErr: true},
		{name: "89 bytes", data: append(append([]byte{}, valid...), 0x00), wantErr: true},
	}

	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			obs, err := parsePendingObservationsAccount(tt.data)
			if tt.wantErr {
				assert.Error(t, err)
				assert.Nil(t, obs)
				return
			}
			require.NoError(t, err)
			require.NotNil(t, obs)
			assert.Equal(t, vaa.ChainID(2), obs.Chain)
			assert.Equal(t, uint32(4), obs.GuardianSetIndex)
			assert.Equal(t, [4]uint32{0x29, 0x2, 0x1, 0x8000_0000}, obs.Signatures)
			assert.Equal(t, fixtureDigest(), obs.ContentDigest)

			var wantPayer [32]byte
			for i := range wantPayer {
				wantPayer[i] = 0xC0 + byte(i)
			}
			assert.Equal(t, solana.PublicKey(wantPayer), obs.Payer)
		})
	}
}

func TestPendingObservationsHasSignature(t *testing.T) {
	valid := mustHexDecode(t, fixturePendingObservationsAccountHex)
	obs, err := parsePendingObservationsAccount(valid)
	require.NoError(t, err)

	tests := []struct {
		name    string
		index   uint8
		want    bool
		wantErr bool
	}{
		{name: "index 0 set", index: 0, want: true},
		{name: "index 33 set", index: 33, want: true},
		{name: "index 64 set", index: 64, want: true},
		{name: "index 127 set", index: 127, want: true},
		{name: "index 1 clear", index: 1, want: false},
		{name: "index 32 clear", index: 32, want: false},
		{name: "index 128 out of range", index: 128, wantErr: true},
	}

	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			got, err := obs.hasSignature(tt.index)
			if tt.wantErr {
				assert.Error(t, err)
				return
			}
			require.NoError(t, err)
			assert.Equal(t, tt.want, got)
		})
	}
}

func TestDerivePendingObservationsPDA(t *testing.T) {
	pda, err := derivePendingObservationsPDA(
		fixtureProgramID(),
		vaa.ChainIDSolana,
		vaa.Address(mustHexDecode32(t, fixtureTransferEmitterHex)),
		fixtureTransferSequence,
		fixtureTransferGuardianSetIndex,
		mustHexDecode32(t, fixtureTransferContentDigestHex),
	)
	require.NoError(t, err)
	assert.Equal(t, fixtureTransferPendingPDAHex, hex.EncodeToString(pda[:]))
}

func TestDeriveNoreplayBucketPDA(t *testing.T) {
	authority, err := deriveNoreplayAuthorityPDA(fixtureProgramID())
	require.NoError(t, err)
	assert.Equal(t, fixtureNoreplayAuthorityPDAHex, hex.EncodeToString(authority[:]))

	// NOREPLAY_PROGRAM_ID, svm/accountant/crates/definitions/src/constants/noreplay.rs.
	noreplayProgram := solana.MustPublicKeyFromBase58("repMHgR5BEpGLeZvM5iGoNNDPw4eu2BS6sXJzaC8K4t")
	const boundaryChain = vaa.ChainID(56)

	tests := []struct {
		name     string
		sequence uint64
		wantHex  string
	}{
		{name: "sequence 1023", sequence: 1023, wantHex: fixtureNoreplayBucketPDASeq1023Hex},
		{name: "sequence 1024", sequence: 1024, wantHex: fixtureNoreplayBucketPDASeq1024Hex},
	}

	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			pda, err := deriveNoreplayBucketPDA(noreplayProgram, authority, boundaryChain, fixtureEmitter(), tt.sequence)
			require.NoError(t, err)
			assert.Equal(t, tt.wantHex, hex.EncodeToString(pda[:]))
		})
	}
}

func TestNoreplayBitSet(t *testing.T) {
	tests := []struct {
		name       string
		bucketData []byte
		sequence   uint64
		want       bool
		wantErr    bool
	}{
		{name: "bit 0 set, bit 1 queried", bucketData: solanaNoreplayBucket(0), sequence: 1, want: false},
		{name: "bit 1023 set, bit 1023 queried", bucketData: solanaNoreplayBucket(1023), sequence: 1023, want: true},
		{name: "sequence 1024 queries bit 0 of the next bucket's page", bucketData: solanaNoreplayBucket(0), sequence: 1024, want: true},
		{name: "byte boundary: bit 7 set, bit 8 clear", bucketData: solanaNoreplayBucket(7), sequence: 8, want: false},
		{name: "byte boundary: bit 8 set", bucketData: solanaNoreplayBucket(8), sequence: 8, want: true},
		{name: "wrong length: too short", bucketData: make([]byte, noreplayBucketLen-1), sequence: 0, wantErr: true},
	}

	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			got, err := noreplayBitSet(tt.bucketData, tt.sequence)
			if tt.wantErr {
				assert.Error(t, err)
				return
			}
			require.NoError(t, err)
			assert.Equal(t, tt.want, got)
		})
	}
}

func TestParseSubmitObservationsIxData(t *testing.T) {
	valid := mustHexDecode(t, fixtureSubmitObservationsIxDataHex)
	require.Len(t, valid, submitObservationsDispatchLen+submitObservationsIxDataLen)
	validSignature := mustHexDecode(t, fixtureSubmitObservationsSignatureTxIDIxDataHex)
	require.Len(t, validSignature, submitObservationsDispatchLen+submitObservationsIxDataLen)

	mutate := func(data []byte, edit func([]byte)) []byte {
		out := append([]byte{}, data...)
		edit(out)
		return out
	}

	tests := []struct {
		name     string
		data     []byte
		wantTxID []byte
		wantErr  bool
	}{
		{name: "valid, 32-byte tx id", data: valid, wantTxID: mustHexDecode(t, fixtureSubmitObservationsTxIDHex)},
		{name: "valid, 64-byte tx id", data: validSignature, wantTxID: mustHexDecode(t, fixtureSubmitObservationsSignatureTxIDHex)},
		{name: "wrong discriminator", data: mutate(valid, func(d []byte) { d[0] = 2 }), wantErr: true}, // Instruction::SubmitVaas
		{name: "278 bytes", data: valid[:len(valid)-1], wantErr: true},
		{name: "tx id length 33", data: mutate(valid, func(d []byte) { d[submitTxIDLenOffset] = 33 }), wantErr: true},
		{name: "32-byte tx id, nonzero last padding byte", data: mutate(valid, func(d []byte) { d[submitFieldsOffset-1] = 1 }), wantErr: true},
	}

	var wantSignature [65]byte
	for i := range wantSignature {
		wantSignature[i] = byte(i)
	}

	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			ix, err := parseSubmitObservationsIxData(tt.data)
			if tt.wantErr {
				assert.Error(t, err)
				assert.Nil(t, ix)
				return
			}
			require.NoError(t, err)
			require.NotNil(t, ix)
			assert.Equal(t, uint32(4), ix.GuardianSetIndex)
			assert.Equal(t, uint8(3), ix.GuardianIndex)
			assert.Equal(t, wantSignature, ix.Signature)
			assert.True(t, ix.TxID.valid())
			assert.Equal(t, tt.wantTxID, ix.TxID.Bytes())

			packed := ix.pack()
			assert.Equal(t, fixtureTransferFieldsHex, hex.EncodeToString(packed[:]))
			assert.Equal(t, mustHexDecode32(t, fixtureTransferContentDigestHex), ix.contentDigest)
		})
	}
}

func TestNewSolanaTxID(t *testing.T) {
	tests := []struct {
		name    string
		id      []byte
		wantErr bool
	}{
		{name: "32 bytes", id: make([]byte, hashTxIDLen)},
		{name: "64 bytes", id: make([]byte, signatureTxIDLen)},
		{name: "33 bytes", id: make([]byte, hashTxIDLen+1), wantErr: true},
	}
	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			for i := range tt.id {
				tt.id[i] = byte(i + 1)
			}
			txID, err := newSolanaTxID(tt.id)
			if tt.wantErr {
				assert.Error(t, err)
				assert.False(t, txID.valid())
				return
			}
			require.NoError(t, err)
			assert.True(t, txID.valid())
			assert.Equal(t, tt.id, txID.Bytes())
		})
	}
	assert.False(t, solanaTxID{}.valid(), "the zero value is not a tx id")
}

func TestSolanaObservationFieldsFromPayload(t *testing.T) {
	transfer := fixturePayload(t, fixtureTransferBodyHex)
	require.Len(t, transfer, tokenBridgeTransferLen)

	transferPlusMax := append(append([]byte{}, transfer...), make([]byte, maxTransferPayloadLen)...)
	transferPlusTooMuch := append(append([]byte{}, transfer...), make([]byte, maxTransferPayloadLen+1)...)

	// Unset fields default to the mainnet transfer.
	tests := []struct {
		name              string
		emitterHex        string
		sequence          uint64
		payload           []byte
		vaaDigestHex      string
		wantFieldsHex     string
		wantContentDigest string
		wantErr           bool
	}{
		{name: "mainnet transfer", payload: transfer},
		{name: "transfer with the maximum extra payload", payload: transferPlusMax},
		{
			name:              "mainnet action 0x99",
			emitterHex:        fixtureOtherEmitterHex,
			sequence:          fixtureOtherSequence,
			payload:           fixturePayload(t, fixtureOtherBodyHex),
			vaaDigestHex:      fixtureOtherVaaDigestHex,
			wantFieldsHex:     fixtureOtherFieldsHex,
			wantContentDigest: fixtureOtherContentDigestHex,
		},
		{name: "empty payload", payload: []byte{}, wantErr: true},
		{name: "transfer one byte short of the fixed head", payload: transfer[:tokenBridgeTransferLen-1], wantErr: true},
		{name: "transfer one byte past the extra-payload bound", payload: transferPlusTooMuch, wantErr: true},
	}

	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			emitterHex, sequence, vaaDigestHex := fixtureTransferEmitterHex, fixtureTransferSequence, fixtureTransferVaaDigestHex
			wantFieldsHex, wantContentDigest := fixtureTransferFieldsHex, fixtureTransferContentDigestHex
			if tt.emitterHex != "" {
				emitterHex, sequence, vaaDigestHex = tt.emitterHex, tt.sequence, tt.vaaDigestHex
				wantFieldsHex, wantContentDigest = tt.wantFieldsHex, tt.wantContentDigest
			}

			fields, err := solanaObservationFieldsFromPayload(vaa.ChainIDSolana, vaa.Address(mustHexDecode32(t, emitterHex)), sequence, tt.payload, mustHexDecode32(t, vaaDigestHex))
			if tt.wantErr {
				assert.Error(t, err)
				assert.Nil(t, fields)
				return
			}
			require.NoError(t, err)
			require.NotNil(t, fields)
			packed := fields.pack()
			assert.Equal(t, wantFieldsHex, hex.EncodeToString(packed[:]))
			assert.Equal(t, mustHexDecode32(t, wantContentDigest), fields.contentDigest)
		})
	}
}

// Values are the ERR_* lines of go_fixture_vectors.rs.
func TestSolanaErrorCodesMatchProgram(t *testing.T) {
	tests := []struct {
		name string
		got  uint32
		want uint32
	}{
		{"ERR_PAYER_MISMATCH", solanaErrPayerMismatch, 4},
		{"ERR_ALREADY_ACCOUNTED", solanaErrAlreadyAccounted, 7},
		{"ERR_INVALID_SIGNATURE", solanaErrInvalidSignature, 9},
		{"ERR_INVALID_GUARDIAN_INDEX", solanaErrInvalidGuardianIndex, 10},
		{"ERR_ALREADY_SIGNED", solanaErrAlreadySigned, 11},
		{"ERR_EXPIRED_GUARDIAN_SET", solanaErrExpiredGuardianSet, 12},
		{"ERR_MISSING_CHAIN_REGISTRATION", solanaErrMissingChainRegistration, 19},
		{"ERR_UNREGISTERED_EMITTER", solanaErrUnregisteredEmitter, 20},
	}
	for _, tc := range tests {
		t.Run(tc.name, func(t *testing.T) {
			assert.Equal(t, tc.want, tc.got)
		})
	}
}
