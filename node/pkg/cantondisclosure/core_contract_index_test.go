package cantondisclosure

import (
	"sync"
	"testing"

	"github.com/stretchr/testify/require"

	"github.com/certusone/wormhole/node/pkg/cantonclient"
)

func TestNewCoreContractIndexByteCap(t *testing.T) {
	testCases := []struct {
		name         string
		byteCapBytes int64
		expectError  bool
	}{
		{name: "production default", byteCapBytes: ProductionIndexByteCapBytes},
		{name: "one entry overhead plus one blob byte", byteCapBytes: indexEntryOverheadBytes + 1},
		{name: "maximum", byteCapBytes: maxIndexByteCapBytes},
		{name: "zero", byteCapBytes: 0, expectError: true},
		{name: "negative", byteCapBytes: -1, expectError: true},
		{name: "one entry overhead only", byteCapBytes: indexEntryOverheadBytes, expectError: true},
		{name: "above maximum", byteCapBytes: maxIndexByteCapBytes + 1, expectError: true},
	}
	for _, testCase := range testCases {
		t.Run(testCase.name, func(t *testing.T) {
			index, err := NewCoreContractIndex(testPartyID(t, testGuardianGovernance), testCase.byteCapBytes)
			if testCase.expectError {
				require.Error(t, err)
				require.Nil(t, index)
				return
			}
			require.NoError(t, err)
			status := index.Status()
			require.Equal(t, IndexReadinessBootstrapping, status.Readiness)
			require.Equal(t, testCase.byteCapBytes, status.ByteCapBytes)
		})
	}
}

func TestNewCoreContractIndexRejectsZeroGuardianGovernance(t *testing.T) {
	index, err := NewCoreContractIndex(cantonclient.PartyID{}, ProductionIndexByteCapBytes)
	require.Error(t, err)
	require.Nil(t, index)
}

func TestProductionIndexByteCapIsFourGibibytes(t *testing.T) {
	require.Equal(t, int64(4*1024*1024*1024), ProductionIndexByteCapBytes)
}

func TestLookupsFailWhileBootstrapping(t *testing.T) {
	index := newTestIndex(t, testLargeIndexByteCap)
	_, err := index.LookupSingleton(cantonclient.CoreContractTemplateCoreState)
	requireIndexNotReady(t, err, IndexReadinessBootstrapping)
	_, err = index.LookupCoveringReplayNode(testPartyID(t, testConsumer), testReplayNamespace(t, testNamespace), testReplayDigest(t, testCoveredDigestHexText))
	requireIndexNotReady(t, err, IndexReadinessBootstrapping)
	_, err = index.LookupClaimedRootsNode(testPartyID(t, testConsumer), testReplayNamespace(t, testNamespace))
	requireIndexNotReady(t, err, IndexReadinessBootstrapping)
}

func TestBootstrapAndTransactionOrdering(t *testing.T) {
	testCases := []struct {
		name          string
		bootstrap     bool
		secondBoot    bool
		offset        int64
		updateCount   int
		expectedError error
	}{
		{name: "transaction before bootstrap", bootstrap: false, offset: 11, updateCount: 1, expectedError: ErrIndexNotBootstrapped},
		{name: "second bootstrap", bootstrap: true, secondBoot: true, expectedError: ErrIndexAlreadyBootstrapped},
		{name: "offset equal to the bootstrap offset", bootstrap: true, offset: testBootstrapOffset, updateCount: 1, expectedError: ErrOffsetNotIncreasing},
		{name: "offset below the bootstrap offset", bootstrap: true, offset: testBootstrapOffset - 1, updateCount: 1, expectedError: ErrOffsetNotIncreasing},
		{name: "empty transaction", bootstrap: true, offset: testBootstrapOffset + 1, updateCount: 0, expectedError: ErrTransactionUpdateCount},
		{name: "next offset", bootstrap: true, offset: testBootstrapOffset + 1, updateCount: 1},
		{name: "offset gap", bootstrap: true, offset: testBootstrapOffset + 1000, updateCount: 1},
	}
	for _, testCase := range testCases {
		t.Run(testCase.name, func(t *testing.T) {
			index := newTestIndex(t, testLargeIndexByteCap)
			if testCase.bootstrap {
				require.NoError(t, index.applyBootstrapSnapshot(testBootstrapOffset, testGenesisSingletons(t)))
			}
			if testCase.secondBoot {
				err := index.applyBootstrapSnapshot(testBootstrapOffset+1, testGenesisSingletons(t))
				require.ErrorIs(t, err, testCase.expectedError)
				return
			}
			updates := make([]coreContractIndexUpdate, 0, testCase.updateCount)
			for updateIndex := 0; updateIndex < testCase.updateCount; updateIndex++ {
				updates = append(updates, createdIndexUpdate(newTestReplayNodeContract(t, updateIndex+1, testConsumer, testNamespace, "", testReplayNodeBlobBytes)))
			}
			statusBefore := index.Status()
			err := index.applyTransactionUpdates(testCase.offset, updates)
			if testCase.expectedError != nil {
				require.ErrorIs(t, err, testCase.expectedError)
				require.Equal(t, statusBefore, index.Status())
				return
			}
			require.NoError(t, err)
			status := index.Status()
			require.Equal(t, testCase.offset, status.Offset)
			require.Equal(t, IndexReadinessReady, status.Readiness)
		})
	}
}

func TestBootstrapRejectsNegativeOffset(t *testing.T) {
	index := newTestIndex(t, testLargeIndexByteCap)
	require.Error(t, index.applyBootstrapSnapshot(-1, testGenesisSingletons(t)))
	require.Equal(t, IndexReadinessBootstrapping, index.Status().Readiness)
}

func TestBootstrapAtOffsetZeroOfAnEmptyLedger(t *testing.T) {
	index := newTestIndex(t, testLargeIndexByteCap)
	require.NoError(t, index.applyBootstrapSnapshot(0, nil))
	status := index.Status()
	require.Equal(t, int64(0), status.Offset)
	require.Equal(t, IndexReadinessSingletonMissing, status.Readiness)
	require.Equal(t, int64(0), status.UsedBytes)
}

func TestCreateThenArchiveServesNoContract(t *testing.T) {
	index := newBootstrappedTestIndex(t, testLargeIndexByteCap)
	node := newTestReplayNodeContract(t, 1, testConsumer, testNamespace, "", testReplayNodeBlobBytes)
	consumer := testPartyID(t, testConsumer)
	namespace := testReplayNamespace(t, testNamespace)
	digest := testReplayDigest(t, testCoveredDigestHexText)

	require.NoError(t, index.applyTransactionUpdates(11, []coreContractIndexUpdate{createdIndexUpdate(node)}))
	result, err := index.LookupCoveringReplayNode(consumer, namespace, digest)
	require.NoError(t, err)
	require.Equal(t, node.ContractID(), result.Contract().ContractID())
	require.Equal(t, int64(11), result.LiveAtIndexOffset())
	require.Equal(t, node.createdEventBlob, result.Contract().CreatedEventBlob())

	require.NoError(t, index.applyTransactionUpdates(12, []coreContractIndexUpdate{archivedIndexUpdate(node)}))
	_, err = index.LookupCoveringReplayNode(consumer, namespace, digest)
	require.ErrorIs(t, err, ErrNoCoveringReplayNode)
	status := index.Status()
	require.Equal(t, 0, status.LiveReplayNodeCount)
	require.Equal(t, int64(12), status.Offset)
	require.Equal(t, IndexReadinessReady, status.Readiness)
}

func TestSplitReplacesParentWithSixteenChildren(t *testing.T) {
	parent := newTestReplayNodeContract(t, 1, testConsumer, testNamespace, "a7", testReplayNodeBlobBytes)
	index := newBootstrappedTestIndex(t, testLargeIndexByteCap, parent)
	usedBytesBefore := index.Status().UsedBytes

	splitUpdates := []coreContractIndexUpdate{archivedIndexUpdate(parent)}
	children := map[string]IndexedCoreContract{}
	for nibbleIndex, nibble := range "0123456789abcdef" {
		child := newTestReplayNodeContract(t, 100+nibbleIndex, testConsumer, testNamespace, "a7"+string(nibble), testReplayNodeBlobBytes)
		children[string(nibble)] = child
		splitUpdates = append(splitUpdates, createdIndexUpdate(child))
	}
	require.NoError(t, index.applyTransactionUpdates(11, splitUpdates))

	consumer := testPartyID(t, testConsumer)
	namespace := testReplayNamespace(t, testNamespace)
	for nibble, child := range children {
		digestHexText := "a7" + nibble + testCoveredDigestHexText[3:]
		result, err := index.LookupCoveringReplayNode(consumer, namespace, testReplayDigest(t, digestHexText))
		require.NoError(t, err, "nibble %s", nibble)
		require.Equal(t, child.ContractID(), result.Contract().ContractID(), "nibble %s", nibble)
		require.Equal(t, int64(11), result.LiveAtIndexOffset())
	}
	status := index.Status()
	require.Equal(t, 16, status.LiveReplayNodeCount)
	require.Equal(t, usedBytesBefore+15*(testReplayNodeBlobBytes+indexEntryOverheadBytes), status.UsedBytes)
}

func TestCoveringReplayNodeLookupAtEachPrefixDepth(t *testing.T) {
	consumer := testPartyID(t, testConsumer)
	namespace := testReplayNamespace(t, testNamespace)
	digest := testReplayDigest(t, testCoveredDigestHexText)
	for nibbleCount := 0; nibbleCount < coveringPrefixCandidateCount; nibbleCount++ {
		node := newTestReplayNodeContract(t, 1, testConsumer, testNamespace, testCoveredDigestHexText[:nibbleCount], testReplayNodeBlobBytes)
		index := newBootstrappedTestIndex(t, testLargeIndexByteCap, node)
		result, err := index.LookupCoveringReplayNode(consumer, namespace, digest)
		require.NoError(t, err, "depth %d", nibbleCount)
		require.Equal(t, node.ContractID(), result.Contract().ContractID(), "depth %d", nibbleCount)
		location, isReplayNode := result.Contract().replayNodeLocationOf()
		require.True(t, isReplayNode)
		require.Equal(t, nibbleCount, location.prefix.NibbleCount())
	}
}

type testReplayNodeSpecification struct {
	serial        int
	consumerText  string
	namespaceText string
	prefixNibbles string
}

func TestCoveringReplayNodeLookupResults(t *testing.T) {
	siblingPrefix := "b"
	testCases := []struct {
		name               string
		nodes              []testReplayNodeSpecification
		expectedSerial     int
		expectedError      error
		expectedNodeCount  int
		expectMultipleNode bool
	}{
		{
			name:           "root only",
			nodes:          []testReplayNodeSpecification{{1, testConsumer, testNamespace, ""}},
			expectedSerial: 1,
		},
		{
			name: "covering node among siblings",
			nodes: []testReplayNodeSpecification{
				{1, testConsumer, testNamespace, "a"},
				{2, testConsumer, testNamespace, siblingPrefix},
			},
			expectedSerial: 1,
		},
		{
			name:          "no node",
			nodes:         nil,
			expectedError: ErrNoCoveringReplayNode,
		},
		{
			name:          "sibling only",
			nodes:         []testReplayNodeSpecification{{1, testConsumer, testNamespace, siblingPrefix}},
			expectedError: ErrNoCoveringReplayNode,
		},
		{
			name:          "other consumer only",
			nodes:         []testReplayNodeSpecification{{1, testOtherConsumer, testNamespace, ""}},
			expectedError: ErrNoCoveringReplayNode,
		},
		{
			name:          "other namespace only",
			nodes:         []testReplayNodeSpecification{{1, testConsumer, testOtherNamespace, ""}},
			expectedError: ErrNoCoveringReplayNode,
		},
		{
			name: "parent and child both live",
			nodes: []testReplayNodeSpecification{
				{1, testConsumer, testNamespace, ""},
				{2, testConsumer, testNamespace, "a7"},
			},
			expectedError:     ErrMultipleCoveringReplayNodes,
			expectedNodeCount: 2,
		},
		{
			name: "two roots of one trie",
			nodes: []testReplayNodeSpecification{
				{1, testConsumer, testNamespace, ""},
				{2, testConsumer, testNamespace, ""},
			},
			expectedError:     ErrMultipleCoveringReplayNodes,
			expectedNodeCount: 2,
		},
		{
			name: "three covering nodes",
			nodes: []testReplayNodeSpecification{
				{1, testConsumer, testNamespace, ""},
				{2, testConsumer, testNamespace, "a"},
				{3, testConsumer, testNamespace, testCoveredDigestHexText[:63]},
			},
			expectedError:     ErrMultipleCoveringReplayNodes,
			expectedNodeCount: 3,
		},
	}
	consumer := testPartyID(t, testConsumer)
	namespace := testReplayNamespace(t, testNamespace)
	digest := testReplayDigest(t, testCoveredDigestHexText)
	for _, testCase := range testCases {
		t.Run(testCase.name, func(t *testing.T) {
			nodes := make([]IndexedCoreContract, 0, len(testCase.nodes))
			for _, specification := range testCase.nodes {
				nodes = append(nodes, newTestReplayNodeContract(t, specification.serial, specification.consumerText, specification.namespaceText, specification.prefixNibbles, testReplayNodeBlobBytes))
			}
			index := newBootstrappedTestIndex(t, testLargeIndexByteCap, nodes...)
			result, err := index.LookupCoveringReplayNode(consumer, namespace, digest)
			if testCase.expectedError != nil {
				require.ErrorIs(t, err, testCase.expectedError)
				require.Equal(t, CoreContractLookupResult{}, result)
				if testCase.expectedNodeCount > 0 {
					var multiple MultipleCoveringReplayNodesError
					require.ErrorAs(t, err, &multiple)
					require.Equal(t, testCase.expectedNodeCount, multiple.CoveringNodeCount)
				}
				require.Equal(t, IndexReadinessReady, index.Status().Readiness)
				return
			}
			require.NoError(t, err)
			require.Equal(t, testContractID(t, testCase.expectedSerial), result.Contract().ContractID())
		})
	}
}

func TestArchiveOfUnknownContractIsCountedAndIgnored(t *testing.T) {
	node := newTestReplayNodeContract(t, 1, testConsumer, testNamespace, "", testReplayNodeBlobBytes)
	index := newBootstrappedTestIndex(t, testLargeIndexByteCap, node)
	statusBefore := index.Status()
	unknownNode := newTestReplayNodeContract(t, 2, testConsumer, testNamespace, "", testReplayNodeBlobBytes)

	require.NoError(t, index.applyTransactionUpdates(11, []coreContractIndexUpdate{archivedIndexUpdate(unknownNode)}))
	status := index.Status()
	require.Equal(t, uint64(1), status.IgnoredUnknownArchiveCount)
	require.Equal(t, IndexReadinessReady, status.Readiness)
	require.Equal(t, int64(11), status.Offset)
	require.Equal(t, statusBefore.UsedBytes, status.UsedBytes)
	require.Equal(t, statusBefore.LiveContractCount, status.LiveContractCount)

	result, err := index.LookupCoveringReplayNode(testPartyID(t, testConsumer), testReplayNamespace(t, testNamespace), testReplayDigest(t, testCoveredDigestHexText))
	require.NoError(t, err)
	require.Equal(t, node.ContractID(), result.Contract().ContractID())
}

func TestArchiveMatchesByContractIDAndChecksTemplate(t *testing.T) {
	node := newTestReplayNodeContract(t, 1, testConsumer, testNamespace, "", testReplayNodeBlobBytes)
	index := newBootstrappedTestIndex(t, testLargeIndexByteCap, node)
	mismatchedArchive := coreContractIndexUpdate{
		kind:     cantonclient.CoreContractUpdateKindArchived,
		archived: archivedCoreContract{contractID: node.ContractID(), template: cantonclient.CoreContractTemplateCoreState},
	}
	err := index.applyTransactionUpdates(11, []coreContractIndexUpdate{mismatchedArchive})
	require.ErrorIs(t, err, ErrIndexInconsistent)
	require.Equal(t, IndexReadinessInconsistent, index.Status().Readiness)
	_, err = index.LookupSingleton(cantonclient.CoreContractTemplateCoreState)
	requireIndexNotReady(t, err, IndexReadinessInconsistent)
}

func TestDuplicateCreateMakesIndexInconsistent(t *testing.T) {
	node := newTestReplayNodeContract(t, 1, testConsumer, testNamespace, "", testReplayNodeBlobBytes)
	testCases := []struct {
		name      string
		apply     func(index *CoreContractIndex) error
		bootstrap []IndexedCoreContract
	}{
		{
			name:      "create of a contract id from the snapshot",
			bootstrap: []IndexedCoreContract{node},
			apply: func(index *CoreContractIndex) error {
				return index.applyTransactionUpdates(11, []coreContractIndexUpdate{createdIndexUpdate(node)})
			},
		},
		{
			name: "two creates of one contract id in one transaction",
			apply: func(index *CoreContractIndex) error {
				return index.applyTransactionUpdates(11, []coreContractIndexUpdate{createdIndexUpdate(node), createdIndexUpdate(node)})
			},
		},
		{
			name: "two creates of one contract id in two transactions",
			apply: func(index *CoreContractIndex) error {
				if err := index.applyTransactionUpdates(11, []coreContractIndexUpdate{createdIndexUpdate(node)}); err != nil {
					return err
				}
				return index.applyTransactionUpdates(12, []coreContractIndexUpdate{createdIndexUpdate(node)})
			},
		},
	}
	for _, testCase := range testCases {
		t.Run(testCase.name, func(t *testing.T) {
			index := newBootstrappedTestIndex(t, testLargeIndexByteCap, testCase.bootstrap...)
			err := testCase.apply(index)
			require.ErrorIs(t, err, ErrDuplicateCreate)
			require.Equal(t, IndexReadinessInconsistent, index.Status().Readiness)
			_, err = index.LookupCoveringReplayNode(testPartyID(t, testConsumer), testReplayNamespace(t, testNamespace), testReplayDigest(t, testCoveredDigestHexText))
			requireIndexNotReady(t, err, IndexReadinessInconsistent)
			err = index.applyTransactionUpdates(100, []coreContractIndexUpdate{archivedIndexUpdate(node)})
			requireIndexNotReady(t, err, IndexReadinessInconsistent)
		})
	}
}

func TestDuplicateContractIDInBootstrapSnapshot(t *testing.T) {
	node := newTestReplayNodeContract(t, 1, testConsumer, testNamespace, "", testReplayNodeBlobBytes)
	index := newTestIndex(t, testLargeIndexByteCap)
	err := index.applyBootstrapSnapshot(testBootstrapOffset, append(testGenesisSingletons(t), node, node))
	require.ErrorIs(t, err, ErrDuplicateCreate)
	require.Equal(t, IndexReadinessInconsistent, index.Status().Readiness)
}

func TestSingletonConflictAndRecovery(t *testing.T) {
	for templateIndex, template := range singletonTemplates {
		t.Run(template.String(), func(t *testing.T) {
			index := newBootstrappedTestIndex(t, testLargeIndexByteCap)
			first, err := index.LookupSingleton(template)
			require.NoError(t, err)
			require.Equal(t, testContractID(t, testGenesisSerialBase+templateIndex), first.Contract().ContractID())
			require.Equal(t, template, first.Contract().Template())
			require.Equal(t, testBootstrapOffset, first.LiveAtIndexOffset())

			second := newTestSingletonContract(t, 500, template, testSingletonBlobBytes)
			require.NoError(t, index.applyTransactionUpdates(11, []coreContractIndexUpdate{createdIndexUpdate(second)}))
			require.Equal(t, IndexReadinessSingletonConflict, index.Status().Readiness)
			_, err = index.LookupSingleton(template)
			requireIndexNotReady(t, err, IndexReadinessSingletonConflict)
			_, err = index.LookupCoveringReplayNode(testPartyID(t, testConsumer), testReplayNamespace(t, testNamespace), testReplayDigest(t, testCoveredDigestHexText))
			requireIndexNotReady(t, err, IndexReadinessSingletonConflict)

			require.NoError(t, index.applyTransactionUpdates(12, []coreContractIndexUpdate{archivedIndexUpdate(first.Contract())}))
			require.Equal(t, IndexReadinessReady, index.Status().Readiness)
			recovered, err := index.LookupSingleton(template)
			require.NoError(t, err)
			require.Equal(t, second.ContractID(), recovered.Contract().ContractID())
			require.Equal(t, int64(12), recovered.LiveAtIndexOffset())
		})
	}
}

func TestSingletonMissingAndRecovery(t *testing.T) {
	for templateIndex, missingTemplate := range singletonTemplates {
		t.Run(missingTemplate.String(), func(t *testing.T) {
			snapshot := []IndexedCoreContract{}
			for _, singleton := range testGenesisSingletons(t) {
				if singleton.Template() != missingTemplate {
					snapshot = append(snapshot, singleton)
				}
			}
			require.Len(t, snapshot, len(singletonTemplates)-1)
			index := newTestIndex(t, testLargeIndexByteCap)
			require.NoError(t, index.applyBootstrapSnapshot(testBootstrapOffset, snapshot))
			require.Equal(t, IndexReadinessSingletonMissing, index.Status().Readiness)
			_, err := index.LookupSingleton(missingTemplate)
			requireIndexNotReady(t, err, IndexReadinessSingletonMissing)

			created := newTestSingletonContract(t, 700+templateIndex, missingTemplate, testSingletonBlobBytes)
			require.NoError(t, index.applyTransactionUpdates(11, []coreContractIndexUpdate{createdIndexUpdate(created)}))
			require.Equal(t, IndexReadinessReady, index.Status().Readiness)
			result, err := index.LookupSingleton(missingTemplate)
			require.NoError(t, err)
			require.Equal(t, created.ContractID(), result.Contract().ContractID())
		})
	}
}

func TestSingletonUpgradeInOneTransactionStaysReady(t *testing.T) {
	index := newBootstrappedTestIndex(t, testLargeIndexByteCap)
	current, err := index.LookupSingleton(cantonclient.CoreContractTemplateCoreState)
	require.NoError(t, err)
	replacement := newTestSingletonContract(t, 800, cantonclient.CoreContractTemplateCoreState, testSingletonBlobBytes)
	require.NoError(t, index.applyTransactionUpdates(11, []coreContractIndexUpdate{archivedIndexUpdate(current.Contract()), createdIndexUpdate(replacement)}))
	require.Equal(t, IndexReadinessReady, index.Status().Readiness)
	result, err := index.LookupSingleton(cantonclient.CoreContractTemplateCoreState)
	require.NoError(t, err)
	require.Equal(t, replacement.ContractID(), result.Contract().ContractID())
}

func TestLookupSingletonRejectsReplayNodeTemplate(t *testing.T) {
	index := newBootstrappedTestIndex(t, testLargeIndexByteCap)
	_, err := index.LookupSingleton(cantonclient.CoreContractTemplateReplayNode)
	require.Error(t, err)
	_, err = index.LookupSingleton(cantonclient.CoreContractTemplate(0))
	require.Error(t, err)
}

// genesisUsedBytes is the byte count of testGenesisSingletons.
const genesisUsedBytes = int64(len(singletonTemplates)) * (testSingletonBlobBytes + indexEntryOverheadBytes)

func TestByteCapBoundaries(t *testing.T) {
	const byteCapBytes = int64(100_000)
	warningThresholdBytes := (byteCapBytes*byteCapWarningPercent + 99) / 100
	require.Equal(t, int64(80_000), warningThresholdBytes)
	testCases := []struct {
		name               string
		totalBytesAfter    int64
		expectRefused      bool
		expectedReadiness  IndexReadiness
		expectedUsedBytes  int64
		expectLookupServed bool
	}{
		{name: "warning threshold minus 1", totalBytesAfter: warningThresholdBytes - 1, expectedReadiness: IndexReadinessReady, expectLookupServed: true},
		{name: "warning threshold", totalBytesAfter: warningThresholdBytes, expectedReadiness: IndexReadinessReadyNearByteCap, expectLookupServed: true},
		{name: "cap minus 1", totalBytesAfter: byteCapBytes - 1, expectedReadiness: IndexReadinessReadyNearByteCap, expectLookupServed: true},
		{name: "exactly the cap", totalBytesAfter: byteCapBytes, expectedReadiness: IndexReadinessReadyNearByteCap, expectLookupServed: true},
		{name: "cap plus 1", totalBytesAfter: byteCapBytes + 1, expectRefused: true, expectedReadiness: IndexReadinessByteCapReached},
	}
	for _, testCase := range testCases {
		t.Run(testCase.name, func(t *testing.T) {
			index := newBootstrappedTestIndex(t, byteCapBytes)
			blobByteCount := testCase.totalBytesAfter - genesisUsedBytes - indexEntryOverheadBytes
			require.Positive(t, blobByteCount)
			node := newTestReplayNodeContract(t, 1, testConsumer, testNamespace, "", int(blobByteCount))

			err := index.applyTransactionUpdates(11, []coreContractIndexUpdate{createdIndexUpdate(node)})
			status := index.Status()
			require.Equal(t, testCase.expectedReadiness, status.Readiness)
			lookupResult, lookupErr := index.LookupCoveringReplayNode(testPartyID(t, testConsumer), testReplayNamespace(t, testNamespace), testReplayDigest(t, testCoveredDigestHexText))
			if testCase.expectRefused {
				require.NoError(t, err)
				require.Equal(t, genesisUsedBytes, status.UsedBytes)
				require.Equal(t, 0, status.LiveReplayNodeCount)
				require.True(t, status.ReplayNodesDroppedAtByteCap)
				require.Equal(t, uint64(1), status.DroppedReplayNodeUpdateCount)
				requireIndexNotReady(t, lookupErr, IndexReadinessByteCapReached)
				_, singletonErr := index.LookupSingleton(cantonclient.CoreContractTemplateCoreState)
				require.NoError(t, singletonErr)
				require.NoError(t, index.applyTransactionUpdates(12, []coreContractIndexUpdate{archivedIndexUpdate(node)}))
				require.Equal(t, uint64(2), index.Status().DroppedReplayNodeUpdateCount)
				require.Equal(t, uint64(0), index.Status().IgnoredUnknownArchiveCount)
				return
			}
			require.NoError(t, err)
			require.Equal(t, testCase.totalBytesAfter, status.UsedBytes)
			require.NoError(t, lookupErr)
			require.Equal(t, node.ContractID(), lookupResult.Contract().ContractID())
		})
	}
}

func TestBootstrapSingletonsAboveByteCapMakeIndexInconsistent(t *testing.T) {
	index := newTestIndex(t, genesisUsedBytes-1)
	err := index.applyBootstrapSnapshot(testBootstrapOffset, testGenesisSingletons(t))
	require.ErrorIs(t, err, ErrByteCapExceeded)
	require.Equal(t, IndexReadinessInconsistent, index.Status().Readiness)
	_, err = index.LookupSingleton(cantonclient.CoreContractTemplateCoreState)
	requireIndexNotReady(t, err, IndexReadinessInconsistent)
}

func TestArchiveReleasesBytes(t *testing.T) {
	node := newTestReplayNodeContract(t, 1, testConsumer, testNamespace, "", testReplayNodeBlobBytes)
	index := newBootstrappedTestIndex(t, testLargeIndexByteCap, node)
	require.Equal(t, genesisUsedBytes+testReplayNodeBlobBytes+indexEntryOverheadBytes, index.Status().UsedBytes)
	require.NoError(t, index.applyTransactionUpdates(11, []coreContractIndexUpdate{archivedIndexUpdate(node)}))
	require.Equal(t, genesisUsedBytes, index.Status().UsedBytes)
}

func TestLookupReturnsACopyOfTheBlob(t *testing.T) {
	index := newBootstrappedTestIndex(t, testLargeIndexByteCap)
	first, err := index.LookupSingleton(cantonclient.CoreContractTemplateCoreState)
	require.NoError(t, err)
	servedBlob := first.Contract().CreatedEventBlob()
	expectedBlob := append([]byte(nil), servedBlob...)
	servedBlob[0] ^= 0xff
	second, err := index.LookupSingleton(cantonclient.CoreContractTemplateCoreState)
	require.NoError(t, err)
	require.Equal(t, expectedBlob, second.Contract().CreatedEventBlob())
}

func TestClaimedRootsNodeLookup(t *testing.T) {
	consumer := testPartyID(t, testConsumer)
	namespace := testReplayNamespace(t, testNamespace)
	pairDigestHexText := ClaimedRootDigest(consumer, namespace).String()
	otherNibble := "0"
	if pairDigestHexText[:1] == otherNibble {
		otherNibble = "1"
	}
	testCases := []struct {
		name            string
		lookupNamespace string
		nodes           []testReplayNodeSpecification
		expectedSerial  int
		expectedError   error
	}{
		{
			name:            "genesis root",
			lookupNamespace: testNamespace,
			nodes:           []testReplayNodeSpecification{{1, testGuardianGovernance, claimedRootsNamespace, ""}},
			expectedSerial:  1,
		},
		{
			name:            "child after a split",
			lookupNamespace: testNamespace,
			nodes: []testReplayNodeSpecification{
				{1, testGuardianGovernance, claimedRootsNamespace, pairDigestHexText[:1]},
				{2, testGuardianGovernance, claimedRootsNamespace, otherNibble},
			},
			expectedSerial: 1,
		},
		{
			name:            "deepest node",
			lookupNamespace: testNamespace,
			nodes:           []testReplayNodeSpecification{{1, testGuardianGovernance, claimedRootsNamespace, pairDigestHexText[:63]}},
			expectedSerial:  1,
		},
		{
			name:            "claimed-roots tree of the consumer instead of guardianGovernance",
			lookupNamespace: testNamespace,
			nodes:           []testReplayNodeSpecification{{1, testConsumer, claimedRootsNamespace, ""}},
			expectedError:   ErrNoCoveringReplayNode,
		},
		{
			name:            "guardianGovernance node of the claimed namespace",
			lookupNamespace: testNamespace,
			nodes:           []testReplayNodeSpecification{{1, testGuardianGovernance, testNamespace, ""}},
			expectedError:   ErrNoCoveringReplayNode,
		},
		{
			name:            "node of another pair digest",
			lookupNamespace: testNamespace,
			nodes:           []testReplayNodeSpecification{{1, testGuardianGovernance, claimedRootsNamespace, otherNibble}},
			expectedError:   ErrNoCoveringReplayNode,
		},
		{
			name:            "two covering nodes",
			lookupNamespace: testNamespace,
			nodes: []testReplayNodeSpecification{
				{1, testGuardianGovernance, claimedRootsNamespace, ""},
				{2, testGuardianGovernance, claimedRootsNamespace, pairDigestHexText[:2]},
			},
			expectedError: ErrMultipleCoveringReplayNodes,
		},
		{
			name:            "reserved namespace",
			lookupNamespace: claimedRootsNamespace,
			nodes:           []testReplayNodeSpecification{{1, testGuardianGovernance, claimedRootsNamespace, ""}},
			expectedError:   ErrReservedReplayNamespace,
		},
	}
	for _, testCase := range testCases {
		t.Run(testCase.name, func(t *testing.T) {
			nodes := make([]IndexedCoreContract, 0, len(testCase.nodes))
			for _, specification := range testCase.nodes {
				nodes = append(nodes, newTestReplayNodeContract(t, specification.serial, specification.consumerText, specification.namespaceText, specification.prefixNibbles, testReplayNodeBlobBytes))
			}
			index := newBootstrappedTestIndex(t, testLargeIndexByteCap, nodes...)
			result, err := index.LookupClaimedRootsNode(consumer, testReplayNamespace(t, testCase.lookupNamespace))
			if testCase.expectedError != nil {
				require.ErrorIs(t, err, testCase.expectedError)
				require.Equal(t, CoreContractLookupResult{}, result)
				return
			}
			require.NoError(t, err)
			require.Equal(t, testContractID(t, testCase.expectedSerial), result.Contract().ContractID())
			require.Equal(t, testBootstrapOffset, result.LiveAtIndexOffset())
		})
	}
}

func TestConvertedUpdatesRequireTheTransactionOffset(t *testing.T) {
	index := newBootstrappedTestIndex(t, testLargeIndexByteCap)
	err := index.ApplyTransaction(11, []cantonclient.CoreContractUpdate{{}})
	require.Error(t, err)
	require.Equal(t, testBootstrapOffset, index.Status().Offset)
	require.Equal(t, IndexReadinessReady, index.Status().Readiness)
}

func TestConvertedSnapshotRequiresTheIndexGuardianGovernance(t *testing.T) {
	index := newTestIndex(t, testLargeIndexByteCap)
	err := index.ApplyBootstrapSnapshot(testBootstrapOffset, []cantonclient.CreatedCoreContract{{}})
	require.Error(t, err)
	require.Equal(t, IndexReadinessBootstrapping, index.Status().Readiness)
}

func TestIndexedCoreContractConstructionRules(t *testing.T) {
	validLocation := replayNodeLocation{consumer: testPartyID(t, testConsumer), namespace: testReplayNamespace(t, testNamespace)}
	testCases := []struct {
		name        string
		contractID  cantonclient.ContractID
		packageID   cantonclient.PackageID
		template    cantonclient.CoreContractTemplate
		location    replayNodeLocation
		blob        []byte
		expectError bool
	}{
		{name: "singleton", contractID: testContractID(t, 1), packageID: testPackageID(t), template: cantonclient.CoreContractTemplateCoreState, blob: []byte{1}},
		{name: "replay node", contractID: testContractID(t, 1), packageID: testPackageID(t), template: cantonclient.CoreContractTemplateReplayNode, location: validLocation, blob: []byte{1}},
		{name: "empty blob", contractID: testContractID(t, 1), packageID: testPackageID(t), template: cantonclient.CoreContractTemplateCoreState, blob: nil, expectError: true},
		{name: "zero contract id", packageID: testPackageID(t), template: cantonclient.CoreContractTemplateCoreState, blob: []byte{1}, expectError: true},
		{name: "zero package id", contractID: testContractID(t, 1), template: cantonclient.CoreContractTemplateCoreState, blob: []byte{1}, expectError: true},
		{name: "unknown template", contractID: testContractID(t, 1), packageID: testPackageID(t), template: cantonclient.CoreContractTemplate(99), blob: []byte{1}, expectError: true},
		{name: "replay node without location", contractID: testContractID(t, 1), packageID: testPackageID(t), template: cantonclient.CoreContractTemplateReplayNode, blob: []byte{1}, expectError: true},
		{name: "singleton with location", contractID: testContractID(t, 1), packageID: testPackageID(t), template: cantonclient.CoreContractTemplateCoreState, location: validLocation, blob: []byte{1}, expectError: true},
	}
	for _, testCase := range testCases {
		t.Run(testCase.name, func(t *testing.T) {
			contract, err := newIndexedCoreContract(testCase.contractID, testCase.packageID, testCase.template, testSynchronizerID(t), testCase.location, testCase.blob)
			if testCase.expectError {
				require.Error(t, err)
				return
			}
			require.NoError(t, err)
			require.Equal(t, testPackageID(t).String()+":"+testCase.template.String(), contract.TemplateID())
			require.Equal(t, int64(len(testCase.blob))+indexEntryOverheadBytes, contract.indexEntryByteCount())
		})
	}
}

func TestConcurrentLookupsDuringUpdates(t *testing.T) {
	const transactionCount = 200
	const readerCount = 8
	root := newTestReplayNodeContract(t, 1, testConsumer, testNamespace, "", testReplayNodeBlobBytes)
	index := newBootstrappedTestIndex(t, testLargeIndexByteCap, root)
	consumer := testPartyID(t, testConsumer)
	namespace := testReplayNamespace(t, testNamespace)
	digest := testReplayDigest(t, testCoveredDigestHexText)

	nodes := make([]IndexedCoreContract, 0, transactionCount+1)
	nodes = append(nodes, root)
	for transactionIndex := 1; transactionIndex <= transactionCount; transactionIndex++ {
		nodes = append(nodes, newTestReplayNodeContract(t, 1+transactionIndex, testConsumer, testNamespace, "", testReplayNodeBlobBytes))
	}

	var readersDone sync.WaitGroup
	stopReaders := make(chan struct{})
	readerFailures := make(chan string, readerCount)
	for readerIndex := 0; readerIndex < readerCount; readerIndex++ {
		readersDone.Add(1)
		go func() {
			defer readersDone.Done()
			for {
				select {
				case <-stopReaders:
					return
				default:
				}
				result, err := index.LookupCoveringReplayNode(consumer, namespace, digest)
				if err != nil {
					readerFailures <- err.Error()
					return
				}
				expectedNode := nodes[result.LiveAtIndexOffset()-testBootstrapOffset]
				if result.Contract().ContractID() != expectedNode.ContractID() {
					readerFailures <- "lookup served a contract that is not live at the reported offset"
					return
				}
			}
		}()
	}
	for transactionIndex := 1; transactionIndex <= transactionCount; transactionIndex++ {
		replaceUpdates := []coreContractIndexUpdate{archivedIndexUpdate(nodes[transactionIndex-1]), createdIndexUpdate(nodes[transactionIndex])}
		require.NoError(t, index.applyTransactionUpdates(testBootstrapOffset+int64(transactionIndex), replaceUpdates))
	}
	close(stopReaders)
	readersDone.Wait()
	close(readerFailures)
	for failure := range readerFailures {
		t.Fatal(failure)
	}
	require.Equal(t, 1, index.Status().LiveReplayNodeCount)
}
