package cantondisclosure

import (
	"fmt"
	"strings"
	"testing"

	"github.com/stretchr/testify/require"

	"github.com/certusone/wormhole/node/pkg/cantonclient"
	"github.com/certusone/wormhole/node/pkg/cantonclient/cantonclienttest"
)

const (
	testOtherGuardianGovernance = "otherGuardianGovernance::" + testFingerprint
	testOtherSynchronizer       = "other-synchronizer::" + testFingerprint
)

func testDecodingLedger() cantonclienttest.CoreContractLedger {
	return cantonclienttest.CoreContractLedger{
		GuardianGovernanceText: testGuardianGovernance,
		PackageIDHex:           strings.Repeat("3a", 32),
		SynchronizerIDText:     testSynchronizer,
	}
}

func testContractIDHex(serial int) string {
	return fmt.Sprintf("00%064x", serial)
}

func decodedSingletonEvent(serial int, template cantonclient.CoreContractTemplate) cantonclienttest.CoreContractEvent {
	return cantonclienttest.CoreContractEvent{
		Template:               template,
		ContractIDHex:          testContractIDHex(serial),
		GuardianGovernanceText: testGuardianGovernance,
		CreatedEventBlob:       testBlob(serial, testSingletonBlobBytes),
	}
}

func decodedReplayNodeEvent(serial int, prefixNibbles string) cantonclienttest.CoreContractEvent {
	return cantonclienttest.CoreContractEvent{
		Template:               cantonclient.CoreContractTemplateReplayNode,
		ContractIDHex:          testContractIDHex(serial),
		GuardianGovernanceText: testGuardianGovernance,
		ConsumerText:           testConsumer,
		NamespaceText:          testNamespace,
		PrefixNibbles:          prefixNibbles,
		CreatedEventBlob:       testBlob(serial, testReplayNodeBlobBytes),
	}
}

// decodedGenesisSnapshot is one decoded instance of each singleton template plus the replay nodes.
func decodedGenesisSnapshot(t *testing.T, ledger cantonclienttest.CoreContractLedger, replayNodes ...cantonclienttest.CoreContractEvent) []cantonclient.CreatedCoreContract {
	t.Helper()
	snapshot := []cantonclient.CreatedCoreContract{}
	for templateIndex, template := range singletonTemplates {
		snapshot = append(snapshot, ledger.DecodeCreated(t, decodedSingletonEvent(testGenesisSerialBase+templateIndex, template)))
	}
	for _, replayNode := range replayNodes {
		snapshot = append(snapshot, ledger.DecodeCreated(t, replayNode))
	}
	return snapshot
}

func TestIndexUpdateFromDecodedCoreContractUpdate(t *testing.T) {
	ledger := testDecodingLedger()
	otherGuardianGovernanceLedger := ledger
	otherGuardianGovernanceLedger.GuardianGovernanceText = testOtherGuardianGovernance
	otherGuardianGovernanceNode := decodedReplayNodeEvent(1, "a7")
	otherGuardianGovernanceNode.GuardianGovernanceText = testOtherGuardianGovernance

	createdNode := ledger.DecodeTransaction(t, 10, 11, cantonclienttest.Created(decodedReplayNodeEvent(1, "a7"))).Updates()[0]
	archivedNode := ledger.DecodeTransaction(t, 10, 11, cantonclienttest.Archived(decodedReplayNodeEvent(1, "a7"))).Updates()[0]
	foreignNode := otherGuardianGovernanceLedger.DecodeTransaction(t, 10, 11, cantonclienttest.Created(otherGuardianGovernanceNode)).Updates()[0]

	testCases := []struct {
		name              string
		update            cantonclient.CoreContractUpdate
		transactionOffset int64
		expectedKind      cantonclient.CoreContractUpdateKind
		expectedErrorText string
	}{
		{name: "valid create", update: createdNode, transactionOffset: 11, expectedKind: cantonclient.CoreContractUpdateKindCreated},
		{name: "valid archive", update: archivedNode, transactionOffset: 11, expectedKind: cantonclient.CoreContractUpdateKindArchived},
		{name: "other guardianGovernance", update: foreignNode, transactionOffset: 11, expectedErrorText: "is not the index guardianGovernance"},
		{name: "create at another offset", update: createdNode, transactionOffset: 12, expectedErrorText: "update offset 11 is not the transaction offset 12"},
		{name: "archive at another offset", update: archivedNode, transactionOffset: 10, expectedErrorText: "update offset 11 is not the transaction offset 10"},
		{name: "unknown kind", update: cantonclient.CoreContractUpdate{}, transactionOffset: 0, expectedErrorText: "unknown core contract update kind 0"},
	}
	guardianGovernance := testPartyID(t, testGuardianGovernance)
	for _, testCase := range testCases {
		t.Run(testCase.name, func(t *testing.T) {
			indexUpdate, err := indexUpdateFromCoreContractUpdate(testCase.update, testCase.transactionOffset, guardianGovernance)
			if testCase.expectedErrorText != "" {
				require.ErrorContains(t, err, testCase.expectedErrorText)
				require.Equal(t, coreContractIndexUpdate{}, indexUpdate)
				return
			}
			require.NoError(t, err)
			require.Equal(t, testCase.expectedKind, indexUpdate.kind)
			if testCase.expectedKind == cantonclient.CoreContractUpdateKindArchived {
				require.Equal(t, testContractIDHex(1), indexUpdate.archived.contractID.String())
				require.Equal(t, cantonclient.CoreContractTemplateReplayNode, indexUpdate.archived.template)
				return
			}
			contract := indexUpdate.created
			require.Equal(t, testContractIDHex(1), contract.ContractID().String())
			require.Equal(t, testPackageID(t), contract.TemplatePackageID())
			require.Equal(t, cantonclient.CoreContractTemplateReplayNode, contract.Template())
			require.Equal(t, testSynchronizer, contract.SynchronizerID().String())
			require.Equal(t, testBlob(1, testReplayNodeBlobBytes), contract.CreatedEventBlob())
			location, isReplayNode := contract.replayNodeLocationOf()
			require.True(t, isReplayNode)
			require.Equal(t, testConsumer, location.consumer.String())
			require.Equal(t, testNamespace, location.namespace.String())
			require.Equal(t, "a7", location.prefix.String())
		})
	}
}

func TestPublicIndexAPIWithDecodedValues(t *testing.T) {
	ledger := testDecodingLedger()
	root := decodedReplayNodeEvent(1, "")
	index := newTestIndex(t, testLargeIndexByteCap)
	require.NoError(t, index.ApplyBootstrapSnapshot(testBootstrapOffset, decodedGenesisSnapshot(t, ledger, root)))

	splitChanges := []cantonclienttest.CoreContractChange{cantonclienttest.Archived(root)}
	children := []cantonclienttest.CoreContractEvent{}
	for nibbleIndex, nibble := range "0123456789abcdef" {
		child := decodedReplayNodeEvent(100+nibbleIndex, string(nibble))
		children = append(children, child)
		splitChanges = append(splitChanges, cantonclienttest.Created(child))
	}
	split := ledger.DecodeTransaction(t, testBootstrapOffset, 11, splitChanges...)
	require.NoError(t, index.ApplyTransaction(split.Offset(), split.Updates()))
	require.NoError(t, index.AdvanceOffset(ledger.DecodeOffsetCheckpoint(t, 11, 14).Offset()))

	result, err := index.LookupCoveringReplayNode(testPartyID(t, testConsumer), testReplayNamespace(t, testNamespace), testReplayDigest(t, testCoveredDigestHexText))
	require.NoError(t, err)
	require.Equal(t, testContractIDHex(110), result.Contract().ContractID().String())
	require.Equal(t, int64(14), result.LiveAtIndexOffset())
	require.Equal(t, strings.Repeat("3a", 32)+":Wormhole.Core.Replay:ReplayNode", result.Contract().TemplateID())

	comparison, err := index.CompareWithSnapshot(14, decodedGenesisSnapshot(t, ledger, children...))
	require.NoError(t, err)
	require.True(t, comparison.MatchesIndex())
}

func TestCompareWithDecodedSnapshotReportsContentMismatch(t *testing.T) {
	ledger := testDecodingLedger()
	node := decodedReplayNodeEvent(1, "a")
	otherSynchronizerLedger := ledger
	otherSynchronizerLedger.SynchronizerIDText = testOtherSynchronizer
	otherPackageLedger := ledger
	otherPackageLedger.PackageIDHex = strings.Repeat("4b", 32)
	nodeWithOtherBlob := node
	nodeWithOtherBlob.CreatedEventBlob = testBlob(2, testReplayNodeBlobBytes)

	testCases := []struct {
		name             string
		snapshotNode     cantonclient.CreatedCoreContract
		expectedMismatch int
	}{
		{name: "same contract", snapshotNode: ledger.DecodeCreated(t, node)},
		{name: "other synchronizer", snapshotNode: otherSynchronizerLedger.DecodeCreated(t, node), expectedMismatch: 1},
		{name: "other template package", snapshotNode: otherPackageLedger.DecodeCreated(t, node), expectedMismatch: 1},
		{name: "other blob", snapshotNode: ledger.DecodeCreated(t, nodeWithOtherBlob), expectedMismatch: 1},
	}
	for _, testCase := range testCases {
		t.Run(testCase.name, func(t *testing.T) {
			index := newTestIndex(t, testLargeIndexByteCap)
			require.NoError(t, index.ApplyBootstrapSnapshot(testBootstrapOffset, decodedGenesisSnapshot(t, ledger, node)))
			snapshot := append(decodedGenesisSnapshot(t, ledger), testCase.snapshotNode)
			comparison, err := index.CompareWithSnapshot(testBootstrapOffset, snapshot)
			require.NoError(t, err)
			require.Equal(t, testCase.expectedMismatch, comparison.ContentMismatchCount())
			require.Equal(t, 0, comparison.MissingFromIndexCount())
			require.Equal(t, 0, comparison.StaleInIndexCount())
			require.Equal(t, testCase.expectedMismatch == 0, comparison.MatchesIndex())
		})
	}
}

func TestDecodedSnapshotOfAnotherGuardianGovernanceIsRefused(t *testing.T) {
	ledger := testDecodingLedger()
	otherLedger := ledger
	otherLedger.GuardianGovernanceText = testOtherGuardianGovernance
	foreignCoreState := decodedSingletonEvent(1, cantonclient.CoreContractTemplateCoreState)
	foreignCoreState.GuardianGovernanceText = testOtherGuardianGovernance
	snapshot := []cantonclient.CreatedCoreContract{otherLedger.DecodeCreated(t, foreignCoreState)}

	index := newTestIndex(t, testLargeIndexByteCap)
	require.ErrorContains(t, index.ApplyBootstrapSnapshot(testBootstrapOffset, snapshot), "is not the index guardianGovernance")
	require.Equal(t, IndexReadinessBootstrapping, index.Status().Readiness)

	require.NoError(t, index.ApplyBootstrapSnapshot(testBootstrapOffset, decodedGenesisSnapshot(t, ledger)))
	_, err := index.CompareWithSnapshot(testBootstrapOffset, snapshot)
	require.ErrorContains(t, err, "is not the index guardianGovernance")
}
