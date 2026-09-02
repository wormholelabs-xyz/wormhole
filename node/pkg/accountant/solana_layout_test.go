package accountant

import (
	"encoding/hex"
	"testing"

	"github.com/gagliardetto/solana-go"
	"github.com/stretchr/testify/assert"
	"github.com/stretchr/testify/require"
	"github.com/wormhole-foundation/wormhole/sdk/vaa"
)

// Fixtures are printed by svm/accountant/programs/global-accountant/tests/go_fixture_vectors.rs:
//
//	cargo test -p global-accountant --test go_fixture_vectors -- --nocapture
func mustHexDecode(t *testing.T, s string) []byte {
	t.Helper()
	b, err := hex.DecodeString(s)
	require.NoError(t, err)
	return b
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

const (
	fixturePendingObservationsAccountHex = "010002000400000029000000808182838485868788898a8b8c8d8e8f909192939495969798999a9b9c9d9e9fc0c1c2c3c4c5c6c7c8c9cacbcccdcecfd0d1d2d3d4d5d6d7d8d9dadbdcdddedf"
	fixturePendingPDAHex                 = "891ed1626c19c4fbcc568edc633725bd2f210aa3fcae9e6cdd18f5b0b11e2bcc"
	fixtureNoreplayAuthorityPDAHex       = "26a205d51ce014d75374204dfdd2d1e20ed4f9483698d53cb48a02e1ff38c048"
	fixtureNoreplayBucketPDASeq1023Hex   = "45127b068360647b21f422e2ac01a7c710dabb76674a6f54312634c5cd489e71"
	fixtureNoreplayBucketPDASeq1024Hex   = "be0c9a2efc9da5d71ff732e6427c324c4465d05134845c014a8d26ff8870721c"
	fixtureACCDGSTLogHex                 = "41434344475354000002404142434445464748494a4b4c4d4e4f505152535455565758595a5b5c5d5e5f00000000000186a0808182838485868788898a8b8c8d8e8f909192939495969798999a9b9c9d9e9f04000000"
	fixtureSubmitObservationsIxDataHex   = "000400000003000102030405060708090a0b0c0d0e0f101112131415161718191a1b1c1d1e1f202122232425262728292a2b2c2d2e2f303132333435363738393a3b3c3d3e3f40e0e1e2e3e4e5e6e7e8e9eaebecedeeeff0f1f2f3f4f5f6f7f8f9fafbfcfdfeff340000000000000000000002404142434445464748494a4b4c4d4e4f505152535455565758595a5b5c5d5e5f00000000000186a00100"
	fixtureSubmitObservationsTxHashHex   = "e0e1e2e3e4e5e6e7e8e9eaebecedeeeff0f1f2f3f4f5f6f7f8f9fafbfcfdfeff"
)

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
		{name: "one byte short", data: valid[:len(valid)-1], wantErr: true},
		{name: "one byte long", data: append(append([]byte{}, valid...), 0x00), wantErr: true},
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
			assert.Equal(t, uint32(0b0010_1001), obs.Signatures)
			assert.Equal(t, fixtureDigest(), obs.Digest)

			var wantPayer [32]byte
			for i := range wantPayer {
				wantPayer[i] = 0xC0 + byte(i)
			}
			assert.Equal(t, solana.PublicKey(wantPayer), obs.Payer)
		})
	}
}

func TestDerivePendingObservationsPDA(t *testing.T) {
	pda, err := derivePendingObservationsPDA(fixtureProgramID(), vaa.ChainID(2), fixtureEmitter(), 100_000, fixtureDigest())
	require.NoError(t, err)
	assert.Equal(t, fixturePendingPDAHex, hex.EncodeToString(pda[:]))
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

func TestParseSubmitObservationsTxHash(t *testing.T) {
	valid := mustHexDecode(t, fixtureSubmitObservationsIxDataHex)
	wantTxHash := mustHexDecode(t, fixtureSubmitObservationsTxHashHex)

	tests := []struct {
		name    string
		data    []byte
		wantErr bool
	}{
		{name: "valid", data: valid, wantErr: false},
		{name: "truncated mid tx_hash", data: valid[:submitObservationsDispatchLen+submitFixedLen+10], wantErr: true},
		{name: "truncated before tx_hash", data: valid[:submitObservationsDispatchLen+submitFixedLen], wantErr: true},
		{name: "empty", data: []byte{}, wantErr: true},
	}

	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			txHash, err := parseSubmitObservationsTxHash(tt.data)
			if tt.wantErr {
				assert.Error(t, err)
				assert.Nil(t, txHash)
				return
			}
			require.NoError(t, err)
			assert.Equal(t, wantTxHash, txHash)
		})
	}
}
