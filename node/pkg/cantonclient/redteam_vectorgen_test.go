package cantonclient

// Signed governance and application VAAs for canton/test/daml/Test/TestRedTeamCore.daml.
// Guardian set 0 is {devnet key 0}; the upgrade fixture installs set 1 = devnet keys 0..3.

import (
	"crypto/ecdsa"
	"encoding/binary"
	"encoding/hex"
	"fmt"
	"testing"
	"time"

	"github.com/certusone/wormhole/node/pkg/devnet"
	"github.com/ethereum/go-ethereum/crypto"
	"github.com/stretchr/testify/require"
	"github.com/wormhole-foundation/wormhole/sdk/vaa"
)

const (
	damlRedTeamCorePath = "../../../canton/test/daml/Test/TestRedTeamCore.daml"

	redTeamSet1GuardianCount = 4

	coreGovernanceActionContractUpgrade    = byte(1)
	coreGovernanceActionGuardianSetUpgrade = byte(2)
	coreGovernanceActionSetMessageFee      = byte(3)
	coreGovernanceActionRecoverChainID     = byte(5)

	redTeamSetMessageFeeAmount = uint64(7)
	redTeamAppEmitterChain     = vaa.ChainIDEthereum
)

// redTeamSigner signs with devnet key keyIndex under guardian index guardianIndex.
type redTeamSigner struct {
	keyIndex      uint64
	guardianIndex uint8
}

func redTeamSignersInOrder(guardianIndices ...uint8) []redTeamSigner {
	signers := make([]redTeamSigner, 0, len(guardianIndices))
	for _, guardianIndex := range guardianIndices {
		signers = append(signers, redTeamSigner{keyIndex: uint64(guardianIndex), guardianIndex: guardianIndex})
	}
	return signers
}

func redTeamGuardianKey(keyIndex uint64) *ecdsa.PrivateKey {
	return devnet.InsecureDeterministicEcdsaKeyByIndex(keyIndex)
}

func redTeamGuardianAddress(keyIndex uint64) []byte {
	return crypto.PubkeyToAddress(redTeamGuardianKey(keyIndex).PublicKey).Bytes()
}

func coreGovernanceModuleID() []byte {
	moduleID := make([]byte, 32)
	copy(moduleID[28:], "Core")
	return moduleID
}

func tokenBridgeGovernanceModuleID() []byte {
	moduleID := make([]byte, 32)
	copy(moduleID[32-len("TokenBridge"):], "TokenBridge")
	return moduleID
}

func coreGovernanceHeader(moduleID []byte, action byte, targetChain vaa.ChainID) []byte {
	header := append([]byte{}, moduleID...)
	header = append(header, action)
	return binary.BigEndian.AppendUint16(header, uint16(targetChain))
}

func guardianSetUpgradePayload(targetChain vaa.ChainID, newGuardianSetIndex uint32, guardianKeyIndices []uint64) []byte {
	payload := coreGovernanceHeader(coreGovernanceModuleID(), coreGovernanceActionGuardianSetUpgrade, targetChain)
	payload = binary.BigEndian.AppendUint32(payload, newGuardianSetIndex)
	payload = append(payload, byte(len(guardianKeyIndices)))
	for _, keyIndex := range guardianKeyIndices {
		payload = append(payload, redTeamGuardianAddress(keyIndex)...)
	}
	return payload
}

// guardianSetUpgradePayload with raw 20-byte addresses, for an address without a private key.
func guardianSetUpgradePayloadWithAddresses(targetChain vaa.ChainID, newGuardianSetIndex uint32, guardianAddresses [][]byte) []byte {
	payload := coreGovernanceHeader(coreGovernanceModuleID(), coreGovernanceActionGuardianSetUpgrade, targetChain)
	payload = binary.BigEndian.AppendUint32(payload, newGuardianSetIndex)
	payload = append(payload, byte(len(guardianAddresses)))
	for _, guardianAddress := range guardianAddresses {
		if len(guardianAddress) != 20 {
			panic(fmt.Sprintf("guardian address of %d bytes, expected 20", len(guardianAddress)))
		}
		payload = append(payload, guardianAddress...)
	}
	return payload
}

func set1GuardianKeyIndices() []uint64 {
	keyIndices := make([]uint64, 0, redTeamSet1GuardianCount)
	for keyIndex := uint64(0); keyIndex < redTeamSet1GuardianCount; keyIndex++ {
		keyIndices = append(keyIndices, keyIndex)
	}
	return keyIndices
}

// setMessageFeePayloadWithFeeWord encodes the fee as a 32-byte big-endian word.
func setMessageFeePayloadWithFeeWord(moduleID []byte, action byte, targetChain vaa.ChainID, feeWord [32]byte) []byte {
	payload := coreGovernanceHeader(moduleID, action, targetChain)
	return append(payload, feeWord[:]...)
}

func feeWordFromUint64(fee uint64) [32]byte {
	var feeWord [32]byte
	binary.BigEndian.PutUint64(feeWord[24:], fee)
	return feeWord
}

func setMessageFeePayload(targetChain vaa.ChainID, fee uint64) []byte {
	return setMessageFeePayloadWithFeeWord(coreGovernanceModuleID(), coreGovernanceActionSetMessageFee, targetChain, feeWordFromUint64(fee))
}

func appendTrailingZeroByte(payload []byte) []byte {
	return append(append([]byte{}, payload...), 0x00)
}

type redTeamVAASpec struct {
	guardianSetIndex uint32
	signers          []redTeamSigner
	emitterChain     vaa.ChainID
	emitterAddress   vaa.Address
	sequence         uint64
	payload          []byte
}

func governanceVAASpec(guardianSetIndex uint32, signers []redTeamSigner, sequence uint64, payload []byte) redTeamVAASpec {
	return redTeamVAASpec{
		guardianSetIndex: guardianSetIndex,
		signers:          signers,
		emitterChain:     vaa.GovernanceChain,
		emitterAddress:   vaa.GovernanceEmitter,
		sequence:         sequence,
		payload:          payload,
	}
}

func encodeRedTeamVAAHex(t *testing.T, spec redTeamVAASpec) string {
	unsignedVAA := &vaa.VAA{
		Version:          vaa.SupportedVAAVersion,
		GuardianSetIndex: spec.guardianSetIndex,
		Timestamp:        time.Unix(fixtureTimestampUnixSeconds, 0),
		Nonce:            fixtureNonce,
		Sequence:         spec.sequence,
		ConsistencyLevel: fixtureConsistencyLevel,
		EmitterChain:     spec.emitterChain,
		EmitterAddress:   spec.emitterAddress,
		Payload:          spec.payload,
	}
	for _, signer := range spec.signers {
		unsignedVAA.AddSignature(redTeamGuardianKey(signer.keyIndex), signer.guardianIndex)
	}
	require.Len(t, unsignedVAA.Signatures, len(spec.signers))
	encodedVAA, err := unsignedVAA.Marshal()
	require.NoError(t, err)
	return hex.EncodeToString(encodedVAA)
}

func redTeamVAAFixture(name string, spec redTeamVAASpec) damlHexFixture {
	return damlHexFixture{
		name:               name,
		damlLiteralPattern: damlTopLevelHexPattern(name),
		generateHex:        func(t *testing.T) string { return encodeRedTeamVAAHex(t, spec) },
	}
}

func redTeamGuardianFixtures() []damlHexFixture {
	var fixtures []damlHexFixture
	for keyIndex := uint64(1); keyIndex < redTeamSet1GuardianCount; keyIndex++ {
		guardianKeyIndex := keyIndex
		fixtureName := fmt.Sprintf("redTeamGuardianPubKey%d", guardianKeyIndex)
		fixtures = append(fixtures, damlHexFixture{
			name:               fixtureName,
			damlLiteralPattern: damlTopLevelHexPattern(fixtureName),
			generateHex: func(t *testing.T) string {
				return hex.EncodeToString(crypto.FromECDSAPub(&redTeamGuardianKey(guardianKeyIndex).PublicKey))
			},
		})
	}
	return fixtures
}

func redTeamDamlHexFixtures() []damlHexFixture {
	set0Signer := redTeamSignersInOrder(0)
	set1Quorum := redTeamSignersInOrder(0, 1, 2)
	wrongGovernanceEmitter := vaa.GovernanceEmitter
	wrongGovernanceEmitter[31] = 0x05
	appEmitter := vaa.Address{}
	appEmitter[31] = 0xaa
	overflowingFeeWord := [32]byte{}
	overflowingFeeWord[23] = 0x01 // 2^64
	zeroGuardianAddress := make([]byte, 20)

	fixtures := redTeamGuardianFixtures()
	fixtures = append(fixtures,
		redTeamVAAFixture("rtGuardianSetUpgradeToSet1",
			governanceVAASpec(0, set0Signer, 100, guardianSetUpgradePayload(vaa.ChainIDCanton, 1, set1GuardianKeyIndices()))),
		redTeamVAAFixture("rtGuardianSetUpgradeSkipsIndex",
			governanceVAASpec(0, set0Signer, 101, guardianSetUpgradePayload(vaa.ChainIDCanton, 2, set1GuardianKeyIndices()))),
		redTeamVAAFixture("rtGuardianSetUpgradeSameIndex",
			governanceVAASpec(0, set0Signer, 102, guardianSetUpgradePayload(vaa.ChainIDCanton, 0, set1GuardianKeyIndices()))),
		redTeamVAAFixture("rtGuardianSetUpgradeLowerIndexBySet1",
			governanceVAASpec(1, set1Quorum, 103, guardianSetUpgradePayload(vaa.ChainIDCanton, 0, []uint64{0}))),
		redTeamVAAFixture("rtGuardianSetUpgradeWrongEmitterChain", redTeamVAASpec{
			guardianSetIndex: 0, signers: set0Signer, emitterChain: vaa.ChainIDEthereum, emitterAddress: vaa.GovernanceEmitter,
			sequence: 104, payload: guardianSetUpgradePayload(vaa.ChainIDCanton, 1, set1GuardianKeyIndices()),
		}),
		redTeamVAAFixture("rtGuardianSetUpgradeWrongEmitterAddress", redTeamVAASpec{
			guardianSetIndex: 0, signers: set0Signer, emitterChain: vaa.GovernanceChain, emitterAddress: wrongGovernanceEmitter,
			sequence: 105, payload: guardianSetUpgradePayload(vaa.ChainIDCanton, 1, set1GuardianKeyIndices()),
		}),
		redTeamVAAFixture("rtGuardianSetUpgradeWrongTargetChain",
			governanceVAASpec(0, set0Signer, 106, guardianSetUpgradePayload(vaa.ChainIDEthereum, 1, set1GuardianKeyIndices()))),
		redTeamVAAFixture("rtGuardianSetUpgradeEmptySet",
			governanceVAASpec(0, set0Signer, 107, guardianSetUpgradePayload(vaa.ChainIDCanton, 1, nil))),
		redTeamVAAFixture("rtGuardianSetUpgradeTrailingByte",
			governanceVAASpec(0, set0Signer, 108, appendTrailingZeroByte(guardianSetUpgradePayload(vaa.ChainIDCanton, 1, set1GuardianKeyIndices())))),
		redTeamVAAFixture("rtSetMessageFeeTrailingByte",
			governanceVAASpec(0, set0Signer, 109, appendTrailingZeroByte(setMessageFeePayload(vaa.ChainIDCanton, redTeamSetMessageFeeAmount)))),
		redTeamVAAFixture("rtSetMessageFeeWrongModule",
			governanceVAASpec(0, set0Signer, 110, setMessageFeePayloadWithFeeWord(tokenBridgeGovernanceModuleID(), coreGovernanceActionSetMessageFee, vaa.ChainIDCanton, feeWordFromUint64(redTeamSetMessageFeeAmount)))),
		redTeamVAAFixture("rtRecoverChainIDUnsupported",
			governanceVAASpec(0, set0Signer, 111, setMessageFeePayloadWithFeeWord(coreGovernanceModuleID(), coreGovernanceActionRecoverChainID, vaa.ChainIDCanton, feeWordFromUint64(redTeamSetMessageFeeAmount)))),
		redTeamVAAFixture("rtGovernanceEmptyPayload",
			governanceVAASpec(0, set0Signer, 112, []byte{})),
		redTeamVAAFixture("rtSetMessageFeeOverflowsInt64",
			governanceVAASpec(0, set0Signer, 113, setMessageFeePayloadWithFeeWord(coreGovernanceModuleID(), coreGovernanceActionSetMessageFee, vaa.ChainIDCanton, overflowingFeeWord))),
		redTeamVAAFixture("rtContractUpgradeTruncated",
			governanceVAASpec(0, set0Signer, 114, coreGovernanceHeader(coreGovernanceModuleID(), coreGovernanceActionContractUpgrade, vaa.ChainIDCanton))),
		redTeamVAAFixture("rtSetMessageFeeBySet1Quorum",
			governanceVAASpec(1, set1Quorum, 120, setMessageFeePayload(vaa.ChainIDCanton, redTeamSetMessageFeeAmount))),
		redTeamVAAFixture("rtSetMessageFeeBySet1BelowQuorum",
			governanceVAASpec(1, redTeamSignersInOrder(0, 1), 121, setMessageFeePayload(vaa.ChainIDCanton, redTeamSetMessageFeeAmount))),
		redTeamVAAFixture("rtSetMessageFeeBySet1DuplicateSigner",
			governanceVAASpec(1, redTeamSignersInOrder(0, 0, 1), 122, setMessageFeePayload(vaa.ChainIDCanton, redTeamSetMessageFeeAmount))),
		redTeamVAAFixture("rtSetMessageFeeBySet1SignersOutOfOrder",
			governanceVAASpec(1, redTeamSignersInOrder(1, 0, 2), 123, setMessageFeePayload(vaa.ChainIDCanton, redTeamSetMessageFeeAmount))),
		redTeamVAAFixture("rtSetMessageFeeBySet1WrongKeyAtIndex",
			governanceVAASpec(1, []redTeamSigner{{keyIndex: 3, guardianIndex: 0}, {keyIndex: 1, guardianIndex: 1}, {keyIndex: 2, guardianIndex: 2}}, 124, setMessageFeePayload(vaa.ChainIDCanton, redTeamSetMessageFeeAmount))),
		redTeamVAAFixture("rtSetMessageFeeBySet1IndexOutOfRange",
			governanceVAASpec(1, []redTeamSigner{{keyIndex: 0, guardianIndex: 0}, {keyIndex: 1, guardianIndex: 1}, {keyIndex: 3, guardianIndex: 7}}, 125, setMessageFeePayload(vaa.ChainIDCanton, redTeamSetMessageFeeAmount))),
		redTeamVAAFixture("rtSetMessageFeeBySet0AfterUpgrade",
			governanceVAASpec(0, set0Signer, 126, setMessageFeePayload(vaa.ChainIDCanton, redTeamSetMessageFeeAmount))),
		redTeamVAAFixture("rtGuardianSetUpgradeZeroAddress",
			governanceVAASpec(0, set0Signer, 127, guardianSetUpgradePayloadWithAddresses(vaa.ChainIDCanton, 1,
				[][]byte{redTeamGuardianAddress(0), zeroGuardianAddress}))),
		redTeamVAAFixture("rtGuardianSetUpgradeOnlyZeroAddress",
			governanceVAASpec(0, set0Signer, 128, guardianSetUpgradePayloadWithAddresses(vaa.ChainIDCanton, 1,
				[][]byte{zeroGuardianAddress}))),
		redTeamVAAFixture("rtGuardianSetUpgradeDuplicateAddress",
			governanceVAASpec(0, set0Signer, 129, guardianSetUpgradePayload(vaa.ChainIDCanton, 1, []uint64{1, 2, 1}))),
		redTeamVAAFixture("rtAppMessageBySet0", redTeamVAASpec{
			guardianSetIndex: 0, signers: set0Signer, emitterChain: redTeamAppEmitterChain, emitterAddress: appEmitter,
			sequence: 1, payload: []byte("hello"),
		}),
		redTeamVAAFixture("rtAppMessageBySet1AllSigners", redTeamVAASpec{
			guardianSetIndex: 1, signers: redTeamSignersInOrder(0, 1, 2, 3), emitterChain: redTeamAppEmitterChain, emitterAddress: appEmitter,
			sequence: 3, payload: []byte("hello"),
		}),
		redTeamVAAFixture("rtAppMessageEmptyPayloadBySet0", redTeamVAASpec{
			guardianSetIndex: 0, signers: set0Signer, emitterChain: redTeamAppEmitterChain, emitterAddress: appEmitter,
			sequence: 2, payload: []byte{},
		}),
	)
	return fixtures
}
