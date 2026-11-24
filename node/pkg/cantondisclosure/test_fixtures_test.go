package cantondisclosure

import (
	"fmt"
	"strings"
	"testing"

	"github.com/stretchr/testify/require"

	"github.com/certusone/wormhole/node/pkg/cantonclient"
)

const (
	testFingerprint          = "1220" + "9f9f9f9f9f9f9f9f9f9f9f9f9f9f9f9f9f9f9f9f9f9f9f9f9f9f9f9f9f9f9f9f"
	testGuardianGovernance   = "guardianGovernance::" + testFingerprint
	testConsumer             = "consumer::" + testFingerprint
	testOtherConsumer        = "otherConsumer::" + testFingerprint
	testNamespace            = "ntt-manager"
	testOtherNamespace       = "ntt-manager-2"
	testSynchronizer         = "global-synchronizer::" + testFingerprint
	testBootstrapOffset      = int64(10)
	testSingletonBlobBytes   = 100
	testReplayNodeBlobBytes  = 200
	testGenesisSerialBase    = 1_000_000
	testLargeIndexByteCap    = int64(1 << 30)
	testCoveredDigestHexText = "a7c4e1f0b2d3958667788990aabbccddeeff00112233445566778899aabbccdd"
)

func testPartyID(t *testing.T, text string) cantonclient.PartyID {
	t.Helper()
	party, err := cantonclient.ParsePartyID(text)
	require.NoError(t, err)
	return party
}

func testContractID(t *testing.T, serial int) cantonclient.ContractID {
	t.Helper()
	contractID, err := cantonclient.ParseContractID(fmt.Sprintf("00%064x", serial))
	require.NoError(t, err)
	return contractID
}

func testPackageID(t *testing.T) cantonclient.PackageID {
	t.Helper()
	packageID, err := cantonclient.ParsePackageID(strings.Repeat("3a", 32))
	require.NoError(t, err)
	return packageID
}

func testSynchronizerID(t *testing.T) cantonclient.SynchronizerID {
	t.Helper()
	synchronizerID, err := cantonclient.ParseSynchronizerID(testSynchronizer)
	require.NoError(t, err)
	return synchronizerID
}

func testReplayNamespace(t *testing.T, text string) cantonclient.ReplayNamespace {
	t.Helper()
	namespace, err := cantonclient.ParseReplayNamespace(text)
	require.NoError(t, err)
	return namespace
}

func testReplayDigest(t *testing.T, hexText string) ReplayDigest {
	t.Helper()
	digest, err := ParseReplayDigest(hexText)
	require.NoError(t, err)
	return digest
}

// testBlob is byteCount bytes that depend on serial, so blobs of distinct contracts differ.
func testBlob(serial int, byteCount int) []byte {
	blob := make([]byte, byteCount)
	for byteIndex := range blob {
		blob[byteIndex] = byte(serial + byteIndex)
	}
	return blob
}

func newTestSingletonContract(t *testing.T, serial int, template cantonclient.CoreContractTemplate, blobByteCount int) IndexedCoreContract {
	t.Helper()
	contract, err := newIndexedCoreContract(testContractID(t, serial), testPackageID(t), template, testSynchronizerID(t), replayNodeLocation{}, testBlob(serial, blobByteCount))
	require.NoError(t, err)
	return contract
}

func newTestReplayNodeContract(t *testing.T, serial int, consumerText string, namespaceText string, prefixNibbles string, blobByteCount int) IndexedCoreContract {
	t.Helper()
	prefix, err := cantonclient.ParseDigestNibblePrefix(prefixNibbles)
	require.NoError(t, err)
	location := replayNodeLocation{
		consumer:  testPartyID(t, consumerText),
		namespace: testReplayNamespace(t, namespaceText),
		prefix:    prefix,
	}
	contract, err := newIndexedCoreContract(testContractID(t, serial), testPackageID(t), cantonclient.CoreContractTemplateReplayNode, testSynchronizerID(t), location, testBlob(serial, blobByteCount))
	require.NoError(t, err)
	return contract
}

// testGenesisSingletons is one live instance of each singleton template.
func testGenesisSingletons(t *testing.T) []IndexedCoreContract {
	t.Helper()
	contracts := make([]IndexedCoreContract, 0, len(singletonTemplates))
	for templateIndex, template := range singletonTemplates {
		contracts = append(contracts, newTestSingletonContract(t, testGenesisSerialBase+templateIndex, template, testSingletonBlobBytes))
	}
	return contracts
}

func newTestIndex(t *testing.T, byteCapBytes int64) *CoreContractIndex {
	t.Helper()
	index, err := NewCoreContractIndex(testPartyID(t, testGuardianGovernance), byteCapBytes)
	require.NoError(t, err)
	return index
}

// newBootstrappedTestIndex bootstraps the genesis singletons plus contracts at testBootstrapOffset.
func newBootstrappedTestIndex(t *testing.T, byteCapBytes int64, contracts ...IndexedCoreContract) *CoreContractIndex {
	t.Helper()
	index := newTestIndex(t, byteCapBytes)
	snapshot := append(testGenesisSingletons(t), contracts...)
	require.NoError(t, index.applyBootstrapSnapshot(testBootstrapOffset, snapshot))
	return index
}

func createdIndexUpdate(contract IndexedCoreContract) coreContractIndexUpdate {
	return coreContractIndexUpdate{kind: cantonclient.CoreContractUpdateKindCreated, created: contract}
}

func archivedIndexUpdate(contract IndexedCoreContract) coreContractIndexUpdate {
	return coreContractIndexUpdate{
		kind:     cantonclient.CoreContractUpdateKindArchived,
		archived: archivedCoreContract{contractID: contract.contractID, template: contract.template},
	}
}

func requireIndexNotReady(t *testing.T, err error, expected IndexReadiness) {
	t.Helper()
	require.ErrorIs(t, err, ErrIndexNotReady)
	var notReady IndexNotReadyError
	require.ErrorAs(t, err, &notReady)
	require.Equal(t, expected, notReady.Readiness)
}
