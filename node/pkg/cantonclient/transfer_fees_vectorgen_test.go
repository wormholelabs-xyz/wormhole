package cantonclient

// Signed TransferFees governance VAAs for canton/test/daml/Test/TestTransferFees.daml.
// Layout: sdk/vaa/payloads.go BodyCoreBridgeTransferFees (header(35) ‖ amount(32) ‖ recipient(32)).

import (
	"encoding/hex"
	"math"
	"testing"
	"time"

	"github.com/holiman/uint256"
	"github.com/stretchr/testify/require"
	"github.com/wormhole-foundation/wormhole/sdk/vaa"
)

const (
	damlTransferFeesPath = "../../../canton/test/daml/Test/TestTransferFees.daml"

	coreGovernanceActionTransferFees = byte(4)
	transferFeesPacketBytes          = 99

	// Above every other governance fixture timestamp.
	transferFeesVectorBaseTimestampUnixSeconds = int64(1795094700)
	transferFeesVectorBaseSequence             = uint64(200)
	transferFeesVectorAmount                   = uint64(11)
	transferFeesVectorRecipientByte            = byte(0xd5)
)

func recipientOfRepeatedByte(recipientByte byte) vaa.Address {
	var recipient vaa.Address
	for position := range recipient {
		recipient[position] = recipientByte
	}
	return recipient
}

func amountWordOf(amount *uint256.Int) [32]byte {
	return amount.Bytes32()
}

// Encodes any target chain and amount, including the chain 0 and amount 0 that the SDK serializer refuses.
func transferFeesPayloadWithAmountWord(targetChain vaa.ChainID, amountWord [32]byte, recipient vaa.Address) []byte {
	payload := coreGovernanceHeader(coreGovernanceModuleID(), coreGovernanceActionTransferFees, targetChain)
	payload = append(payload, amountWord[:]...)
	return append(payload, recipient[:]...)
}

// The guardian serializer output, checked against the independent encoding.
func transferFeesPayloadBySDK(t *testing.T, targetChain vaa.ChainID, amount *uint256.Int, recipient vaa.Address) []byte {
	require.False(t, vaa.CoreBridgeTransferFeesUsesCosmWasmLayout(targetChain))
	payload, err := vaa.BodyCoreBridgeTransferFees{ChainID: targetChain, Amount: amount, Recipient: recipient}.Serialize()
	require.NoError(t, err)
	require.Len(t, payload, transferFeesPacketBytes)
	require.Equal(t, transferFeesPayloadWithAmountWord(targetChain, amountWordOf(amount), recipient), payload)
	return payload
}

type transferFeesVAASpec struct {
	guardianSetIndex     uint32
	signers              []redTeamSigner
	timestampUnixSeconds int64
	sequence             uint64
	payload              func(t *testing.T) []byte
}

func encodeTransferFeesVAA(t *testing.T, spec transferFeesVAASpec) *vaa.VAA {
	payload := spec.payload(t)
	require.Len(t, payload, transferFeesPacketBytes)
	governanceVAA := &vaa.VAA{
		Version:          vaa.SupportedVAAVersion,
		GuardianSetIndex: spec.guardianSetIndex,
		Timestamp:        time.Unix(spec.timestampUnixSeconds, 0),
		Nonce:            fixtureNonce,
		Sequence:         spec.sequence,
		ConsistencyLevel: fixtureConsistencyLevel,
		EmitterChain:     vaa.GovernanceChain,
		EmitterAddress:   vaa.GovernanceEmitter,
		Payload:          payload,
	}
	for _, signer := range spec.signers {
		governanceVAA.AddSignature(redTeamGuardianKey(signer.keyIndex), signer.guardianIndex)
	}
	require.Len(t, governanceVAA.Signatures, len(spec.signers))
	return governanceVAA
}

func encodeTransferFeesVAAHex(t *testing.T, spec transferFeesVAASpec) string {
	encodedVAA, err := encodeTransferFeesVAA(t, spec).Marshal()
	require.NoError(t, err)
	return hex.EncodeToString(encodedVAA)
}

func transferFeesVAAFixture(name string, spec transferFeesVAASpec) damlHexFixture {
	return damlHexFixture{
		name:               name,
		damlLiteralPattern: damlTopLevelHexPattern(name),
		generateHex:        func(t *testing.T) string { return encodeTransferFeesVAAHex(t, spec) },
	}
}

// One fixture per sequence offset, each with its own timestamp.
func transferFeesVectorSpec(offset uint64, payload func(t *testing.T) []byte) transferFeesVAASpec {
	return transferFeesVAASpec{
		guardianSetIndex:     fixtureGuardianSetIndex,
		signers:              redTeamSignersInOrder(0),
		timestampUnixSeconds: transferFeesVectorBaseTimestampUnixSeconds + int64(offset), //nolint:gosec // offsets are small
		sequence:             transferFeesVectorBaseSequence + offset,
		payload:              payload,
	}
}

// Body of govTransferFeesVAA, signed by a quorum of set 1 (devnet keys 0..3).
func govTransferFeesResignedBySet1Spec() transferFeesVAASpec {
	return transferFeesVAASpec{
		guardianSetIndex:     1,
		signers:              redTeamSignersInOrder(0, 1, 2),
		timestampUnixSeconds: govTransferFeesTimestampUnixSeconds,
		sequence:             govTransferFeesSequence,
		payload:              govTransferFeesPayload,
	}
}

func transferFeesDamlHexFixtures() []damlHexFixture {
	vectorRecipient := recipientOfRepeatedByte(transferFeesVectorRecipientByte)
	vectorAmount := uint256.NewInt(transferFeesVectorAmount)
	sdkPayload := func(targetChain vaa.ChainID, amount *uint256.Int) func(t *testing.T) []byte {
		return func(t *testing.T) []byte { return transferFeesPayloadBySDK(t, targetChain, amount, vectorRecipient) }
	}
	rawPayload := func(targetChain vaa.ChainID, amount *uint256.Int) func(t *testing.T) []byte {
		return func(t *testing.T) []byte {
			return transferFeesPayloadWithAmountWord(targetChain, amountWordOf(amount), vectorRecipient)
		}
	}
	maxInt64Amount := uint256.NewInt(math.MaxInt64)
	twoPow63Amount := new(uint256.Int).Lsh(uint256.NewInt(1), 63)
	maxUint256Amount := new(uint256.Int).SetAllOne()

	return []damlHexFixture{
		transferFeesVAAFixture("tfTransferFeesToAllChains", transferFeesVectorSpec(1, rawPayload(vaa.ChainIDUnset, vectorAmount))),
		transferFeesVAAFixture("tfTransferFeesToEthereum", transferFeesVectorSpec(2, sdkPayload(vaa.ChainIDEthereum, vectorAmount))),
		transferFeesVAAFixture("tfTransferFeesAmountZero", transferFeesVectorSpec(3, rawPayload(vaa.ChainIDCanton, uint256.NewInt(0)))),
		transferFeesVAAFixture("tfTransferFeesAmountMaxInt64", transferFeesVectorSpec(4, sdkPayload(vaa.ChainIDCanton, maxInt64Amount))),
		transferFeesVAAFixture("tfTransferFeesAmountTwoPow63", transferFeesVectorSpec(5, sdkPayload(vaa.ChainIDCanton, twoPow63Amount))),
		transferFeesVAAFixture("tfTransferFeesAmountMaxUint256", transferFeesVectorSpec(6, sdkPayload(vaa.ChainIDCanton, maxUint256Amount))),
		transferFeesVAAFixture("tfTransferFeesToZeroRecipient", transferFeesVectorSpec(7, func(t *testing.T) []byte {
			return transferFeesPayloadWithAmountWord(vaa.ChainIDCanton, amountWordOf(vectorAmount), vaa.Address{})
		})),
		transferFeesVAAFixture("tfTransferFeesResignedBySet1", govTransferFeesResignedBySet1Spec()),
	}
}

func TestTransferFeesSDKSerializerRefusesChainZeroAndAmountZero(t *testing.T) {
	recipient := recipientOfRepeatedByte(transferFeesVectorRecipientByte)
	cases := []struct {
		name        string
		targetChain vaa.ChainID
		amount      *uint256.Int
		recipient   vaa.Address
	}{
		{name: "chain 0", targetChain: vaa.ChainIDUnset, amount: uint256.NewInt(1), recipient: recipient},
		{name: "amount 0", targetChain: vaa.ChainIDCanton, amount: uint256.NewInt(0), recipient: recipient},
		{name: "zero recipient", targetChain: vaa.ChainIDCanton, amount: uint256.NewInt(1), recipient: vaa.Address{}},
	}
	for _, testCase := range cases {
		t.Run(testCase.name, func(t *testing.T) {
			_, err := vaa.BodyCoreBridgeTransferFees{ChainID: testCase.targetChain, Amount: testCase.amount, Recipient: testCase.recipient}.Serialize()
			require.Error(t, err)
		})
	}
}

func TestTransferFeesResignedBySet1KeepsTheDigest(t *testing.T) {
	originalVAA := encodeTransferFeesVAA(t, transferFeesVAASpec{
		guardianSetIndex:     fixtureGuardianSetIndex,
		signers:              redTeamSignersInOrder(0),
		timestampUnixSeconds: govTransferFeesTimestampUnixSeconds,
		sequence:             govTransferFeesSequence,
		payload:              govTransferFeesPayload,
	})
	resignedVAA := encodeTransferFeesVAA(t, govTransferFeesResignedBySet1Spec())
	require.Equal(t, originalVAA.SigningDigest(), resignedVAA.SigningDigest())
	require.NotEqual(t, originalVAA.GuardianSetIndex, resignedVAA.GuardianSetIndex)

	encodedOriginalVAA, err := originalVAA.Marshal()
	require.NoError(t, err)
	require.Equal(t, generateGovTransferFeesVAAHex(t), hex.EncodeToString(encodedOriginalVAA))
}
