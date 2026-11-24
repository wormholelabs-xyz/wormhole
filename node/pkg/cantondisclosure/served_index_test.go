package cantondisclosure

import (
	"testing"
	"time"

	"github.com/stretchr/testify/require"

	"github.com/certusone/wormhole/node/pkg/cantonclient"
)

const testLedgerLagBound = time.Minute

type testClock struct {
	current time.Time
}

func (clock *testClock) now() time.Time { return clock.current }

func newTestServedIndex(t *testing.T) (*ServedIndex, *testClock) {
	t.Helper()
	clock := &testClock{current: time.Date(2026, 11, 24, 12, 0, 0, 0, time.UTC)}
	served, err := newServedIndexWithClock(testLedgerLagBound, clock.now)
	require.NoError(t, err)
	return served, clock
}

func TestNewServedIndexLedgerLagBound(t *testing.T) {
	testCases := []struct {
		name           string
		ledgerLagBound time.Duration
		expectError    bool
	}{
		{name: "minimum", ledgerLagBound: minLedgerLagBound},
		{name: "maximum", ledgerLagBound: maxLedgerLagBound},
		{name: "below minimum", ledgerLagBound: minLedgerLagBound - time.Nanosecond, expectError: true},
		{name: "above maximum", ledgerLagBound: maxLedgerLagBound + time.Nanosecond, expectError: true},
		{name: "zero", ledgerLagBound: 0, expectError: true},
	}
	for _, testCase := range testCases {
		t.Run(testCase.name, func(t *testing.T) {
			served, err := NewServedIndex(testCase.ledgerLagBound)
			if testCase.expectError {
				require.Error(t, err)
				require.Nil(t, served)
				return
			}
			require.NoError(t, err)
			require.Equal(t, testCase.ledgerLagBound, served.ServiceReadiness().LedgerLagBound)
		})
	}
}

func TestServiceReadinessStates(t *testing.T) {
	testCases := []struct {
		name           string
		prepare        func(t *testing.T, served *ServedIndex, clock *testClock)
		expectedStatus ServiceReadinessStatus
		expectReady    bool
	}{
		{
			name:           "before the bootstrap",
			prepare:        func(t *testing.T, served *ServedIndex, clock *testClock) { served.setStreamConnected(true) },
			expectedStatus: ServiceReadinessStatusBootstrapping,
		},
		{
			name: "stream disconnected",
			prepare: func(t *testing.T, served *ServedIndex, clock *testClock) {
				served.swapIndex(newBootstrappedTestIndex(t, testLargeIndexByteCap))
				served.recordLedgerEnd(testBootstrapOffset, clock.current)
			},
			expectedStatus: ServiceReadinessStatusStreamDisconnected,
		},
		{
			name: "ready at the ledger end",
			prepare: func(t *testing.T, served *ServedIndex, clock *testClock) {
				served.swapIndex(newBootstrappedTestIndex(t, testLargeIndexByteCap))
				served.setStreamConnected(true)
				served.recordLedgerEnd(testBootstrapOffset, clock.current)
				clock.current = clock.current.Add(testLedgerLagBound)
			},
			expectedStatus: ServiceReadinessStatusReady,
			expectReady:    true,
		},
		{
			name: "no ledger end read",
			prepare: func(t *testing.T, served *ServedIndex, clock *testClock) {
				served.swapIndex(newBootstrappedTestIndex(t, testLargeIndexByteCap))
				served.setStreamConnected(true)
			},
			expectedStatus: ServiceReadinessStatusLedgerLagAboveBound,
		},
		{
			name: "latest ledger end read is older than the bound",
			prepare: func(t *testing.T, served *ServedIndex, clock *testClock) {
				served.swapIndex(newBootstrappedTestIndex(t, testLargeIndexByteCap))
				served.setStreamConnected(true)
				served.recordLedgerEnd(testBootstrapOffset, clock.current)
				clock.current = clock.current.Add(testLedgerLagBound + time.Nanosecond)
			},
			expectedStatus: ServiceReadinessStatusLedgerLagAboveBound,
		},
		{
			name: "unreached ledger end within the bound",
			prepare: func(t *testing.T, served *ServedIndex, clock *testClock) {
				served.swapIndex(newBootstrappedTestIndex(t, testLargeIndexByteCap))
				served.setStreamConnected(true)
				served.recordLedgerEnd(testBootstrapOffset+5, clock.current)
				clock.current = clock.current.Add(testLedgerLagBound / 2)
				served.recordLedgerEnd(testBootstrapOffset+9, clock.current)
				clock.current = clock.current.Add(testLedgerLagBound / 2)
			},
			expectedStatus: ServiceReadinessStatusReady,
			expectReady:    true,
		},
		{
			name: "unreached ledger end above the bound while newer reads continue",
			prepare: func(t *testing.T, served *ServedIndex, clock *testClock) {
				served.swapIndex(newBootstrappedTestIndex(t, testLargeIndexByteCap))
				served.setStreamConnected(true)
				served.recordLedgerEnd(testBootstrapOffset+5, clock.current)
				clock.current = clock.current.Add(testLedgerLagBound)
				served.recordLedgerEnd(testBootstrapOffset+9, clock.current)
				clock.current = clock.current.Add(time.Second)
			},
			expectedStatus: ServiceReadinessStatusLedgerLagAboveBound,
		},
		{
			name: "index reaches the unreached ledger end",
			prepare: func(t *testing.T, served *ServedIndex, clock *testClock) {
				index := newBootstrappedTestIndex(t, testLargeIndexByteCap)
				served.swapIndex(index)
				served.setStreamConnected(true)
				served.recordLedgerEnd(testBootstrapOffset+5, clock.current)
				clock.current = clock.current.Add(testLedgerLagBound)
				served.recordLedgerEnd(testBootstrapOffset+9, clock.current)
				require.NoError(t, index.AdvanceOffset(testBootstrapOffset+5))
				clock.current = clock.current.Add(time.Second)
			},
			expectedStatus: ServiceReadinessStatusReady,
			expectReady:    true,
		},
		{
			name: "singleton missing",
			prepare: func(t *testing.T, served *ServedIndex, clock *testClock) {
				index := newTestIndex(t, testLargeIndexByteCap)
				require.NoError(t, index.applyBootstrapSnapshot(testBootstrapOffset, nil, false))
				served.swapIndex(index)
				served.setStreamConnected(true)
				served.recordLedgerEnd(testBootstrapOffset, clock.current)
			},
			expectedStatus: "index_singleton_missing",
		},
		{
			name: "inconsistent index",
			prepare: func(t *testing.T, served *ServedIndex, clock *testClock) {
				node := newTestReplayNodeContract(t, 1, testConsumer, testNamespace, "", testReplayNodeBlobBytes)
				index := newBootstrappedTestIndex(t, testLargeIndexByteCap, node)
				require.Error(t, index.applyTransactionUpdates(testBootstrapOffset+1, []coreContractIndexUpdate{createdIndexUpdate(node)}))
				served.swapIndex(index)
				served.setStreamConnected(true)
				served.recordLedgerEnd(testBootstrapOffset, clock.current)
			},
			expectedStatus: "index_inconsistent",
		},
		{
			name: "byte cap reached",
			prepare: func(t *testing.T, served *ServedIndex, clock *testClock) {
				const byteCapBytes = int64(20_000)
				served.swapIndex(newBootstrappedTestIndex(t, byteCapBytes, newTestReplayNodeContract(t, 1, testConsumer, testNamespace, "", int(byteCapBytes))))
				served.setStreamConnected(true)
				served.recordLedgerEnd(testBootstrapOffset, clock.current)
			},
			expectedStatus: ServiceReadinessStatusReadyByteCapReached,
			expectReady:    true,
		},
	}
	for _, testCase := range testCases {
		t.Run(testCase.name, func(t *testing.T) {
			served, clock := newTestServedIndex(t)
			testCase.prepare(t, served, clock)
			readiness := served.ServiceReadiness()
			require.Equal(t, testCase.expectedStatus, readiness.Status())
			require.Equal(t, testCase.expectReady, readiness.IsReady())
		})
	}
}

func TestServedIndexLookupsBeforeTheBootstrap(t *testing.T) {
	served, _ := newTestServedIndex(t)
	_, err := served.LookupSingleton(cantonclient.CoreContractTemplateCoreState)
	requireIndexNotReady(t, err, IndexReadinessBootstrapping)
	_, err = served.LookupCoveringReplayNode(testPartyID(t, testConsumer), testReplayNamespace(t, testNamespace), testReplayDigest(t, testCoveredDigestHexText))
	requireIndexNotReady(t, err, IndexReadinessBootstrapping)
	_, err = served.LookupClaimedRootsNode(testPartyID(t, testConsumer), testReplayNamespace(t, testNamespace))
	requireIndexNotReady(t, err, IndexReadinessBootstrapping)
	_, hasIndex := served.IndexStatus()
	require.False(t, hasIndex)
	require.Equal(t, int64(0), served.LedgerOffsetsBehind())
}

func TestServedIndexSwapServesTheNewIndex(t *testing.T) {
	served, clock := newTestServedIndex(t)
	first := newBootstrappedTestIndex(t, testLargeIndexByteCap)
	served.swapIndex(first)
	served.recordLedgerEnd(testBootstrapOffset+7, clock.current)
	require.Equal(t, int64(7), served.LedgerOffsetsBehind())

	replacement := newTestIndex(t, testLargeIndexByteCap)
	node := newTestReplayNodeContract(t, 1, testConsumer, testNamespace, "", testReplayNodeBlobBytes)
	require.NoError(t, replacement.applyBootstrapSnapshot(testBootstrapOffset+7, append(testGenesisSingletons(t), node), false))
	served.swapIndex(replacement)
	result, err := served.LookupCoveringReplayNode(testPartyID(t, testConsumer), testReplayNamespace(t, testNamespace), testReplayDigest(t, testCoveredDigestHexText))
	require.NoError(t, err)
	require.Equal(t, node.ContractID(), result.Contract().ContractID())
	require.Equal(t, int64(0), served.LedgerOffsetsBehind())

	require.Panics(t, func() { served.swapIndex(newTestIndex(t, testLargeIndexByteCap)) })
	require.Panics(t, func() { served.swapIndex(nil) })
}

// simulateConstantLedgerLag polls every ProductionLedgerEndPollInterval for runDuration. The ledger end grows by one
// per poll; the index reaches each ledger end constantLag after its poll. It returns the readiness after each poll.
// The reported lag is the age of the oldest unreached read: constantLag minus one poll interval.
func simulateConstantLedgerLag(t *testing.T, served *ServedIndex, clock *testClock, index *CoreContractIndex, constantLag time.Duration, runDuration time.Duration) []ServiceReadiness {
	t.Helper()
	startedAt := clock.current
	readinessAfterEachPoll := []ServiceReadiness{}
	pollCount := int(runDuration / ProductionLedgerEndPollInterval)
	for pollIndex := 1; pollIndex <= pollCount; pollIndex++ {
		clock.current = startedAt.Add(time.Duration(pollIndex) * ProductionLedgerEndPollInterval)
		reachedPollIndex := pollIndex - int(constantLag/ProductionLedgerEndPollInterval)
		if reachedPollIndex >= 1 {
			require.NoError(t, index.AdvanceOffset(testBootstrapOffset+int64(reachedPollIndex)))
		}
		served.recordLedgerEnd(testBootstrapOffset+int64(pollIndex), clock.current)
		readinessAfterEachPoll = append(readinessAfterEachPoll, served.ServiceReadiness())
	}
	return readinessAfterEachPoll
}

func TestLedgerLagUnderAConstantLag(t *testing.T) {
	testCases := []struct {
		name                 string
		constantLag          time.Duration
		expectAboveTheBound  bool
		expectedReportedLags time.Duration
	}{
		{name: "constant lag above the bound", constantLag: 90 * time.Second, expectAboveTheBound: true, expectedReportedLags: 85 * time.Second},
		{name: "constant lag below the bound", constantLag: 30 * time.Second, expectedReportedLags: 25 * time.Second},
	}
	for _, testCase := range testCases {
		t.Run(testCase.name, func(t *testing.T) {
			served, clock := newTestServedIndex(t)
			index := newBootstrappedTestIndex(t, testLargeIndexByteCap)
			served.swapIndex(index)
			served.setStreamConnected(true)
			readinessAfterEachPoll := simulateConstantLedgerLag(t, served, clock, index, testCase.constantLag, 400*time.Second)
			for pollIndex, readiness := range readinessAfterEachPoll {
				elapsed := time.Duration(pollIndex+1) * ProductionLedgerEndPollInterval
				if elapsed < testCase.constantLag {
					continue
				}
				require.Equal(t, testCase.expectedReportedLags, readiness.LedgerLag, "after %s", elapsed)
				require.Equal(t, testCase.expectAboveTheBound, readiness.Status() == ServiceReadinessStatusLedgerLagAboveBound, "after %s", elapsed)
			}
		})
	}
}

func TestLedgerLagRecoversWhenTheIndexCatchesUp(t *testing.T) {
	served, clock := newTestServedIndex(t)
	index := newBootstrappedTestIndex(t, testLargeIndexByteCap)
	served.swapIndex(index)
	served.setStreamConnected(true)
	readinessAfterEachPoll := simulateConstantLedgerLag(t, served, clock, index, 90*time.Second, 200*time.Second)
	require.Equal(t, ServiceReadinessStatusLedgerLagAboveBound, readinessAfterEachPoll[len(readinessAfterEachPoll)-1].Status())

	latestLedgerEnd := testBootstrapOffset + int64(len(readinessAfterEachPoll))
	require.NoError(t, index.AdvanceOffset(latestLedgerEnd))
	readiness := served.ServiceReadiness()
	require.Equal(t, ServiceReadinessStatusReady, readiness.Status())
	require.Equal(t, time.Duration(0), readiness.LedgerLag)
	require.Empty(t, served.unreachedLedgerEnds)
}

func TestLedgerEndObservationQueueFullMergesIntoTheNewestEntry(t *testing.T) {
	served, clock := newTestServedIndex(t)
	served.swapIndex(newBootstrappedTestIndex(t, testLargeIndexByteCap))
	served.setStreamConnected(true)
	firstObservedAt := clock.current
	readCount := ledgerEndObservationCapacity + 50
	for readIndex := 1; readIndex <= readCount; readIndex++ {
		served.recordLedgerEnd(testBootstrapOffset+int64(readIndex), clock.current)
		clock.current = clock.current.Add(ProductionLedgerEndPollInterval)
	}
	require.Len(t, served.unreachedLedgerEnds, ledgerEndObservationCapacity)
	require.Equal(t, firstObservedAt, served.unreachedLedgerEnds[0].observedAt)
	require.Equal(t, testBootstrapOffset+1, served.unreachedLedgerEnds[0].offset)
	newest := served.unreachedLedgerEnds[ledgerEndObservationCapacity-1]
	require.Equal(t, testBootstrapOffset+int64(readCount), newest.offset)
	require.Equal(t, firstObservedAt.Add(time.Duration(ledgerEndObservationCapacity-1)*ProductionLedgerEndPollInterval), newest.observedAt)
	require.Equal(t, clock.current.Sub(firstObservedAt), served.ServiceReadiness().LedgerLag)
}

func TestLedgerEndAtOrBelowTheNewestEntryKeepsItsTime(t *testing.T) {
	served, clock := newTestServedIndex(t)
	served.swapIndex(newBootstrappedTestIndex(t, testLargeIndexByteCap))
	firstObservedAt := clock.current
	served.recordLedgerEnd(testBootstrapOffset+3, clock.current)
	clock.current = clock.current.Add(ProductionLedgerEndPollInterval)
	served.recordLedgerEnd(testBootstrapOffset+3, clock.current)
	clock.current = clock.current.Add(ProductionLedgerEndPollInterval)
	served.recordLedgerEnd(testBootstrapOffset, clock.current)
	require.Len(t, served.unreachedLedgerEnds, 1)
	require.Equal(t, firstObservedAt, served.unreachedLedgerEnds[0].observedAt)
}

func TestCheckLedgerLagBoundAgainstThePollInterval(t *testing.T) {
	require.Greater(t, minLedgerLagBound, ProductionLedgerEndPollInterval)
	require.Equal(t, int(maxLedgerLagBound/ProductionLedgerEndPollInterval)+1, ledgerEndObservationCapacity)
	require.Error(t, CheckLedgerLagBound(ProductionLedgerEndPollInterval))
}
