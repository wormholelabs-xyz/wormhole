package accountant

import (
	"encoding/hex"
	"math/big"
	"testing"

	"github.com/gagliardetto/solana-go"
	"github.com/stretchr/testify/assert"
	"github.com/stretchr/testify/require"
	"github.com/wormhole-foundation/wormhole/sdk/vaa"
)

// svm/accountant/programs/global-accountant/tests/go_fixture_vectors.rs prints the fixtures:
//
//	cd svm/accountant && just go-fixtures
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

// VaaBodyHeader::LEN, crates/definitions/src/vaa.rs. The payload follows it.
const fixtureVaaBodyHeaderLen = 51

const (
	fixturePendingObservationsAccountHex = "010002000400000029000000020000000100000000000080808182838485868788898a8b8c8d8e8f909192939495969798999a9b9c9d9e9fc0c1c2c3c4c5c6c7c8c9cacbcccdcecfd0d1d2d3d4d5d6d7d8d9dadbdcdddedf"
	fixturePendingPDAHex                 = "348f55786a5496ba40c0e28d05705814e35173bc1bb25d02a1e58a5d80a98fe0"
	fixtureNoreplayAuthorityPDAHex       = "26a205d51ce014d75374204dfdd2d1e20ed4f9483698d53cb48a02e1ff38c048"
	fixtureNoreplayBucketPDASeq1023Hex   = "45127b068360647b21f422e2ac01a7c710dabb76674a6f54312634c5cd489e71"
	fixtureNoreplayBucketPDASeq1024Hex   = "be0c9a2efc9da5d71ff732e6427c324c4465d05134845c014a8d26ff8870721c"
	fixtureACCDGSTLogHex                 = "41434344475354000002404142434445464748494a4b4c4d4e4f505152535455565758595a5b5c5d5e5f00000000000186a0808182838485868788898a8b8c8d8e8f909192939495969798999a9b9c9d9e9f04000000"
)

// Mainnet Solana Token Bridge transfer, sequence 1395207, guardian set 6.
const (
	fixtureTransferBodyHex          = "6a0ceb5c000000000001ec7372995d5cc8732397fb0ad35c0121e0eaa90d26f828a534cab54391b3a4f50000000000154a0720010000000000000000000000000000000000000000000000000000017a3782f44a000000000000000000000000814e0908b12a99fecf5bc101bb5d0b8b5cdf7d260002000000000000000000000000d9f76930d7df99aef4dc4cff48ee236d4583c28a00020000000000000000000000000000000000000000000000000000000000000000"
	fixtureTransferEmitterHex       = "ec7372995d5cc8732397fb0ad35c0121e0eaa90d26f828a534cab54391b3a4f5"
	fixtureTransferVaaDigestHex     = "89c41f5ac9c35ba9d15bf358b931d6f754a77349022d0a64f13962078f632a39"
	fixtureTransferFieldsHex        = "010001ec7372995d5cc8732397fb0ad35c0121e0eaa90d26f828a534cab54391b3a4f50000000000154a070002000000000000000000000000814e0908b12a99fecf5bc101bb5d0b8b5cdf7d2600020000000000000000000000000000000000000000000000000000017a3782f44a89c41f5ac9c35ba9d15bf358b931d6f754a77349022d0a64f13962078f632a39"
	fixtureTransferContentDigestHex = "534e4da8419f27c2d2ea93913e0f6492ec991fac607b905fffe0d1b16c82111d"
	fixtureTransferPendingPDAHex    = "8b89ff0436841aad684856dc16fbcf536e880633ad8564b568126ff43ab23ff4"
	fixtureTransferChain            = vaa.ChainID(1)
	fixtureTransferSequence         = uint64(1_395_207)
	fixtureTransferGuardianSetIndex = uint32(6)
)

// Mainnet Solana Token Bridge message with non-transfer action 0x99, sequence 2211.
const (
	fixtureOtherBodyHex          = "6a0ce8fd0000000000014385cebf45845f3a162f42c96a3dfe696b7eb8368f1af1e7613f870af36f1fc600000000000008a3209945ff100bc1cbe9c2b8f484c234b5c6814d54d355cf3cd4515c26e4d4519ba4c3718d500000000000000000000000006915fe8dad5d32c2ee961e2f432d7dd5916316de00916c556f4cf35829572e5264ae5bb30feddc003ddd63809d7531cc9d4b695881611a35b5038601ea71ab990524577c4b8016d76d8b73e9eead7ae1e71c8d8fe03a004f994e545408000000d0073147e508e502462eb4cdb735b83acc7e9c54a35e07ad518596e5efe963ee889cd1556b000000000000000000000000d9f76930d7df99aef4dc4cff48ee236d4583c28a00040000"
	fixtureOtherEmitterHex       = "4385cebf45845f3a162f42c96a3dfe696b7eb8368f1af1e7613f870af36f1fc6"
	fixtureOtherVaaDigestHex     = "e4cac284656ac74ad4ef1b0ec7c2be76289705458071c7ddbef805499a054116"
	fixtureOtherFieldsHex        = "9900014385cebf45845f3a162f42c96a3dfe696b7eb8368f1af1e7613f870af36f1fc600000000000008a30000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000e4cac284656ac74ad4ef1b0ec7c2be76289705458071c7ddbef805499a054116"
	fixtureOtherContentDigestHex = "01717e74a82fc2625fa902ae4813e83ea5c875776dffd459e626eeb804d4d400"
	fixtureOtherChain            = vaa.ChainID(1)
	fixtureOtherSequence         = uint64(2211)
)

// submit_observations data for the transfer body, guardian set 4, guardian index 3: one with
// a 32-byte tx id, one with a 64-byte tx id.
const (
	fixtureSubmitObservationsIxDataHex              = "000400000003000102030405060708090a0b0c0d0e0f101112131415161718191a1b1c1d1e1f202122232425262728292a2b2c2d2e2f303132333435363738393a3b3c3d3e3f4020e0e1e2e3e4e5e6e7e8e9eaebecedeeeff0f1f2f3f4f5f6f7f8f9fafbfcfdfeff0000000000000000000000000000000000000000000000000000000000000000010001ec7372995d5cc8732397fb0ad35c0121e0eaa90d26f828a534cab54391b3a4f50000000000154a070002000000000000000000000000814e0908b12a99fecf5bc101bb5d0b8b5cdf7d2600020000000000000000000000000000000000000000000000000000017a3782f44a89c41f5ac9c35ba9d15bf358b931d6f754a77349022d0a64f13962078f632a39"
	fixtureSubmitObservationsTxIDHex                = "e0e1e2e3e4e5e6e7e8e9eaebecedeeeff0f1f2f3f4f5f6f7f8f9fafbfcfdfeff"
	fixtureSubmitObservationsSignatureTxIDIxDataHex = "000400000003000102030405060708090a0b0c0d0e0f101112131415161718191a1b1c1d1e1f202122232425262728292a2b2c2d2e2f303132333435363738393a3b3c3d3e3f4040a0a1a2a3a4a5a6a7a8a9aaabacadaeafb0b1b2b3b4b5b6b7b8b9babbbcbdbebfc0c1c2c3c4c5c6c7c8c9cacbcccdcecfd0d1d2d3d4d5d6d7d8d9dadbdcdddedf010001ec7372995d5cc8732397fb0ad35c0121e0eaa90d26f828a534cab54391b3a4f50000000000154a070002000000000000000000000000814e0908b12a99fecf5bc101bb5d0b8b5cdf7d2600020000000000000000000000000000000000000000000000000000017a3782f44a89c41f5ac9c35ba9d15bf358b931d6f754a77349022d0a64f13962078f632a39"
	fixtureSubmitObservationsSignatureTxIDHex       = "a0a1a2a3a4a5a6a7a8a9aaabacadaeafb0b1b2b3b4b5b6b7b8b9babbbcbdbebfc0c1c2c3c4c5c6c7c8c9cacbcccdcecfd0d1d2d3d4d5d6d7d8d9dadbdcdddedf"
)

// transferPayload returns the token-bridge payload of the mainnet transfer fixture.
func transferPayload(t *testing.T) []byte {
	t.Helper()
	body := mustHexDecode(t, fixtureTransferBodyHex)
	require.Greater(t, len(body), fixtureVaaBodyHeaderLen)
	return body[fixtureVaaBodyHeaderLen:]
}

// otherPayload returns the action-0x99 payload of the mainnet fixture.
func otherPayload(t *testing.T) []byte {
	t.Helper()
	body := mustHexDecode(t, fixtureOtherBodyHex)
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
		{name: "empty", data: []byte{}, wantErr: true},
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
		{name: "empty", data: []byte{}, wantErr: true},
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
		{name: "index 3 set", index: 3, want: true},
		{name: "index 5 set", index: 5, want: true},
		{name: "index 33 set", index: 33, want: true},
		{name: "index 64 set", index: 64, want: true},
		{name: "index 127 set", index: 127, want: true},
		{name: "index 1 clear", index: 1, want: false},
		{name: "index 31 clear", index: 31, want: false},
		{name: "index 32 clear", index: 32, want: false},
		{name: "index 63 clear", index: 63, want: false},
		{name: "index 126 clear", index: 126, want: false},
		{name: "index 128 out of range", index: 128, wantErr: true},
		{name: "index 255 out of range", index: 255, wantErr: true},
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
	transferEmitter := vaa.Address(mustHexDecode32(t, fixtureTransferEmitterHex))
	transferDigest := mustHexDecode32(t, fixtureTransferContentDigestHex)

	tests := []struct {
		name             string
		chain            vaa.ChainID
		emitter          vaa.Address
		sequence         uint64
		guardianSetIndex uint32
		contentDigest    [32]byte
		wantHex          string
	}{
		{
			name:             "synthetic vector at guardian set 4",
			chain:            vaa.ChainID(2),
			emitter:          fixtureEmitter(),
			sequence:         100_000,
			guardianSetIndex: 4,
			contentDigest:    fixtureDigest(),
			wantHex:          fixturePendingPDAHex,
		},
		{
			name:             "mainnet transfer at guardian set 6",
			chain:            fixtureTransferChain,
			emitter:          transferEmitter,
			sequence:         fixtureTransferSequence,
			guardianSetIndex: fixtureTransferGuardianSetIndex,
			contentDigest:    transferDigest,
			wantHex:          fixtureTransferPendingPDAHex,
		},
	}

	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			pda, err := derivePendingObservationsPDA(fixtureProgramID(), tt.chain, tt.emitter, tt.sequence, tt.guardianSetIndex, tt.contentDigest)
			require.NoError(t, err)
			assert.Equal(t, tt.wantHex, hex.EncodeToString(pda[:]))
		})
	}

	// A guardian-set rotation opens a sibling record.
	rotated, err := derivePendingObservationsPDA(fixtureProgramID(), fixtureTransferChain, transferEmitter, fixtureTransferSequence, fixtureTransferGuardianSetIndex+1, transferDigest)
	require.NoError(t, err)
	assert.NotEqual(t, fixtureTransferPendingPDAHex, hex.EncodeToString(rotated[:]))
}

func TestDeriveNoreplayAuthorityPDA(t *testing.T) {
	pda, err := deriveNoreplayAuthorityPDA(fixtureProgramID())
	require.NoError(t, err)
	assert.Equal(t, fixtureNoreplayAuthorityPDAHex, hex.EncodeToString(pda[:]))
}

func TestDeriveNoreplayBucketPDA(t *testing.T) {
	authority, err := deriveNoreplayAuthorityPDA(fixtureProgramID())
	require.NoError(t, err)

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

	pda1023, err := deriveNoreplayBucketPDA(noreplayProgram, authority, boundaryChain, fixtureEmitter(), 1023)
	require.NoError(t, err)
	pda1024, err := deriveNoreplayBucketPDA(noreplayProgram, authority, boundaryChain, fixtureEmitter(), 1024)
	require.NoError(t, err)
	assert.NotEqual(t, pda1023, pda1024, "sequence 1023 and 1024 must fall in different buckets")
}

func TestNoreplayBitSet(t *testing.T) {
	makeBucket := func(setBits ...uint64) []byte {
		buf := make([]byte, noreplayBucketLen)
		for _, bit := range setBits {
			buf[noreplayBitmapOffset+int(bit/8)] |= 1 << (bit % 8)
		}
		return buf
	}

	tests := []struct {
		name       string
		bucketData []byte
		sequence   uint64
		want       bool
		wantErr    bool
	}{
		{name: "bit 0 set, bit 0 queried", bucketData: makeBucket(0), sequence: 0, want: true},
		{name: "bit 0 set, bit 1 queried", bucketData: makeBucket(0), sequence: 1, want: false},
		{name: "bit 1023 set, bit 1023 queried", bucketData: makeBucket(1023), sequence: 1023, want: true},
		{name: "bit 1023 set, sequence 1024 wraps to bit 0", bucketData: makeBucket(1023), sequence: 1024, want: false},
		{name: "sequence 1024 queries bit 0 of the next bucket's page", bucketData: makeBucket(0), sequence: 1024, want: true},
		{name: "byte boundary: bit 7 set, bit 8 clear", bucketData: makeBucket(7), sequence: 8, want: false},
		{name: "byte boundary: bit 8 set", bucketData: makeBucket(8), sequence: 8, want: true},
		{name: "wrong length: too short", bucketData: make([]byte, noreplayBucketLen-1), sequence: 0, wantErr: true},
		{name: "wrong length: too long", bucketData: make([]byte, noreplayBucketLen+1), sequence: 0, wantErr: true},
		{name: "wrong length: empty", bucketData: []byte{}, sequence: 0, wantErr: true},
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
		{name: "280 bytes", data: append(append([]byte{}, valid...), 0x00), wantErr: true},
		{name: "empty", data: []byte{}, wantErr: true},
		{name: "tx id length 0", data: mutate(valid, func(d []byte) { d[submitTxIDLenOffset] = 0 }), wantErr: true},
		{name: "tx id length 33", data: mutate(valid, func(d []byte) { d[submitTxIDLenOffset] = 33 }), wantErr: true},
		{name: "tx id length 65", data: mutate(validSignature, func(d []byte) { d[submitTxIDLenOffset] = 65 }), wantErr: true},
		{name: "32-byte tx id, nonzero last padding byte", data: mutate(valid, func(d []byte) { d[submitFieldsOffset-1] = 1 }), wantErr: true},
		{name: "32-byte tx id, nonzero first padding byte", data: mutate(valid, func(d []byte) { d[submitTxIDOffset+hashTxIDLen] = 1 }), wantErr: true},
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

			assert.Equal(t, uint8(0x01), ix.Action)
			assert.Equal(t, fixtureTransferChain, ix.Chain)
			assert.Equal(t, vaa.Address(mustHexDecode32(t, fixtureTransferEmitterHex)), ix.Emitter)
			assert.Equal(t, fixtureTransferSequence, ix.Sequence)
			assert.Equal(t, vaa.ChainID(2), ix.TokenChain)
			assert.Equal(t, vaa.ChainID(2), ix.RecipientChain)
			assert.Equal(t, mustHexDecode32(t, fixtureTransferVaaDigestHex), ix.VaaDigest)

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
		{name: "empty", id: []byte{}, wantErr: true},
		{name: "31 bytes", id: make([]byte, hashTxIDLen-1), wantErr: true},
		{name: "33 bytes", id: make([]byte, hashTxIDLen+1), wantErr: true},
		{name: "65 bytes", id: make([]byte, signatureTxIDLen+1), wantErr: true},
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
	transfer := transferPayload(t)
	require.Len(t, transfer, tokenBridgeTransferLen)

	transferPlusMax := append(append([]byte{}, transfer...), make([]byte, maxTransferPayloadLen)...)
	transferPlusTooMuch := append(append([]byte{}, transfer...), make([]byte, maxTransferPayloadLen+1)...)

	transferEmitter := vaa.Address(mustHexDecode32(t, fixtureTransferEmitterHex))
	otherEmitter := vaa.Address(mustHexDecode32(t, fixtureOtherEmitterHex))

	tests := []struct {
		name                string
		chain               vaa.ChainID
		emitter             vaa.Address
		sequence            uint64
		payload             []byte
		vaaDigest           [32]byte
		wantFieldsHex       string
		wantContentDigest   string
		wantZeroTransferSet bool
		wantErr             bool
	}{
		{
			name:              "mainnet transfer",
			chain:             fixtureTransferChain,
			emitter:           transferEmitter,
			sequence:          fixtureTransferSequence,
			payload:           transfer,
			vaaDigest:         mustHexDecode32(t, fixtureTransferVaaDigestHex),
			wantFieldsHex:     fixtureTransferFieldsHex,
			wantContentDigest: fixtureTransferContentDigestHex,
		},
		{
			name:              "transfer with the maximum extra payload",
			chain:             fixtureTransferChain,
			emitter:           transferEmitter,
			sequence:          fixtureTransferSequence,
			payload:           transferPlusMax,
			vaaDigest:         mustHexDecode32(t, fixtureTransferVaaDigestHex),
			wantFieldsHex:     fixtureTransferFieldsHex,
			wantContentDigest: fixtureTransferContentDigestHex,
		},
		{
			name:                "mainnet action 0x99",
			chain:               fixtureOtherChain,
			emitter:             otherEmitter,
			sequence:            fixtureOtherSequence,
			payload:             otherPayload(t),
			vaaDigest:           mustHexDecode32(t, fixtureOtherVaaDigestHex),
			wantFieldsHex:       fixtureOtherFieldsHex,
			wantContentDigest:   fixtureOtherContentDigestHex,
			wantZeroTransferSet: true,
		},
		{
			name:     "empty payload",
			chain:    fixtureTransferChain,
			emitter:  transferEmitter,
			sequence: fixtureTransferSequence,
			payload:  []byte{},
			wantErr:  true,
		},
		{
			name:     "transfer one byte short of the fixed head",
			chain:    fixtureTransferChain,
			emitter:  transferEmitter,
			sequence: fixtureTransferSequence,
			payload:  transfer[:tokenBridgeTransferLen-1],
			wantErr:  true,
		},
		{
			name:     "transfer one byte past the extra-payload bound",
			chain:    fixtureTransferChain,
			emitter:  transferEmitter,
			sequence: fixtureTransferSequence,
			payload:  transferPlusTooMuch,
			wantErr:  true,
		},
	}

	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			fields, err := solanaObservationFieldsFromPayload(tt.chain, tt.emitter, tt.sequence, tt.payload, tt.vaaDigest)
			if tt.wantErr {
				assert.Error(t, err)
				assert.Nil(t, fields)
				return
			}
			require.NoError(t, err)
			require.NotNil(t, fields)
			packed := fields.pack()
			assert.Equal(t, tt.wantFieldsHex, hex.EncodeToString(packed[:]))
			assert.Equal(t, mustHexDecode32(t, tt.wantContentDigest), fields.contentDigest)
			if tt.wantZeroTransferSet {
				assert.Equal(t, vaa.ChainID(0), fields.TokenChain)
				assert.Equal(t, [32]byte{}, fields.TokenAddress)
				assert.Equal(t, vaa.ChainID(0), fields.RecipientChain)
				assert.Equal(t, [32]byte{}, fields.Amount)
			}
		})
	}
}

// The transfer offsets must agree with the guardian SDK's own decoder.
func TestSolanaObservationFieldsMatchSDKTransferHeader(t *testing.T) {
	payload := transferPayload(t)
	fields, err := solanaObservationFieldsFromPayload(
		fixtureTransferChain,
		vaa.Address(mustHexDecode32(t, fixtureTransferEmitterHex)),
		fixtureTransferSequence,
		payload,
		mustHexDecode32(t, fixtureTransferVaaDigestHex),
	)
	require.NoError(t, err)

	hdr, err := vaa.DecodeTransferPayloadHdr(payload)
	require.NoError(t, err)

	assert.Equal(t, hdr.Type, fields.Action)
	assert.Equal(t, hdr.OriginChain, fields.TokenChain)
	assert.Equal(t, vaa.Address(fields.TokenAddress), hdr.OriginAddress)
	assert.Equal(t, hdr.TargetChain, fields.RecipientChain)
	assert.Equal(t, 0, hdr.Amount.Cmp(new(big.Int).SetBytes(fields.Amount[:])))
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
