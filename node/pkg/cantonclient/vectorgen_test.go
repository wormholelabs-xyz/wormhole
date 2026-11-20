package cantonclient

// Generates the hex fixtures in canton/test/daml and asserts the Daml literals
// match (TestCore.daml and TestRedTeamCore.daml). Rewrite them after a deliberate change with:
//
//	CANTON_VECTORGEN=write go test -count=1 ./pkg/cantonclient/ -run TestCantonDamlFixtures
//
// The fixture file is outside the node module, so the Go test cache does not
// track it: run this guard with -count=1.

import (
	"encoding/binary"
	"encoding/hex"
	"os"
	"path/filepath"
	"regexp"
	"strconv"
	"testing"
	"time"

	"github.com/certusone/wormhole/node/pkg/devnet"
	"github.com/ethereum/go-ethereum/common"
	"github.com/ethereum/go-ethereum/crypto"
	"github.com/holiman/uint256"
	"github.com/stretchr/testify/require"
	"github.com/wormhole-foundation/wormhole/sdk/vaa"
)

const (
	damlTestCorePath = "../../../canton/test/daml/Test/TestCore.daml"

	devnetGuardianAddressHex = "befa429d57cd18b7f8a4d91a2da9ab4af05d0fbe"

	fixtureTimestampUnixSeconds = 1700000000
	fixtureNonce                = uint32(0)
	fixtureSequence             = uint64(1)
	fixtureConsistencyLevel     = uint8(0)
	fixtureGuardianSetIndex     = uint32(0)
	fixtureSetMessageFeeAmount  = uint64(1000)

	// Ethereum mainnet core getCurrentGuardianSetIndex() on 2026-11-18.
	nonZeroGenesisGuardianSetIndex = uint32(7)
	set7SetMessageFeeSequence      = uint64(7)
	set7GuardianSetUpgradeSequence = uint64(8)

	addressVectorRegistrar = "vector-registrar::1220deadbeefdeadbeefdeadbeefdeadbeefdeadbeefdeadbeefdeadbeefdeadbeef"
	addressVectorOwner     = "vector-owner::1220cafebabe"
	addressVectorEmitterID = uint64(7)
	emitterAddressTag      = "wormhole:emitter:v1"
)

type damlHexFixture struct {
	name string
	// First submatch is the hex literal.
	damlLiteralPattern *regexp.Regexp
	generateHex        func(t *testing.T) string
}

// Matches `binding = parseHexTextOrAbort "<hex>"`, the typed fixture form of Wormhole.Core.Bytes.
func damlTopLevelHexPattern(binding string) *regexp.Regexp {
	return regexp.MustCompile(`(?m)^` + regexp.QuoteMeta(binding) + ` = parseHexTextOrAbort "([0-9a-f]+)"$`)
}

// Matches the "=== \"<hex>\"" assertion within the indented body of binding.
func damlIndentedBodyHexPattern(binding string) *regexp.Regexp {
	return regexp.MustCompile(`(?m)^` + regexp.QuoteMeta(binding) + ` =\n(?: [^\n]*\n)*? +=== "([0-9a-f]{64})"$`)
}

func damlHexFixtures() []damlHexFixture {
	return []damlHexFixture{
		{
			name:               "devnetGuardian",
			damlLiteralPattern: damlTopLevelHexPattern("devnetGuardian"),
			generateHex: func(t *testing.T) string {
				guardianKey := devnet.InsecureDeterministicEcdsaKeyByIndex(0)
				guardianAddressHex := hex.EncodeToString(crypto.PubkeyToAddress(guardianKey.PublicKey).Bytes())
				require.Equal(t, devnetGuardianAddressHex, guardianAddressHex)
				return guardianAddressHex
			},
		},
		{
			name:               "devnetGuardianPubKey",
			damlLiteralPattern: damlTopLevelHexPattern("devnetGuardianPubKey"),
			generateHex: func(t *testing.T) string {
				guardianKey := devnet.InsecureDeterministicEcdsaKeyByIndex(0)
				return hex.EncodeToString(crypto.FromECDSAPub(&guardianKey.PublicKey))
			},
		},
		{
			name:               "govSetFeeVAA",
			damlLiteralPattern: damlTopLevelHexPattern("govSetFeeVAA"),
			generateHex:        generateGovSetMessageFeeVAAHex,
		},
		{
			name:               "govSetFeeVAASignedBySet7",
			damlLiteralPattern: damlTopLevelHexPattern("govSetFeeVAASignedBySet7"),
			generateHex:        generateGovSetMessageFeeVAASignedBySet7Hex,
		},
		{
			name:               "govGuardianSetUpgrade7To8VAA",
			damlLiteralPattern: damlTopLevelHexPattern("govGuardianSetUpgrade7To8VAA"),
			generateHex:        generateGuardianSetUpgrade7To8VAAHex,
		},
		{
			name:               "testAddressVector",
			damlLiteralPattern: damlIndentedBodyHexPattern("testAddressVector"),
			generateHex:        generateAddressVectorHex,
		},
	}
}

// SetMessageFee(chain = Canton, fee = 1000) from the governance emitter, signed by devnet guardian 0.
func generateGovSetMessageFeeVAAHex(t *testing.T) string {
	return encodeGovernanceVAASignedByDevnetKey0Hex(t, fixtureGuardianSetIndex, fixtureSequence, setMessageFeeFixturePayload(t))
}

// generateGovSetMessageFeeVAAHex under guardian set index 7.
func generateGovSetMessageFeeVAASignedBySet7Hex(t *testing.T) string {
	return encodeGovernanceVAASignedByDevnetKey0Hex(t, nonZeroGenesisGuardianSetIndex, set7SetMessageFeeSequence, setMessageFeeFixturePayload(t))
}

// GuardianSetUpgrade(chain = 0, new index 8, keys = [devnet key 1]), signed by devnet key 0 under set 7.
func generateGuardianSetUpgrade7To8VAAHex(t *testing.T) string {
	newGuardianKey := devnet.InsecureDeterministicEcdsaKeyByIndex(1)
	guardianSetUpgradePayload, err := vaa.BodyGuardianSetUpdate{
		Keys:     []common.Address{crypto.PubkeyToAddress(newGuardianKey.PublicKey)},
		NewIndex: nonZeroGenesisGuardianSetIndex + 1,
	}.Serialize()
	require.NoError(t, err)
	return encodeGovernanceVAASignedByDevnetKey0Hex(t, nonZeroGenesisGuardianSetIndex, set7GuardianSetUpgradeSequence, guardianSetUpgradePayload)
}

func setMessageFeeFixturePayload(t *testing.T) []byte {
	setMessageFeePayload, err := vaa.BodyCoreBridgeSetMessageFee{
		ChainID:    vaa.ChainIDCanton,
		MessageFee: uint256.NewInt(fixtureSetMessageFeeAmount),
	}.Serialize()
	require.NoError(t, err)
	return setMessageFeePayload
}

// A governance VAA from the governance emitter with one signature by devnet key 0 at guardian position 0.
func encodeGovernanceVAASignedByDevnetKey0Hex(t *testing.T, guardianSetIndex uint32, sequence uint64, payload []byte) string {
	governanceVAA := &vaa.VAA{
		Version:          vaa.SupportedVAAVersion,
		GuardianSetIndex: guardianSetIndex,
		Timestamp:        time.Unix(fixtureTimestampUnixSeconds, 0),
		Nonce:            fixtureNonce,
		Sequence:         sequence,
		ConsistencyLevel: fixtureConsistencyLevel,
		EmitterChain:     vaa.GovernanceChain,
		EmitterAddress:   vaa.GovernanceEmitter,
		Payload:          payload,
	}
	guardianKey := devnet.InsecureDeterministicEcdsaKeyByIndex(0)
	governanceVAA.AddSignature(guardianKey, 0)

	require.NoError(t, governanceVAA.Verify([]common.Address{common.HexToAddress(devnetGuardianAddressHex)}))
	otherGuardianKey := devnet.InsecureDeterministicEcdsaKeyByIndex(1)
	require.Error(t, governanceVAA.Verify([]common.Address{crypto.PubkeyToAddress(otherGuardianKey.PublicKey)}))

	encodedVAA, err := governanceVAA.Marshal()
	require.NoError(t, err)
	require.Equal(t, guardianSetIndex, binary.BigEndian.Uint32(encodedVAA[1:5]))
	return hex.EncodeToString(encodedVAA)
}

// keccak256(utf8(tag) || lp(registrar) || lp(owner) || uint64be(emitterID)), lp = uint32be length prefix.
func generateAddressVectorHex(t *testing.T) string {
	appendLengthPrefixed := func(preimage []byte, partyID string) []byte {
		var lengthPrefix [4]byte
		binary.BigEndian.PutUint32(lengthPrefix[:], uint32(len(partyID))) //nolint:gosec // fixture strings are short
		return append(append(preimage, lengthPrefix[:]...), partyID...)
	}
	preimage := []byte(emitterAddressTag)
	preimage = appendLengthPrefixed(preimage, addressVectorRegistrar)
	preimage = appendLengthPrefixed(preimage, addressVectorOwner)
	var emitterIDBytes [8]byte
	binary.BigEndian.PutUint64(emitterIDBytes[:], addressVectorEmitterID)
	preimage = append(preimage, emitterIDBytes[:]...)

	emitterAddress := crypto.Keccak256(preimage)
	require.Len(t, emitterAddress, 32)
	return hex.EncodeToString(emitterAddress)
}

const damlCoreStatePath = "../../../canton/core/daml/Wormhole/Core/State.daml"

var damlMaxEmitterIDPattern = regexp.MustCompile(`(?m)^maxEmitterId = ([0-9]+)$`)

// MaxEmitterID must equal Wormhole.Core.State.maxEmitterId. Run with -count=1 (see the file header).
func TestMaxEmitterIDMatchesDaml(t *testing.T) {
	damlSource, err := os.ReadFile(damlCoreStatePath)
	require.NoError(t, err)
	matches := damlMaxEmitterIDPattern.FindAllSubmatch(damlSource, -1)
	require.Len(t, matches, 1)
	require.Equal(t, strconv.FormatUint(MaxEmitterID, 10), string(matches[0][1]))
}

func TestCantonDamlFixtures(t *testing.T) {
	fixtureFiles := []struct {
		damlPath string
		fixtures []damlHexFixture
	}{
		{damlPath: damlTestCorePath, fixtures: damlHexFixtures()},
		{damlPath: damlRedTeamCorePath, fixtures: redTeamDamlHexFixtures()},
	}
	for _, fixtureFile := range fixtureFiles {
		t.Run(filepath.Base(fixtureFile.damlPath), func(t *testing.T) {
			checkOrRewriteDamlFixtures(t, fixtureFile.damlPath, fixtureFile.fixtures)
		})
	}
}

func checkOrRewriteDamlFixtures(t *testing.T, damlPath string, fixtures []damlHexFixture) {
	damlSourceBytes, err := os.ReadFile(damlPath)
	require.NoError(t, err)
	damlSource := string(damlSourceBytes)
	rewriteFixtures := os.Getenv("CANTON_VECTORGEN") == "write"

	for _, fixture := range fixtures {
		t.Run(fixture.name, func(t *testing.T) {
			matches := fixture.damlLiteralPattern.FindAllStringSubmatchIndex(damlSource, -1)
			require.Len(t, matches, 1, "fixture %s must appear exactly once in %s", fixture.name, damlPath)
			literalStart, literalEnd := matches[0][2], matches[0][3]

			generatedHex := fixture.generateHex(t)
			require.NotEmpty(t, generatedHex)

			if rewriteFixtures {
				damlSource = damlSource[:literalStart] + generatedHex + damlSource[literalEnd:]
				t.Logf("%s = %s", fixture.name, generatedHex)
				return
			}
			require.Equal(t, generatedHex, damlSource[literalStart:literalEnd],
				"Daml fixture drifted; rerun with CANTON_VECTORGEN=write")
		})
	}

	if rewriteFixtures {
		require.False(t, t.Failed(), "refusing to rewrite %s after a failed fixture", damlPath)
		writeFileAtomically(t, damlPath, []byte(damlSource))
	}
}

// writeFileAtomically replaces path via a same-directory temp file and rename, keeping path's mode.
func writeFileAtomically(t *testing.T, path string, contents []byte) {
	t.Helper()
	originalInfo, err := os.Stat(path)
	require.NoError(t, err)
	tempFile, err := os.CreateTemp(filepath.Dir(path), "."+filepath.Base(path)+".tmp-*")
	require.NoError(t, err)
	tempPath := tempFile.Name()
	t.Cleanup(func() { _ = os.Remove(tempPath) })

	_, writeErr := tempFile.Write(contents)
	syncErr := tempFile.Sync()
	closeErr := tempFile.Close()
	require.NoError(t, writeErr)
	require.NoError(t, syncErr)
	require.NoError(t, closeErr)
	require.NoError(t, os.Chmod(tempPath, originalInfo.Mode().Perm()))
	require.NoError(t, os.Rename(tempPath, path))
}
