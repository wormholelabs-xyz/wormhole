package accountant

import (
	"encoding/binary"
	"encoding/json"
	"fmt"
	"math/big"
	"os"
	"strings"
	"testing"

	"github.com/certusone/wormhole/node/pkg/common"
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

// mustContentDigest recomputes the content digest of f.
func mustContentDigest(t *testing.T, f *solanaNttObservationFields) [32]byte {
	t.Helper()
	digest, err := f.computeContentDigest()
	require.NoError(t, err)
	return digest
}

func TestNttWireLayouts(t *testing.T) {
	transfer := nttTestTransferPayload(8, 12_345, 10, nil, nil)
	managerHead := transfer[nttTransceiverHeadLen : nttTransceiverHeadLen+nttManagerHeadLen]
	nativeTransfer := transfer[nttTransceiverHeadLen+nttManagerHeadLen : nttTransceiverHeadLen+nttManagerHeadLen+nttNativeTokenTransferLen]
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
		{"transceiver head", func(t *testing.T) {
			checkWireLayout[nttTransceiverHeadWire](t, transfer[:nttTransceiverHeadLen])
		}},
		{"manager head", func(t *testing.T) {
			checkWireLayout[nttManagerHeadWire](t, managerHead)
		}},
		{"native token transfer", func(t *testing.T) {
			checkWireLayout[nttNativeTokenTransferWire](t, nativeTransfer)
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

// nttCorpusRow is one vectors row of ntt_test_vectors.json.
type nttCorpusRow struct {
	Chain                  uint16 `json:"chain"`
	Emitter                string `json:"emitter"`
	Sequence               uint64 `json:"sequence"`
	ViaRelayer             bool   `json:"via_relayer"`
	ExpectedDigest         string `json:"expected_digest"`
	ExpectedAmount         string `json:"expected_amount"`
	ExpectedTokenChain     uint16 `json:"expected_token_chain"`
	ExpectedTokenAddress   string `json:"expected_token_address"`
	ExpectedRecipientChain uint16 `json:"expected_recipient_chain"`
	VaaHex                 string `json:"vaa_hex"`
}

type nttCorpusHub struct {
	Chain      uint16 `json:"chain"`
	Address    string `json:"address"`
	HubChain   uint16 `json:"hub_chain"`
	HubAddress string `json:"hub_address"`
}

func mustHex0x(t *testing.T, s string) []byte {
	t.Helper()
	return mustHexDecode(t, strings.TrimPrefix(s, "0x"))
}

// loadNttCorpus reads the mainnet NTT corpus the program tests use.
func loadNttCorpus(t *testing.T) ([]nttCorpusRow, map[emitterKey]solanaNttHub) {
	t.Helper()
	raw, err := os.ReadFile(fixtureNttCorpusPath)
	require.NoError(t, err)
	var corpus struct {
		Hubs    []nttCorpusHub `json:"hubs"`
		Vectors []nttCorpusRow `json:"vectors"`
	}
	require.NoError(t, json.Unmarshal(raw, &corpus))
	require.Len(t, corpus.Vectors, fixtureNttCorpusVectors)

	hubs := make(map[emitterKey]solanaNttHub, len(corpus.Hubs))
	for _, h := range corpus.Hubs {
		key := emitterKey{emitterChainId: vaa.ChainID(h.Chain), emitterAddr: vaa.Address(mustHex0x(t, h.Address))}
		hubs[key] = solanaNttHub{Chain: vaa.ChainID(h.HubChain), Address: vaa.Address(mustHex0x(t, h.HubAddress))}
	}
	return corpus.Vectors, hubs
}

// normalizeNttAmount mirrors ntt/amount.rs normalize_trimmed_amount as a 32-byte big-endian value.
func normalizeNttAmount(decimals uint8, amount uint64) [32]byte {
	n := new(big.Int).SetUint64(amount)
	switch {
	case decimals > 8:
		n.Quo(n, new(big.Int).Exp(big.NewInt(10), big.NewInt(int64(decimals-8)), nil))
	case decimals < 8:
		n.Mul(n, new(big.Int).Exp(big.NewInt(10), big.NewInt(int64(8-decimals)), nil))
	}
	var out [32]byte
	n.FillBytes(out[:])
	return out
}

func TestNttParseTransceiverTransferMainnetCorpus(t *testing.T) {
	rows, hubs := loadNttCorpus(t)
	directEmitters, arEmitters, err := nttGetEmitters(common.MainNet)
	require.NoError(t, err)
	acct := &Accountant{nttDirectEmitters: directEmitters, nttArEmitters: arEmitters}

	covered := 0
	for _, row := range rows {
		t.Run(fmt.Sprintf("%s/%d", vaa.ChainID(row.Chain), row.Sequence), func(t *testing.T) {
			v, err := vaa.Unmarshal(mustHex0x(t, row.VaaHex))
			require.NoError(t, err)
			require.Equal(t, vaa.ChainID(row.Chain), v.EmitterChain)
			require.Equal(t, vaa.Address(mustHex0x(t, row.Emitter)), v.EmitterAddress)
			require.Equal(t, row.Sequence, v.Sequence)
			vaaDigest := [32]byte(v.SigningDigest())
			require.Equal(t, [32]byte(mustHex0x(t, row.ExpectedDigest)), vaaDigest)

			sender, message := v.EmitterAddress, v.Payload
			if row.ViaRelayer {
				ok, relayedSender, relayedMessage := nttParseArPayload(v.Payload)
				require.True(t, ok)
				sender, message = relayedSender, relayedMessage
			}
			transfer, err := nttParseTransceiverTransfer(message)
			require.NoError(t, err)
			require.Equal(t, vaa.ChainID(row.ExpectedRecipientChain), transfer.ToChain)
			require.Equal(t, [32]byte(mustHex0x(t, row.ExpectedAmount)), normalizeNttAmount(transfer.TrimmedDecimals, transfer.TrimmedAmount))
			require.Equal(t, solanaNttHub{Chain: vaa.ChainID(row.ExpectedTokenChain), Address: vaa.Address(mustHex0x(t, row.ExpectedTokenAddress))}, hubs[emitterKey{emitterChainId: v.EmitterChain, emitterAddr: sender}])

			// The guardian's mainnet config covers every row and builds the same record.
			msg := &common.MessagePublication{EmitterChain: v.EmitterChain, EmitterAddress: v.EmitterAddress, Sequence: v.Sequence, Payload: v.Payload}
			isCovered, isNTT, _ := acct.isMessageCoveredByAccountant(msg)
			require.True(t, isCovered)
			require.True(t, isNTT)
			covered++
			fields, err := acct.solanaNttObservationFieldsFromMessage(msg, vaaDigest)
			require.NoError(t, err)
			require.Equal(t, sender, fields.Sender)
			require.Equal(t, row.ViaRelayer, fields.Sender != fields.Emitter)
			require.Equal(t, transfer.TrimmedDecimals, fields.TrimmedDecimals)
			require.Equal(t, transfer.TrimmedAmount, fields.TrimmedAmount)
			require.Equal(t, vaaDigest, fields.VaaDigest)
			require.Equal(t, fields.contentDigest, mustContentDigest(t, fields))
		})
	}
	require.Equal(t, fixtureNttCorpusVectors, covered)
}

func TestSolanaNttObservationFieldsFromFixtureBodies(t *testing.T) {
	directEmitters, arEmitters, err := nttGetEmitters(common.MainNet)
	require.NoError(t, err)
	acct := &Accountant{nttDirectEmitters: directEmitters, nttArEmitters: arEmitters}

	tests := []struct {
		name string
		body string
		want solanaNttObservationFields
	}{
		{name: "direct", body: fixtureNttDirectBodyHex, want: fixtureNttDirectFields(t)},
		{name: "relayed", body: fixtureNttRelayedBodyHex, want: fixtureNttRelayedFields(t)},
	}
	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			body := mustHexDecode(t, tt.body)
			require.Greater(t, len(body), fixtureVaaBodyHeaderLen)
			msg := &common.MessagePublication{
				EmitterChain:   vaa.ChainID(binary.BigEndian.Uint16(body[8:10])),
				EmitterAddress: vaa.Address(body[10:42]),
				Sequence:       binary.BigEndian.Uint64(body[42:50]),
				Payload:        body[fixtureVaaBodyHeaderLen:],
			}
			got, err := acct.solanaNttObservationFieldsFromMessage(msg, tt.want.VaaDigest)
			require.NoError(t, err)
			require.Equal(t, tt.want, *got)
		})
	}

	t.Run("emitter outside the config", func(t *testing.T) {
		body := mustHexDecode(t, fixtureNttDirectBodyHex)
		msg := &common.MessagePublication{EmitterChain: vaa.ChainIDEthereum, EmitterAddress: vaa.Address(body[10:42]), Payload: body[fixtureVaaBodyHeaderLen:]}
		_, err := acct.solanaNttObservationFieldsFromMessage(msg, [32]byte{})
		require.Error(t, err)
	})
}

// nttTestTransferPayload mirrors transfer.rs tests transceiver_message(native_transfer(...)).
func nttTestTransferPayload(decimals uint8, amount uint64, toChain vaa.ChainID, additional []byte, transceiverPayload []byte) []byte {
	inner := make([]byte, 0, nttNativeTokenTransferLen+nttLengthPrefixLen+len(additional))
	inner = append(inner, nttNativeTokenTransferPrefix[:]...)
	inner = append(inner, decimals)
	inner = binary.BigEndian.AppendUint64(inner, amount)
	inner = append(inner, make([]byte, 32)...) // source_token
	inner = append(inner, make([]byte, 32)...) // to
	inner = binary.BigEndian.AppendUint16(inner, uint16(toChain))
	if additional != nil {
		inner = binary.BigEndian.AppendUint16(inner, uint16(len(additional))) // #nosec G115 -- test sizes
		inner = append(inner, additional...)
	}

	manager := make([]byte, 0, nttManagerHeadLen+len(inner))
	manager = append(manager, make([]byte, 64)...)                       // id, sender
	manager = binary.BigEndian.AppendUint16(manager, uint16(len(inner))) // #nosec G115 -- test sizes
	manager = append(manager, inner...)

	out := make([]byte, 0, nttTransceiverHeadLen+len(manager)+2+len(transceiverPayload))
	out = append(out, nttTransceiverMessagePrefix[:]...)
	out = append(out, make([]byte, 64)...)                         // source and recipient manager
	out = binary.BigEndian.AppendUint16(out, uint16(len(manager))) // #nosec G115 -- test sizes
	out = append(out, manager...)
	out = binary.BigEndian.AppendUint16(out, uint16(len(transceiverPayload))) // #nosec G115 -- test sizes
	out = append(out, transceiverPayload...)
	return out
}

func TestNttParseTransceiverTransfer(t *testing.T) {
	const (
		managerLenOffset = nttTransceiverHeadLen - 2
		innerLenOffset   = nttTransceiverHeadLen + nttManagerHeadLen - 2
	)
	ok := nttTransfer{TrimmedDecimals: 8, TrimmedAmount: 12_345, ToChain: 10}
	build := func() []byte { return nttTestTransferPayload(8, 12_345, 10, nil, nil) }
	withAdditional := nttTestTransferPayload(8, 12_345, 10, []byte{0x42, 0x42, 0x42, 0x42, 0x42}, nil)
	mutate := func(b []byte, edit func(b []byte) []byte) []byte { return edit(append([]byte(nil), b...)) }

	tests := []struct {
		name    string
		payload []byte
		want    nttTransfer
		wantErr bool
	}{
		{name: "well formed", payload: build(), want: ok},
		{name: "minimal length", payload: func() []byte { b := build(); require.Len(t, b, nttMinTransferLen); return b }(), want: ok},
		{name: "transceiver payload present", payload: nttTestTransferPayload(8, 12_345, 10, nil, []byte{1, 2, 3}), want: ok},
		{name: "additional payload present", payload: withAdditional, want: ok},
		{name: "largest normalizable decimals", payload: nttTestTransferPayload(maxNttTrimmedDecimals, 1, 10, nil, nil), want: nttTransfer{TrimmedDecimals: maxNttTrimmedDecimals, TrimmedAmount: 1, ToChain: 10}},
		{name: "empty", payload: nil, wantErr: true},
		{name: "bad transceiver prefix", payload: mutate(build(), func(b []byte) []byte { b[0] = 0; return b }), wantErr: true},
		{name: "bad ntt prefix", payload: mutate(build(), func(b []byte) []byte { b[NTT_PREFIX_OFFSET] = 0; return b }), wantErr: true},
		{name: "truncated", payload: build()[:nttMinTransferLen-1], wantErr: true},
		{name: "manager len short", payload: mutate(build(), func(b []byte) []byte { b[managerLenOffset+1]--; return b }), wantErr: true},
		{name: "inner len long", payload: mutate(build(), func(b []byte) []byte { b[innerLenOffset+1]++; return b }), wantErr: true},
		{name: "additional len off", payload: mutate(withAdditional, func(b []byte) []byte { b[NTT_PREFIX_OFFSET+nttNativeTokenTransferLen+1]++; return b }), wantErr: true},
		{name: "trailing byte", payload: append(build(), 0x99), wantErr: true},
		{name: "decimals past the program's normalization", payload: nttTestTransferPayload(maxNttTrimmedDecimals+1, 1, 10, nil, nil), wantErr: true},
		{name: "over cap", payload: nttTestTransferPayload(8, 12_345, 10, make([]byte, 1900), nil), wantErr: true},
	}
	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			got, err := nttParseTransceiverTransfer(tt.payload)
			if tt.wantErr {
				require.Error(t, err)
				return
			}
			require.NoError(t, err)
			assert.Equal(t, tt.want, got)
			// The looser coverage check agrees on every accepted transfer.
			assert.True(t, nttIsPayloadNTT(tt.payload))
		})
	}
}
