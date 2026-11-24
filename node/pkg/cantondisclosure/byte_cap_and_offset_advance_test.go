package cantondisclosure

import (
	"testing"

	"github.com/stretchr/testify/require"

	"github.com/certusone/wormhole/node/pkg/cantonclient"
	"github.com/certusone/wormhole/node/pkg/cantonclient/cantonclienttest"
)

func TestByteCapDropsReplayNodesAndKeepsServingSingletons(t *testing.T) {
	const byteCapBytes = int64(20_000)
	replayNodeEntryBytes := int64(testReplayNodeBlobBytes + indexEntryOverheadBytes)
	fillingBlobBytes := int(byteCapBytes - genesisUsedBytes - replayNodeEntryBytes - indexEntryOverheadBytes)
	testCases := []struct {
		name                      string
		bootstrapReplayNodes      []IndexedCoreContract
		transaction               func(index *CoreContractIndex) []coreContractIndexUpdate
		expectedDroppedUpdates    uint64
		expectedCoreStateContract func(index *CoreContractIndex) cantonclient.ContractID
	}{
		{
			name:                 "replay node create above the cap",
			bootstrapReplayNodes: []IndexedCoreContract{newTestReplayNodeContract(t, 1, testConsumer, testNamespace, "a", testReplayNodeBlobBytes)},
			transaction: func(index *CoreContractIndex) []coreContractIndexUpdate {
				oversizedNode := newTestReplayNodeContract(t, 2, testConsumer, testNamespace, "b", int(byteCapBytes))
				return []coreContractIndexUpdate{createdIndexUpdate(oversizedNode)}
			},
			expectedDroppedUpdates: 1,
		},
		{
			name: "bootstrap snapshot above the cap",
			bootstrapReplayNodes: []IndexedCoreContract{
				newTestReplayNodeContract(t, 1, testConsumer, testNamespace, "a", testReplayNodeBlobBytes),
				newTestReplayNodeContract(t, 2, testConsumer, testNamespace, "b", int(byteCapBytes)),
				newTestReplayNodeContract(t, 3, testConsumer, testNamespace, "c", testReplayNodeBlobBytes),
			},
			expectedDroppedUpdates: 2,
		},
		{
			name: "singleton replacement that fits only without the replay nodes",
			bootstrapReplayNodes: []IndexedCoreContract{
				newTestReplayNodeContract(t, 1, testConsumer, testNamespace, "a", testReplayNodeBlobBytes),
				newTestReplayNodeContract(t, 2, testConsumer, testNamespace, "b", fillingBlobBytes),
			},
			transaction: func(index *CoreContractIndex) []coreContractIndexUpdate {
				current, err := index.LookupSingleton(cantonclient.CoreContractTemplateCoreState)
				require.NoError(t, err)
				replacement := newTestSingletonContract(t, 900, cantonclient.CoreContractTemplateCoreState, testSingletonBlobBytes+int(replayNodeEntryBytes))
				return []coreContractIndexUpdate{archivedIndexUpdate(current.Contract()), createdIndexUpdate(replacement)}
			},
			expectedCoreStateContract: func(index *CoreContractIndex) cantonclient.ContractID { return testContractID(t, 900) },
		},
	}
	for _, testCase := range testCases {
		t.Run(testCase.name, func(t *testing.T) {
			index := newBootstrappedTestIndex(t, byteCapBytes, testCase.bootstrapReplayNodes...)
			if testCase.transaction != nil {
				require.True(t, index.Status().Readiness.ServesReplayNodeLookups())
				require.NoError(t, index.applyTransactionUpdates(testBootstrapOffset+1, testCase.transaction(index)))
			}
			status := index.Status()
			require.Equal(t, IndexReadinessByteCapReached, status.Readiness)
			require.True(t, status.ReplayNodesDroppedAtByteCap)
			require.Equal(t, 0, status.LiveReplayNodeCount)
			require.Equal(t, len(singletonTemplates), status.LiveContractCount)
			require.Equal(t, testCase.expectedDroppedUpdates, status.DroppedReplayNodeUpdateCount)
			require.LessOrEqual(t, status.UsedBytes, byteCapBytes)

			for _, template := range singletonTemplates {
				_, err := index.LookupSingleton(template)
				require.NoError(t, err, template.String())
			}
			if testCase.expectedCoreStateContract != nil {
				coreState, err := index.LookupSingleton(cantonclient.CoreContractTemplateCoreState)
				require.NoError(t, err)
				require.Equal(t, testCase.expectedCoreStateContract(index), coreState.Contract().ContractID())
			}
			_, err := index.LookupCoveringReplayNode(testPartyID(t, testConsumer), testReplayNamespace(t, testNamespace), testReplayDigest(t, testCoveredDigestHexText))
			requireIndexNotReady(t, err, IndexReadinessByteCapReached)
			_, err = index.LookupClaimedRootsNode(testPartyID(t, testConsumer), testReplayNamespace(t, testNamespace))
			requireIndexNotReady(t, err, IndexReadinessByteCapReached)
			_, err = index.compareWithIndexedSnapshot(status.Offset, testGenesisSingletons(t))
			require.ErrorIs(t, err, ErrReplayNodesDroppedAtByteCap)
		})
	}
}

func TestByteCapReachedKeepsApplyingSingletonUpdates(t *testing.T) {
	const byteCapBytes = int64(20_000)
	index := newBootstrappedTestIndex(t, byteCapBytes, newTestReplayNodeContract(t, 1, testConsumer, testNamespace, "", int(byteCapBytes)))
	require.Equal(t, IndexReadinessByteCapReached, index.Status().Readiness)

	current, err := index.LookupSingleton(cantonclient.CoreContractTemplateEmitterRegistry)
	require.NoError(t, err)
	replacement := newTestSingletonContract(t, 901, cantonclient.CoreContractTemplateEmitterRegistry, testSingletonBlobBytes)
	smallNode := newTestReplayNodeContract(t, 2, testConsumer, testNamespace, "", testReplayNodeBlobBytes)
	require.NoError(t, index.applyTransactionUpdates(11, []coreContractIndexUpdate{
		archivedIndexUpdate(current.Contract()),
		createdIndexUpdate(replacement),
		createdIndexUpdate(smallNode),
	}))
	require.NoError(t, index.AdvanceOffset(12))

	status := index.Status()
	require.Equal(t, IndexReadinessByteCapReached, status.Readiness)
	require.Equal(t, int64(12), status.Offset)
	require.Equal(t, uint64(2), status.DroppedReplayNodeUpdateCount)
	require.Equal(t, 0, status.LiveReplayNodeCount)
	result, err := index.LookupSingleton(cantonclient.CoreContractTemplateEmitterRegistry)
	require.NoError(t, err)
	require.Equal(t, replacement.ContractID(), result.Contract().ContractID())
	require.Equal(t, int64(12), result.LiveAtIndexOffset())
}

func TestAdvanceOffset(t *testing.T) {
	testCases := []struct {
		name            string
		prepare         func(t *testing.T) *CoreContractIndex
		offset          int64
		expectedError   error
		expectedOffset  int64
		expectNotReady  bool
		expectReadiness IndexReadiness
	}{
		{name: "before bootstrap", prepare: func(t *testing.T) *CoreContractIndex { return newTestIndex(t, testLargeIndexByteCap) }, offset: 11, expectedError: ErrIndexNotBootstrapped, expectedOffset: 0},
		{name: "equal to the index offset", prepare: func(t *testing.T) *CoreContractIndex { return newBootstrappedTestIndex(t, testLargeIndexByteCap) }, offset: testBootstrapOffset, expectedError: ErrOffsetNotIncreasing, expectedOffset: testBootstrapOffset},
		{name: "below the index offset", prepare: func(t *testing.T) *CoreContractIndex { return newBootstrappedTestIndex(t, testLargeIndexByteCap) }, offset: testBootstrapOffset - 1, expectedError: ErrOffsetNotIncreasing, expectedOffset: testBootstrapOffset},
		{name: "next offset", prepare: func(t *testing.T) *CoreContractIndex { return newBootstrappedTestIndex(t, testLargeIndexByteCap) }, offset: testBootstrapOffset + 1, expectedOffset: testBootstrapOffset + 1},
		{name: "offset gap", prepare: func(t *testing.T) *CoreContractIndex { return newBootstrappedTestIndex(t, testLargeIndexByteCap) }, offset: testBootstrapOffset + 1_000_000, expectedOffset: testBootstrapOffset + 1_000_000},
		{
			name: "inconsistent index",
			prepare: func(t *testing.T) *CoreContractIndex {
				node := newTestReplayNodeContract(t, 1, testConsumer, testNamespace, "", testReplayNodeBlobBytes)
				index := newBootstrappedTestIndex(t, testLargeIndexByteCap, node)
				require.Error(t, index.applyTransactionUpdates(11, []coreContractIndexUpdate{createdIndexUpdate(node)}))
				return index
			},
			offset: 12, expectedError: ErrIndexNotReady, expectedOffset: testBootstrapOffset,
		},
	}
	for _, testCase := range testCases {
		t.Run(testCase.name, func(t *testing.T) {
			index := testCase.prepare(t)
			readinessBefore := index.Status().Readiness
			err := index.AdvanceOffset(testCase.offset)
			if testCase.expectedError != nil {
				require.ErrorIs(t, err, testCase.expectedError)
			} else {
				require.NoError(t, err)
				result, lookupErr := index.LookupSingleton(cantonclient.CoreContractTemplateCoreState)
				require.NoError(t, lookupErr)
				require.Equal(t, testCase.offset, result.LiveAtIndexOffset())
			}
			require.Equal(t, testCase.expectedOffset, index.Status().Offset)
			require.Equal(t, readinessBefore, index.Status().Readiness)
		})
	}
}

func TestTransactionUpdateCountLimit(t *testing.T) {
	testCases := []struct {
		name          string
		updateCount   int
		expectedError error
	}{
		{name: "maximum", updateCount: maxCoreContractUpdatesPerTransaction},
		{name: "maximum plus one", updateCount: maxCoreContractUpdatesPerTransaction + 1, expectedError: ErrTransactionUpdateCount},
	}
	require.Equal(t, 65536, maxCoreContractUpdatesPerTransaction)
	unknownNode := newTestReplayNodeContract(t, 1, testConsumer, testNamespace, "", 1)
	for _, testCase := range testCases {
		t.Run(testCase.name, func(t *testing.T) {
			index := newBootstrappedTestIndex(t, testLargeIndexByteCap)
			updates := make([]coreContractIndexUpdate, testCase.updateCount)
			for updateIndex := range updates {
				updates[updateIndex] = archivedIndexUpdate(unknownNode)
			}
			err := index.applyTransactionUpdates(11, updates)
			if testCase.expectedError != nil {
				require.ErrorIs(t, err, testCase.expectedError)
				require.Equal(t, testBootstrapOffset, index.Status().Offset)
				require.ErrorIs(t, index.ApplyTransaction(11, make([]cantonclient.CoreContractUpdate, testCase.updateCount)), testCase.expectedError)
				return
			}
			require.NoError(t, err)
			require.Equal(t, uint64(testCase.updateCount), index.Status().IgnoredUnknownArchiveCount)
			publicErr := index.ApplyTransaction(12, make([]cantonclient.CoreContractUpdate, testCase.updateCount))
			require.Error(t, publicErr)
			require.NotErrorIs(t, publicErr, ErrTransactionUpdateCount)
		})
	}
}

func TestUnknownIndexUpdateKindIsRefusedBeforeAnyChange(t *testing.T) {
	node := newTestReplayNodeContract(t, 1, testConsumer, testNamespace, "", testReplayNodeBlobBytes)
	index := newBootstrappedTestIndex(t, testLargeIndexByteCap)
	statusBefore := index.Status()
	err := index.applyTransactionUpdates(11, []coreContractIndexUpdate{createdIndexUpdate(node), {kind: cantonclient.CoreContractUpdateKind(0)}})
	require.ErrorContains(t, err, "unknown kind 0")
	require.Equal(t, statusBefore, index.Status())
}

func TestSnapshotComparisonRecomputesUsedBytes(t *testing.T) {
	index := newBootstrappedTestIndex(t, testLargeIndexByteCap)
	_, err := index.compareWithIndexedSnapshot(testBootstrapOffset, testGenesisSingletons(t))
	require.NoError(t, err)
	index.usedBytes++
	require.Panics(t, func() {
		_, _ = index.compareWithIndexedSnapshot(testBootstrapOffset, testGenesisSingletons(t))
	})
}

func TestIndexReadinessServesLookups(t *testing.T) {
	testCases := []struct {
		readiness               IndexReadiness
		servesSingletonLookups  bool
		servesReplayNodeLookups bool
	}{
		{readiness: IndexReadinessBootstrapping},
		{readiness: IndexReadinessReady, servesSingletonLookups: true, servesReplayNodeLookups: true},
		{readiness: IndexReadinessReadyNearByteCap, servesSingletonLookups: true, servesReplayNodeLookups: true},
		{readiness: IndexReadinessSingletonMissing},
		{readiness: IndexReadinessSingletonConflict},
		{readiness: IndexReadinessByteCapReached, servesSingletonLookups: true},
		{readiness: IndexReadinessInconsistent},
		{readiness: IndexReadiness(0)},
	}
	for _, testCase := range testCases {
		t.Run(testCase.readiness.String(), func(t *testing.T) {
			require.Equal(t, testCase.servesSingletonLookups, testCase.readiness.ServesSingletonLookups())
			require.Equal(t, testCase.servesReplayNodeLookups, testCase.readiness.ServesReplayNodeLookups())
		})
	}
}

func TestBootstrapWithoutReplayNodes(t *testing.T) {
	ledger := testDecodingLedger()
	testCases := []struct {
		name              string
		snapshot          func(t *testing.T) []cantonclient.CreatedCoreContract
		expectedErrorText string
	}{
		{name: "singletons", snapshot: func(t *testing.T) []cantonclient.CreatedCoreContract { return decodedGenesisSnapshot(t, ledger) }},
		{name: "a replay node", snapshot: func(t *testing.T) []cantonclient.CreatedCoreContract {
			return decodedGenesisSnapshot(t, ledger, decodedReplayNodeEvent(1, ""))
		}, expectedErrorText: "ReplayNode in a snapshot without replay nodes"},
	}
	for _, testCase := range testCases {
		t.Run(testCase.name, func(t *testing.T) {
			index := newTestIndex(t, testLargeIndexByteCap)
			err := index.ApplyBootstrapSnapshotWithoutReplayNodes(testBootstrapOffset, testCase.snapshot(t))
			if testCase.expectedErrorText != "" {
				require.ErrorContains(t, err, testCase.expectedErrorText)
				require.Equal(t, IndexReadinessBootstrapping, index.Status().Readiness)
				return
			}
			require.NoError(t, err)
			status := index.Status()
			require.Equal(t, IndexReadinessByteCapReached, status.Readiness)
			require.True(t, status.ReplayNodesDroppedAtByteCap)
			require.Equal(t, testBootstrapOffset, status.Offset)
			for _, template := range singletonTemplates {
				_, lookupErr := index.LookupSingleton(template)
				require.NoError(t, lookupErr, template.String())
			}
			_, err = index.LookupCoveringReplayNode(testPartyID(t, testConsumer), testReplayNamespace(t, testNamespace), testReplayDigest(t, testCoveredDigestHexText))
			requireIndexNotReady(t, err, IndexReadinessByteCapReached)

			node := ledger.DecodeTransaction(t, testBootstrapOffset, 11, cantonclienttest.Created(decodedReplayNodeEvent(2, "")))
			require.NoError(t, index.ApplyTransaction(node.Offset(), node.Updates()))
			require.Equal(t, uint64(1), index.Status().DroppedReplayNodeUpdateCount)
			require.Equal(t, 0, index.Status().LiveReplayNodeCount)
		})
	}
}
