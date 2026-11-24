package cantondisclosure

import (
	"testing"

	"github.com/stretchr/testify/require"

	"github.com/certusone/wormhole/node/pkg/cantonclient"
)

func TestCompareWithSnapshot(t *testing.T) {
	liveNode := newTestReplayNodeContract(t, 1, testConsumer, testNamespace, "a", testReplayNodeBlobBytes)
	otherLiveNode := newTestReplayNodeContract(t, 2, testConsumer, testNamespace, "b", testReplayNodeBlobBytes)
	unindexedNode := newTestReplayNodeContract(t, 3, testConsumer, testNamespace, "c", testReplayNodeBlobBytes)
	liveNodeWithOtherBlob := newTestReplayNodeContract(t, 1, testConsumer, testNamespace, "a", testReplayNodeBlobBytes+1)
	liveNodeWithOtherPrefix := newTestReplayNodeContract(t, 1, testConsumer, testNamespace, "d", testReplayNodeBlobBytes)

	testCases := []struct {
		name                  string
		snapshotReplayNodes   []IndexedCoreContract
		expectedDifferences   []SnapshotDifference
		expectedMissingCount  int
		expectedStaleCount    int
		expectedMismatchCount int
	}{
		{
			name:                "identical",
			snapshotReplayNodes: []IndexedCoreContract{liveNode, otherLiveNode},
		},
		{
			name:                 "contract missing from the index",
			snapshotReplayNodes:  []IndexedCoreContract{liveNode, otherLiveNode, unindexedNode},
			expectedDifferences:  []SnapshotDifference{{Kind: SnapshotDifferenceMissingFromIndex, ContractID: unindexedNode.ContractID(), Template: cantonclient.CoreContractTemplateReplayNode}},
			expectedMissingCount: 1,
		},
		{
			name:                "archived contract live in the index",
			snapshotReplayNodes: []IndexedCoreContract{liveNode},
			expectedDifferences: []SnapshotDifference{{Kind: SnapshotDifferenceStaleInIndex, ContractID: otherLiveNode.ContractID(), Template: cantonclient.CoreContractTemplateReplayNode}},
			expectedStaleCount:  1,
		},
		{
			name:                  "blob differs",
			snapshotReplayNodes:   []IndexedCoreContract{liveNodeWithOtherBlob, otherLiveNode},
			expectedDifferences:   []SnapshotDifference{{Kind: SnapshotDifferenceContentMismatch, ContractID: liveNode.ContractID(), Template: cantonclient.CoreContractTemplateReplayNode}},
			expectedMismatchCount: 1,
		},
		{
			name:                  "replay node location differs",
			snapshotReplayNodes:   []IndexedCoreContract{liveNodeWithOtherPrefix, otherLiveNode},
			expectedDifferences:   []SnapshotDifference{{Kind: SnapshotDifferenceContentMismatch, ContractID: liveNode.ContractID(), Template: cantonclient.CoreContractTemplateReplayNode}},
			expectedMismatchCount: 1,
		},
		{
			name:                 "every kind",
			snapshotReplayNodes:  []IndexedCoreContract{liveNodeWithOtherBlob, unindexedNode},
			expectedMissingCount: 1, expectedStaleCount: 1, expectedMismatchCount: 1,
			expectedDifferences: []SnapshotDifference{
				{Kind: SnapshotDifferenceContentMismatch, ContractID: liveNode.ContractID(), Template: cantonclient.CoreContractTemplateReplayNode},
				{Kind: SnapshotDifferenceMissingFromIndex, ContractID: unindexedNode.ContractID(), Template: cantonclient.CoreContractTemplateReplayNode},
				{Kind: SnapshotDifferenceStaleInIndex, ContractID: otherLiveNode.ContractID(), Template: cantonclient.CoreContractTemplateReplayNode},
			},
		},
	}
	for _, testCase := range testCases {
		t.Run(testCase.name, func(t *testing.T) {
			index := newBootstrappedTestIndex(t, testLargeIndexByteCap, liveNode, otherLiveNode)
			snapshot := append(testGenesisSingletons(t), testCase.snapshotReplayNodes...)
			comparison, err := index.compareWithIndexedSnapshot(testBootstrapOffset, snapshot)
			require.NoError(t, err)
			require.Equal(t, testBootstrapOffset, comparison.Offset())
			require.Equal(t, testCase.expectedDifferences, comparison.ReportedDifferences())
			require.Equal(t, testCase.expectedMissingCount, comparison.MissingFromIndexCount())
			require.Equal(t, testCase.expectedStaleCount, comparison.StaleInIndexCount())
			require.Equal(t, testCase.expectedMismatchCount, comparison.ContentMismatchCount())
			require.Equal(t, len(testCase.expectedDifferences) == 0, comparison.MatchesIndex())
		})
	}
}

func TestCompareWithSnapshotPreconditions(t *testing.T) {
	node := newTestReplayNodeContract(t, 1, testConsumer, testNamespace, "", testReplayNodeBlobBytes)
	testCases := []struct {
		name           string
		bootstrap      bool
		snapshotOffset int64
		snapshot       []IndexedCoreContract
		expectedError  error
	}{
		{name: "index not bootstrapped", bootstrap: false, snapshotOffset: testBootstrapOffset, expectedError: ErrIndexNotBootstrapped},
		{name: "snapshot offset below the index offset", bootstrap: true, snapshotOffset: testBootstrapOffset - 1, expectedError: ErrSnapshotOffsetMismatch},
		{name: "snapshot offset above the index offset", bootstrap: true, snapshotOffset: testBootstrapOffset + 1, expectedError: ErrSnapshotOffsetMismatch},
		{name: "duplicate contract id in the snapshot", bootstrap: true, snapshotOffset: testBootstrapOffset, snapshot: []IndexedCoreContract{node, node}, expectedError: ErrSnapshotRepeatsContractID},
	}
	for _, testCase := range testCases {
		t.Run(testCase.name, func(t *testing.T) {
			index := newTestIndex(t, testLargeIndexByteCap)
			if testCase.bootstrap {
				require.NoError(t, index.applyBootstrapSnapshot(testBootstrapOffset, testGenesisSingletons(t)))
			}
			_, err := index.compareWithIndexedSnapshot(testCase.snapshotOffset, testCase.snapshot)
			require.ErrorIs(t, err, testCase.expectedError)
		})
	}
}

func TestCompareWithSnapshotBoundsReportedDifferences(t *testing.T) {
	index := newBootstrappedTestIndex(t, testLargeIndexByteCap)
	snapshot := testGenesisSingletons(t)
	unindexedNodeCount := maxReportedSnapshotDifferences + 5
	for serial := 1; serial <= unindexedNodeCount; serial++ {
		snapshot = append(snapshot, newTestReplayNodeContract(t, serial, testConsumer, testNamespace, "", 1))
	}
	comparison, err := index.compareWithIndexedSnapshot(testBootstrapOffset, snapshot)
	require.NoError(t, err)
	require.Len(t, comparison.ReportedDifferences(), maxReportedSnapshotDifferences)
	require.Equal(t, unindexedNodeCount, comparison.MissingFromIndexCount())
	require.False(t, comparison.MatchesIndex())
}

func TestCompareWithSnapshotReturnsACopyOfTheDifferences(t *testing.T) {
	index := newBootstrappedTestIndex(t, testLargeIndexByteCap)
	comparison, err := index.compareWithIndexedSnapshot(testBootstrapOffset, nil)
	require.NoError(t, err)
	reported := comparison.ReportedDifferences()
	require.Len(t, reported, len(singletonTemplates))
	reported[0] = SnapshotDifference{}
	require.NotEqual(t, SnapshotDifference{}, comparison.ReportedDifferences()[0])
}
