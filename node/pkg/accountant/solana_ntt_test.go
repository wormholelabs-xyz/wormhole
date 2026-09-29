package accountant

import (
	"context"
	"encoding/binary"
	"testing"
	"time"

	"github.com/certusone/wormhole/node/pkg/common"
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
