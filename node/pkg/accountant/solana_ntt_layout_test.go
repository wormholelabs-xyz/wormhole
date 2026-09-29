package accountant

import (
	"testing"

	"github.com/gagliardetto/solana-go"
	"github.com/stretchr/testify/assert"
	"github.com/stretchr/testify/require"
	"github.com/wormhole-foundation/wormhole/sdk/vaa"
)

// Fixtures come from solana_ntt_layout_fixtures_gen_test.go, which svm/accountant/crates/go-codegen
// writes from the mainnet NTT corpus and the program code paths.

func mustAddress(t *testing.T, s string) vaa.Address {
	t.Helper()
	return vaa.Address(mustHexDecode32(t, s))
}

func fixtureNttDirectFields(t *testing.T) solanaNttObservationFields {
	t.Helper()
	emitter := mustAddress(t, fixtureNttDirectEmitterHex)
	f := solanaNttObservationFields{
		Chain:           vaa.ChainID(fixtureNttDirectChain),
		Emitter:         emitter,
		Sequence:        fixtureNttDirectSequence,
		Sender:          emitter,
		RecipientChain:  vaa.ChainID(fixtureNttDirectRecipientChain),
		TrimmedDecimals: fixtureNttDirectTrimmedDecimals,
		TrimmedAmount:   fixtureNttDirectTrimmedAmount,
		VaaDigest:       mustHexDecode32(t, fixtureNttDirectVaaDigestHex),
	}
	require.NoError(t, f.setContentDigest())
	return f
}

func fixtureNttRelayedFields(t *testing.T) solanaNttObservationFields {
	t.Helper()
	f := solanaNttObservationFields{
		Chain:           vaa.ChainID(fixtureNttRelayedChain),
		Emitter:         mustAddress(t, fixtureNttRelayedEmitterHex),
		Sequence:        fixtureNttRelayedSequence,
		Sender:          mustAddress(t, fixtureNttRelayedSenderHex),
		RecipientChain:  vaa.ChainID(fixtureNttRelayedRecipientChain),
		TrimmedDecimals: fixtureNttRelayedTrimmedDecimals,
		TrimmedAmount:   fixtureNttRelayedTrimmedAmount,
		VaaDigest:       mustHexDecode32(t, fixtureNttRelayedVaaDigestHex),
	}
	require.NoError(t, f.setContentDigest())
	return f
}

func TestNttObservationFieldsPackAndDigest(t *testing.T) {
	tests := []struct {
		name        string
		fields      solanaNttObservationFields
		wantFields  string
		wantContent string
		direct      bool
	}{
		{name: "direct", fields: fixtureNttDirectFields(t), wantFields: fixtureNttDirectFieldsHex, wantContent: fixtureNttDirectContentDigestHex, direct: true},
		{name: "relayed", fields: fixtureNttRelayedFields(t), wantFields: fixtureNttRelayedFieldsHex, wantContent: fixtureNttRelayedContentDigestHex},
	}
	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			packed, err := tt.fields.pack()
			require.NoError(t, err)
			assert.Equal(t, mustHexDecode(t, tt.wantFields), packed)
			assert.Equal(t, mustHexDecode32(t, tt.wantContent), tt.fields.contentDigest)
			assert.Equal(t, tt.direct, tt.fields.Sender == tt.fields.Emitter)

			unpacked, err := unpackNttObservationFields(packed)
			require.NoError(t, err)
			assert.Equal(t, tt.fields, unpacked)
		})
	}

	for _, n := range []int{0, nttObservationFieldsLen - 1, nttObservationFieldsLen + 1} {
		_, err := unpackNttObservationFields(make([]byte, n))
		assert.Error(t, err, "%d bytes", n)
	}
}

func TestNttObservationSigningDigest(t *testing.T) {
	fields := fixtureNttRelayedFields(t)
	hashTxID, err := newSolanaTxID(mustHexDecode(t, fixtureNttHashTxIDHex))
	require.NoError(t, err)
	signatureTxID, err := newSolanaTxID(mustHexDecode(t, fixtureNttSignatureTxIDHex))
	require.NoError(t, err)

	tests := []struct {
		name   string
		prefix []byte
		txID   solanaTxID
		want   string
		wantEq bool
	}{
		{name: "32-byte tx id", prefix: NttSubmitObservationPrefix, txID: hashTxID, want: fixtureNttHashTxIDSigningDigestHex, wantEq: true},
		{name: "64-byte tx id", prefix: NttSubmitObservationPrefix, txID: signatureTxID, want: fixtureNttSignatureTxIDSigningDigestHex, wantEq: true},
		{name: "wtt prefix", prefix: SubmitObservationPrefix, txID: hashTxID, want: fixtureNttHashTxIDSigningDigestHex},
	}
	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			got, err := solanaObservationSigningDigest(tt.prefix, tt.txID, &fields)
			require.NoError(t, err)
			if tt.wantEq {
				require.Equal(t, mustHexDecode32(t, tt.want), [32]byte(got))
			} else {
				require.NotEqual(t, mustHexDecode32(t, tt.want), [32]byte(got))
			}
		})
	}
}

func TestEncodeNttSubmitObservationsIxData(t *testing.T) {
	fields := fixtureNttRelayedFields(t)
	var signature [submitSignatureLen]byte
	for i := range signature {
		signature[i] = byte(i)
	}
	tests := []struct {
		name string
		txID string
		want string
	}{
		{name: "32-byte tx id", txID: fixtureNttHashTxIDHex, want: fixtureNttHashTxIDIxDataHex},
		{name: "64-byte tx id", txID: fixtureNttSignatureTxIDHex, want: fixtureNttSignatureTxIDIxDataHex},
	}
	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			txID, err := newSolanaTxID(mustHexDecode(t, tt.txID))
			require.NoError(t, err)
			got, err := encodeSubmitObservationsIxData(fixtureNttGuardianSetIndex, fixtureNttGuardianIndex, signature[:], txID, &fields)
			require.NoError(t, err)
			require.Equal(t, mustHexDecode(t, tt.want), got)
			require.Len(t, got, nttSubmitObservationsInstructionLen)
		})
	}

	var nilFields *solanaNttObservationFields
	txID, err := newSolanaTxID(mustHexDecode(t, fixtureNttHashTxIDHex))
	require.NoError(t, err)
	_, err = encodeSubmitObservationsIxData(0, 0, signature[:], txID, nilFields)
	require.Error(t, err)
}

func TestParseNttSubmitObservationsIxData(t *testing.T) {
	hashData := mustHexDecode(t, fixtureNttHashTxIDIxDataHex)
	require.Len(t, hashData, nttSubmitObservationsInstructionLen)
	signatureData := mustHexDecode(t, fixtureNttSignatureTxIDIxDataHex)
	require.Len(t, signatureData, nttSubmitObservationsInstructionLen)

	mutate := func(data []byte, edit func(*nttSubmitObservationsInstructionWire)) []byte {
		var wire nttSubmitObservationsInstructionWire
		require.NoError(t, decodeWire(data, &wire))
		edit(&wire)
		out, err := encodeWire(&wire)
		require.NoError(t, err)
		return out
	}

	tests := []struct {
		name     string
		data     []byte
		wantTxID string
		wantErr  bool
	}{
		{name: "valid, 32-byte tx id", data: hashData, wantTxID: fixtureNttHashTxIDHex},
		{name: "valid, 64-byte tx id", data: signatureData, wantTxID: fixtureNttSignatureTxIDHex},
		{name: "empty", data: nil, wantErr: true},
		{name: "one byte short", data: hashData[:len(hashData)-1], wantErr: true},
		{name: "one byte long", data: append(append([]byte(nil), hashData...), 0), wantErr: true},
		{name: "wtt length", data: make([]byte, submitObservationsInstructionLen), wantErr: true},
		{name: "wrong discriminator", data: mutate(hashData, func(w *nttSubmitObservationsInstructionWire) { w.Discriminator = 2 }), wantErr: true}, // Instruction::SubmitVaas
		{name: "tx id length 0", data: mutate(hashData, func(w *nttSubmitObservationsInstructionWire) { w.Data.TxIDLen = 0 }), wantErr: true},
		{name: "tx id length between the two forms", data: mutate(hashData, func(w *nttSubmitObservationsInstructionWire) { w.Data.TxIDLen = hashTxIDLen + 1 }), wantErr: true},
		{name: "32-byte tx id, nonzero last padding byte", data: mutate(hashData, func(w *nttSubmitObservationsInstructionWire) { w.Data.TxID[signatureTxIDLen-1] = 1 }), wantErr: true},
	}

	var wantSignature [submitSignatureLen]byte
	for i := range wantSignature {
		wantSignature[i] = byte(i)
	}
	wantFields := fixtureNttRelayedFields(t)

	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			ix, err := parseNttSubmitObservationsIxData(tt.data)
			if tt.wantErr {
				assert.Error(t, err)
				assert.Nil(t, ix)
				return
			}
			require.NoError(t, err)
			assert.Equal(t, fixtureNttGuardianSetIndex, ix.GuardianSetIndex)
			assert.Equal(t, fixtureNttGuardianIndex, ix.GuardianIndex)
			assert.Equal(t, wantSignature, ix.Signature)
			assert.Equal(t, mustHexDecode(t, tt.wantTxID), ix.TxID.Bytes())
			assert.Equal(t, wantFields, ix.solanaNttObservationFields)
		})
	}
}

func TestDeriveSolanaNttPDAs(t *testing.T) {
	program := fixtureProgramID()
	fields := fixtureNttRelayedFields(t)
	hubChain := vaa.ChainID(fixtureNttRelayedHubChain)
	hubAddress := mustAddress(t, fixtureNttRelayedHubAddressHex)
	peer := mustAddress(t, fixtureNttPeerHex)

	derive := map[string]func() (solana.PublicKey, error){
		"pending": func() (solana.PublicKey, error) {
			return derivePendingObservationsPDA(program, fields.Chain, fields.Emitter, fields.Sequence, fixtureNttGuardianSetIndex, fields.contentDigest)
		},
		"hub": func() (solana.PublicKey, error) {
			return deriveTransceiverHubPDA(program, fields.Chain, fields.Sender)
		},
		"peer src": func() (solana.PublicKey, error) {
			return deriveTransceiverPeerPDA(program, fields.Chain, fields.Sender, fields.RecipientChain)
		},
		"peer dst": func() (solana.PublicKey, error) {
			return deriveTransceiverPeerPDA(program, fields.RecipientChain, peer, fields.Chain)
		},
		"source balance": func() (solana.PublicKey, error) {
			return deriveBalanceAccountPDA(program, fields.Chain, hubChain, hubAddress)
		},
		"dest balance": func() (solana.PublicKey, error) {
			return deriveBalanceAccountPDA(program, fields.RecipientChain, hubChain, hubAddress)
		},
		"relayer registration": func() (solana.PublicKey, error) {
			return deriveChainRegistrationPDA(program, fields.Chain)
		},
	}
	want := map[string]string{
		"pending":              fixtureNttPendingPDAHex,
		"hub":                  fixtureNttHubPDAHex,
		"peer src":             fixtureNttPeerSrcPDAHex,
		"peer dst":             fixtureNttPeerDstPDAHex,
		"source balance":       fixtureNttSourceBalancePDAHex,
		"dest balance":         fixtureNttDestBalancePDAHex,
		"relayer registration": fixtureNttRelayerRegistrationPDAHex,
	}
	require.Len(t, derive, len(want))
	for name, fn := range derive {
		t.Run(name, func(t *testing.T) {
			got, err := fn()
			require.NoError(t, err)
			assert.Equal(t, solana.PublicKey(mustHexDecode32(t, want[name])), got)
		})
	}

	// The sender and the emitter differ, so a swapped seed derives another hub.
	swapped, err := deriveTransceiverHubPDA(program, fields.Chain, fields.Emitter)
	require.NoError(t, err)
	assert.NotEqual(t, solana.PublicKey(mustHexDecode32(t, fixtureNttHubPDAHex)), swapped)
}

func TestParseTransceiverAccounts(t *testing.T) {
	fields := fixtureNttRelayedFields(t)
	hubImage := mustHexDecode(t, fixtureNttHubAccountHex)
	peerImage := mustHexDecode(t, fixtureNttPeerSrcAccountHex)

	mutateHub := func(edit func(*transceiverHubWire)) []byte {
		var wire transceiverHubWire
		require.NoError(t, decodeWire(hubImage, &wire))
		edit(&wire)
		return mustEncodeWire(&wire)
	}
	mutatePeer := func(edit func(*transceiverPeerWire)) []byte {
		var wire transceiverPeerWire
		require.NoError(t, decodeWire(peerImage, &wire))
		edit(&wire)
		return mustEncodeWire(&wire)
	}

	t.Run("hub", func(t *testing.T) {
		tests := []struct {
			name    string
			data    []byte
			chain   vaa.ChainID
			address vaa.Address
			wantErr bool
		}{
			{name: "exact image", data: hubImage, chain: fields.Chain, address: fields.Sender},
			{name: "peer tag", data: mutateHub(func(w *transceiverHubWire) { w.Tag = transceiverPeerTag }), chain: fields.Chain, address: fields.Sender, wantErr: true},
			{name: "one byte short", data: hubImage[:transceiverHubLen-1], chain: fields.Chain, address: fields.Sender, wantErr: true},
			{name: "one byte long", data: append(append([]byte(nil), hubImage...), 0), chain: fields.Chain, address: fields.Sender, wantErr: true},
			{name: "other chain", data: hubImage, chain: fields.RecipientChain, address: fields.Sender, wantErr: true},
			{name: "other address", data: hubImage, chain: fields.Chain, address: fields.Emitter, wantErr: true},
		}
		for _, tt := range tests {
			t.Run(tt.name, func(t *testing.T) {
				hub, err := parseTransceiverHubAccount(tt.data, tt.chain, tt.address)
				if tt.wantErr {
					assert.Error(t, err)
					return
				}
				require.NoError(t, err)
				assert.Equal(t, solanaNttHub{Chain: vaa.ChainID(fixtureNttRelayedHubChain), Address: mustAddress(t, fixtureNttRelayedHubAddressHex)}, hub)
			})
		}
	})

	t.Run("peer", func(t *testing.T) {
		tests := []struct {
			name      string
			data      []byte
			chain     vaa.ChainID
			address   vaa.Address
			destChain vaa.ChainID
			wantErr   bool
		}{
			{name: "exact image", data: peerImage, chain: fields.Chain, address: fields.Sender, destChain: fields.RecipientChain},
			{name: "hub tag", data: mutatePeer(func(w *transceiverPeerWire) { w.Tag = transceiverHubTag }), chain: fields.Chain, address: fields.Sender, destChain: fields.RecipientChain, wantErr: true},
			{name: "one byte short", data: peerImage[:transceiverPeerLen-1], chain: fields.Chain, address: fields.Sender, destChain: fields.RecipientChain, wantErr: true},
			{name: "one byte long", data: append(append([]byte(nil), peerImage...), 0), chain: fields.Chain, address: fields.Sender, destChain: fields.RecipientChain, wantErr: true},
			{name: "other chain", data: peerImage, chain: fields.RecipientChain, address: fields.Sender, destChain: fields.RecipientChain, wantErr: true},
			{name: "other address", data: peerImage, chain: fields.Chain, address: fields.Emitter, destChain: fields.RecipientChain, wantErr: true},
			{name: "other dest chain", data: peerImage, chain: fields.Chain, address: fields.Sender, destChain: fields.Chain, wantErr: true},
		}
		for _, tt := range tests {
			t.Run(tt.name, func(t *testing.T) {
				peer, err := parseTransceiverPeerAccount(tt.data, tt.chain, tt.address, tt.destChain)
				if tt.wantErr {
					assert.Error(t, err)
					return
				}
				require.NoError(t, err)
				assert.Equal(t, mustAddress(t, fixtureNttPeerHex), peer)
			})
		}
	})
}

func TestNttWireLayouts(t *testing.T) {
	submitInstruction := mustHexDecode(t, fixtureNttHashTxIDIxDataHex)
	tests := []struct {
		name  string
		check func(t *testing.T)
	}{
		{"observation fields", func(t *testing.T) {
			checkWireLayout[nttObservationFieldsWire](t, mustHexDecode(t, fixtureNttRelayedFieldsHex))
		}},
		{"submit_observations instruction", func(t *testing.T) {
			checkWireLayout[nttSubmitObservationsInstructionWire](t, submitInstruction)
		}},
		{"submit_observations instruction data", func(t *testing.T) {
			checkWireLayout[nttSubmitObservationsIxDataWire](t, submitInstruction[nttSubmitObservationsInstructionLen-nttSubmitObservationsIxDataLen:])
		}},
		{"transceiver hub", func(t *testing.T) {
			checkWireLayout[transceiverHubWire](t, mustHexDecode(t, fixtureNttHubAccountHex))
		}},
		{"transceiver peer", func(t *testing.T) {
			checkWireLayout[transceiverPeerWire](t, mustHexDecode(t, fixtureNttPeerSrcAccountHex))
		}},
	}
	for _, tc := range tests {
		t.Run(tc.name, tc.check)
	}
}

func TestNttSolanaErrorCodesMatchProgram(t *testing.T) {
	tests := []struct {
		name string
		got  uint32
		want uint32
	}{
		{"ERR_INVALID_INSTRUCTION_DATA", solanaErrInvalidInstructionData, 1},
		{"ERR_MALFORMED_NTT_MESSAGE", solanaErrMalformedNttMessage, 34},
		{"ERR_MISSING_TRANSCEIVER_HUB", solanaErrMissingTransceiverHub, 39},
		{"ERR_MISSING_SOURCE_PEER", solanaErrMissingSourcePeer, 46},
		{"ERR_MISSING_DESTINATION_PEER", solanaErrMissingDestinationPeer, 47},
		{"ERR_PEERS_NOT_CROSS_REGISTERED", solanaErrPeersNotCrossRegistered, 48},
	}
	for _, tc := range tests {
		t.Run(tc.name, func(t *testing.T) {
			assert.Equal(t, tc.want, tc.got)
		})
	}
}
