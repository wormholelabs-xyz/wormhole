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

// swapTestIndex records a hosted read party and serves index with a bootstrap ledger end read at its offset and the clock time.
func swapTestIndex(served *ServedIndex, index *CoreContractIndex, clock *testClock) {
	served.setReadPartyHosted(true)
	served.swapIndex(index, observedLedgerEnd{offset: index.Status().Offset, observedAt: clock.current}, served.currentReadPartyRehostCount())
}

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
				swapTestIndex(served, newBootstrappedTestIndex(t, testLargeIndexByteCap), clock)
				served.recordLedgerEnd(testBootstrapOffset, clock.current)
			},
			expectedStatus: ServiceReadinessStatusStreamDisconnected,
		},
		{
			name: "ready at the ledger end",
			prepare: func(t *testing.T, served *ServedIndex, clock *testClock) {
				swapTestIndex(served, newBootstrappedTestIndex(t, testLargeIndexByteCap), clock)
				served.setStreamConnected(true)
				served.recordLedgerEnd(testBootstrapOffset, clock.current)
				clock.current = clock.current.Add(testLedgerLagBound)
			},
			expectedStatus: ServiceReadinessStatusReady,
			expectReady:    true,
		},
		{
			name: "read party not hosted",
			prepare: func(t *testing.T, served *ServedIndex, clock *testClock) {
				swapTestIndex(served, newBootstrappedTestIndex(t, testLargeIndexByteCap), clock)
				served.setStreamConnected(true)
				served.recordLedgerEnd(testBootstrapOffset, clock.current)
				served.setReadPartyHosted(false)
			},
			expectedStatus: ServiceReadinessStatusReadPartyNotHosted,
		},
		{
			name: "read party hosted again",
			prepare: func(t *testing.T, served *ServedIndex, clock *testClock) {
				swapTestIndex(served, newBootstrappedTestIndex(t, testLargeIndexByteCap), clock)
				served.setStreamConnected(true)
				served.recordLedgerEnd(testBootstrapOffset, clock.current)
				served.setReadPartyHosted(false)
				served.setReadPartyHosted(true)
			},
			expectedStatus: ServiceReadinessStatusReadPartyRehosted,
		},
		{
			name: "replacement read before the re-hosting",
			prepare: func(t *testing.T, served *ServedIndex, clock *testClock) {
				swapTestIndex(served, newBootstrappedTestIndex(t, testLargeIndexByteCap), clock)
				served.setStreamConnected(true)
				rehostCountBeforeTheReplacementSnapshot := served.currentReadPartyRehostCount()
				served.setReadPartyHosted(false)
				served.setReadPartyHosted(true)
				replacement := newBootstrappedTestIndex(t, testLargeIndexByteCap)
				served.swapIndex(replacement, observedLedgerEnd{offset: testBootstrapOffset, observedAt: clock.current}, rehostCountBeforeTheReplacementSnapshot)
			},
			expectedStatus: ServiceReadinessStatusReadPartyRehosted,
		},
		{
			name: "replacement read after the re-hosting",
			prepare: func(t *testing.T, served *ServedIndex, clock *testClock) {
				swapTestIndex(served, newBootstrappedTestIndex(t, testLargeIndexByteCap), clock)
				served.setStreamConnected(true)
				served.setReadPartyHosted(false)
				served.setReadPartyHosted(true)
				swapTestIndex(served, newBootstrappedTestIndex(t, testLargeIndexByteCap), clock)
			},
			expectedStatus: ServiceReadinessStatusReady,
			expectReady:    true,
		},
		{
			name: "hosting read at the bound",
			prepare: func(t *testing.T, served *ServedIndex, clock *testClock) {
				swapTestIndex(served, newBootstrappedTestIndex(t, testLargeIndexByteCap), clock)
				served.setStreamConnected(true)
				clock.current = clock.current.Add(testLedgerLagBound)
				served.recordLedgerEnd(testBootstrapOffset, clock.current)
			},
			expectedStatus: ServiceReadinessStatusReady,
			expectReady:    true,
		},
		{
			name: "hosting read older than the bound",
			prepare: func(t *testing.T, served *ServedIndex, clock *testClock) {
				swapTestIndex(served, newBootstrappedTestIndex(t, testLargeIndexByteCap), clock)
				served.setStreamConnected(true)
				clock.current = clock.current.Add(testLedgerLagBound + time.Nanosecond)
				served.recordLedgerEnd(testBootstrapOffset, clock.current)
			},
			expectedStatus: ServiceReadinessStatusReadPartyHostingUnknown,
		},
		{
			name: "no hosting read before the swap",
			prepare: func(t *testing.T, served *ServedIndex, clock *testClock) {
				served.swapIndex(newBootstrappedTestIndex(t, testLargeIndexByteCap), observedLedgerEnd{offset: testBootstrapOffset, observedAt: clock.current}, 0)
				served.setStreamConnected(true)
			},
			expectedStatus: ServiceReadinessStatusReadPartyHostingUnknown,
		},
		{
			name: "read party not hosted before the bootstrap",
			prepare: func(t *testing.T, served *ServedIndex, clock *testClock) {
				served.setStreamConnected(true)
				served.setReadPartyHosted(false)
			},
			expectedStatus: ServiceReadinessStatusBootstrapping,
		},
		{
			name: "read party not hosted at the byte cap",
			prepare: func(t *testing.T, served *ServedIndex, clock *testClock) {
				const byteCapBytes = int64(20_000)
				swapTestIndex(served, newBootstrappedTestIndex(t, byteCapBytes, newTestReplayNodeContract(t, 1, testConsumer, testNamespace, "", int(byteCapBytes))), clock)
				served.setStreamConnected(true)
				served.recordLedgerEnd(testBootstrapOffset, clock.current)
				served.setReadPartyHosted(false)
			},
			expectedStatus: ServiceReadinessStatusReadPartyNotHosted,
		},
		{
			name: "no ledger end read since a bootstrap older than the bound",
			prepare: func(t *testing.T, served *ServedIndex, clock *testClock) {
				swapTestIndex(served, newBootstrappedTestIndex(t, testLargeIndexByteCap), clock)
				served.setStreamConnected(true)
				clock.current = clock.current.Add(testLedgerLagBound + time.Nanosecond)
				served.setReadPartyHosted(true)
			},
			expectedStatus: ServiceReadinessStatusLedgerLagAboveBound,
		},
		{
			name: "latest ledger end read is older than the bound",
			prepare: func(t *testing.T, served *ServedIndex, clock *testClock) {
				swapTestIndex(served, newBootstrappedTestIndex(t, testLargeIndexByteCap), clock)
				served.setStreamConnected(true)
				served.recordLedgerEnd(testBootstrapOffset, clock.current)
				clock.current = clock.current.Add(testLedgerLagBound + time.Nanosecond)
				served.setReadPartyHosted(true)
			},
			expectedStatus: ServiceReadinessStatusLedgerLagAboveBound,
		},
		{
			name: "unreached ledger end within the bound",
			prepare: func(t *testing.T, served *ServedIndex, clock *testClock) {
				swapTestIndex(served, newBootstrappedTestIndex(t, testLargeIndexByteCap), clock)
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
				swapTestIndex(served, newBootstrappedTestIndex(t, testLargeIndexByteCap), clock)
				served.setStreamConnected(true)
				served.recordLedgerEnd(testBootstrapOffset+5, clock.current)
				clock.current = clock.current.Add(testLedgerLagBound)
				served.recordLedgerEnd(testBootstrapOffset+9, clock.current)
				clock.current = clock.current.Add(time.Second)
				served.setReadPartyHosted(true)
			},
			expectedStatus: ServiceReadinessStatusLedgerLagAboveBound,
		},
		{
			name: "index reaches an old ledger end read but not a newer one",
			prepare: func(t *testing.T, served *ServedIndex, clock *testClock) {
				index := newBootstrappedTestIndex(t, testLargeIndexByteCap)
				swapTestIndex(served, index, clock)
				served.setStreamConnected(true)
				served.recordLedgerEnd(testBootstrapOffset+5, clock.current)
				clock.current = clock.current.Add(testLedgerLagBound)
				served.recordLedgerEnd(testBootstrapOffset+9, clock.current)
				require.NoError(t, index.AdvanceOffset(testBootstrapOffset+5))
				clock.current = clock.current.Add(time.Second)
				served.setReadPartyHosted(true)
			},
			expectedStatus: ServiceReadinessStatusLedgerLagAboveBound,
		},
		{
			name: "index reaches the newest ledger end read",
			prepare: func(t *testing.T, served *ServedIndex, clock *testClock) {
				index := newBootstrappedTestIndex(t, testLargeIndexByteCap)
				swapTestIndex(served, index, clock)
				served.setStreamConnected(true)
				served.recordLedgerEnd(testBootstrapOffset+5, clock.current)
				clock.current = clock.current.Add(testLedgerLagBound)
				served.recordLedgerEnd(testBootstrapOffset+9, clock.current)
				require.NoError(t, index.AdvanceOffset(testBootstrapOffset+9))
				clock.current = clock.current.Add(time.Second)
				served.setReadPartyHosted(true)
			},
			expectedStatus: ServiceReadinessStatusReady,
			expectReady:    true,
		},
		{
			name: "singleton missing",
			prepare: func(t *testing.T, served *ServedIndex, clock *testClock) {
				index := newTestIndex(t, testLargeIndexByteCap)
				require.NoError(t, index.applyBootstrapSnapshot(testBootstrapOffset, nil, false))
				swapTestIndex(served, index, clock)
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
				swapTestIndex(served, index, clock)
				served.setStreamConnected(true)
				served.recordLedgerEnd(testBootstrapOffset, clock.current)
			},
			expectedStatus: "index_inconsistent",
		},
		{
			name: "byte cap reached",
			prepare: func(t *testing.T, served *ServedIndex, clock *testClock) {
				const byteCapBytes = int64(20_000)
				swapTestIndex(served, newBootstrappedTestIndex(t, byteCapBytes, newTestReplayNodeContract(t, 1, testConsumer, testNamespace, "", int(byteCapBytes))), clock)
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
			require.Contains(t, allServiceReadinessStatuses(), readiness.Status())
		})
	}
}

func TestServedIndexLookupsFailWhileTheHostingRefusesThem(t *testing.T) {
	testCases := []struct {
		name           string
		prepare        func(served *ServedIndex, clock *testClock)
		expectedStatus ServiceReadinessStatus
	}{
		{name: "not hosted", prepare: func(served *ServedIndex, clock *testClock) { served.setReadPartyHosted(false) }, expectedStatus: ServiceReadinessStatusReadPartyNotHosted},
		{name: "hosted again", prepare: func(served *ServedIndex, clock *testClock) {
			served.setReadPartyHosted(false)
			served.setReadPartyHosted(true)
		}, expectedStatus: ServiceReadinessStatusReadPartyRehosted},
		{name: "hosting read older than the bound", prepare: func(served *ServedIndex, clock *testClock) {
			clock.current = clock.current.Add(testLedgerLagBound + time.Nanosecond)
		}, expectedStatus: ServiceReadinessStatusReadPartyHostingUnknown},
	}
	for _, testCase := range testCases {
		t.Run(testCase.name, func(t *testing.T) {
			served, clock := newTestServedIndex(t)
			node := newTestReplayNodeContract(t, 1, testConsumer, testNamespace, "", testReplayNodeBlobBytes)
			swapTestIndex(served, newBootstrappedTestIndex(t, testLargeIndexByteCap, node), clock)
			_, err := served.LookupSingleton(cantonclient.CoreContractTemplateCoreState)
			require.NoError(t, err)

			testCase.prepare(served, clock)
			expectedErr := ReadPartyHostingNotReadyError{Status: testCase.expectedStatus}
			_, err = served.LookupSingleton(cantonclient.CoreContractTemplateCoreState)
			require.Equal(t, expectedErr, err)
			require.ErrorIs(t, err, ErrIndexNotReady)
			_, err = served.LookupCoveringReplayNode(testPartyID(t, testConsumer), testReplayNamespace(t, testNamespace), testReplayDigest(t, testCoveredDigestHexText))
			require.Equal(t, expectedErr, err)
			_, err = served.LookupClaimedRootsNode(testPartyID(t, testConsumer), testReplayNamespace(t, testNamespace))
			require.Equal(t, expectedErr, err)
			require.Equal(t, testCase.expectedStatus, served.ServiceReadiness().Status())
		})
	}
}

func TestReadPartyHostingTransitions(t *testing.T) {
	served, clock := newTestServedIndex(t)
	require.Equal(t, readPartyHostingTransitionUnchanged, served.setReadPartyHosted(false), "the zero value is not hosted")
	require.Equal(t, readPartyHostingTransitionRegained, served.setReadPartyHosted(true))
	require.Zero(t, served.currentReadPartyRehostCount(), "a hosting change before the first swap needs no replacement")
	swapTestIndex(served, newBootstrappedTestIndex(t, testLargeIndexByteCap), clock)
	require.Equal(t, readPartyHostingTransitionUnchanged, served.setReadPartyHosted(true))
	require.Equal(t, readPartyHostingTransitionLost, served.setReadPartyHosted(false))
	require.Equal(t, readPartyHostingTransitionUnchanged, served.setReadPartyHosted(false))
	require.False(t, served.readPartyRehostReplacementDue(), "an unhosted read party needs no replacement yet")
	require.Equal(t, readPartyHostingTransitionRegained, served.setReadPartyHosted(true))
	require.Equal(t, uint64(1), served.currentReadPartyRehostCount())
	require.True(t, served.readPartyRehostReplacementDue())
	clock.current = clock.current.Add(testLedgerLagBound + time.Nanosecond)
	require.False(t, served.readPartyRehostReplacementDue(), "an unknown hosting needs a successful read first")

	clock.current = clock.current.Add(-(testLedgerLagBound + 2*time.Nanosecond))
	require.Panics(t, func() { served.setReadPartyHosted(true) }, "hosting reads go back in time")
	require.Panics(t, func() {
		served.swapIndex(newBootstrappedTestIndex(t, testLargeIndexByteCap), observedLedgerEnd{offset: testBootstrapOffset}, 2)
	}, "rehost count above the current count")
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
	swapTestIndex(served, first, clock)
	served.recordLedgerEnd(testBootstrapOffset+7, clock.current)
	require.Equal(t, int64(7), served.LedgerOffsetsBehind())

	replacement := newTestIndex(t, testLargeIndexByteCap)
	node := newTestReplayNodeContract(t, 1, testConsumer, testNamespace, "", testReplayNodeBlobBytes)
	require.NoError(t, replacement.applyBootstrapSnapshot(testBootstrapOffset+7, append(testGenesisSingletons(t), node), false))
	swapTestIndex(served, replacement, clock)
	result, err := served.LookupCoveringReplayNode(testPartyID(t, testConsumer), testReplayNamespace(t, testNamespace), testReplayDigest(t, testCoveredDigestHexText))
	require.NoError(t, err)
	require.Equal(t, node.ContractID(), result.Contract().ContractID())
	require.Equal(t, int64(0), served.LedgerOffsetsBehind())

	require.Panics(t, func() { swapTestIndex(served, newTestIndex(t, testLargeIndexByteCap), clock) })
	require.Panics(t, func() { served.swapIndex(nil, observedLedgerEnd{}, 0) })
	require.Panics(t, func() { swapTestIndex(served, newBootstrappedTestIndex(t, testLargeIndexByteCap), clock) }, "offset below the served offset")
	aboveTheBootstrap := newBootstrappedTestIndex(t, testLargeIndexByteCap)
	require.NoError(t, aboveTheBootstrap.AdvanceOffset(testBootstrapOffset+7))
	require.Panics(t, func() {
		served.swapIndex(aboveTheBootstrap, observedLedgerEnd{offset: testBootstrapOffset + 8, observedAt: clock.current}, 0)
	}, "bootstrap ledger end above the index offset")
}

// simulateConstantLedgerLag steps the clock by one second for runDuration. Every ProductionLedgerEndPollInterval
// the ledger end grows by one and the writer reads it; the index reaches each read ledger end constantLag after
// the read. It returns the elapsed time and the readiness after each step.
func simulateConstantLedgerLag(t *testing.T, served *ServedIndex, clock *testClock, index *CoreContractIndex, constantLag time.Duration, runDuration time.Duration) ([]time.Duration, []ServiceReadiness) {
	t.Helper()
	startedAt := clock.current
	elapsedTimes := []time.Duration{}
	readinessAfterEachStep := []ServiceReadiness{}
	readCount := 0
	reachedCount := 0
	for elapsed := time.Second; elapsed <= runDuration; elapsed += time.Second {
		clock.current = startedAt.Add(elapsed)
		for reachedCount < readCount && time.Duration(reachedCount+1)*ProductionLedgerEndPollInterval+constantLag <= elapsed {
			reachedCount++
			require.NoError(t, index.AdvanceOffset(testBootstrapOffset+int64(reachedCount)))
		}
		if elapsed%ProductionLedgerEndPollInterval == 0 {
			readCount++
			served.recordLedgerEnd(testBootstrapOffset+int64(readCount), clock.current)
			served.setReadPartyHosted(true)
		}
		elapsedTimes = append(elapsedTimes, elapsed)
		readinessAfterEachStep = append(readinessAfterEachStep, served.ServiceReadiness())
	}
	return elapsedTimes, readinessAfterEachStep
}

func TestLedgerLagUnderAConstantLag(t *testing.T) {
	testCases := []struct {
		name        string
		constantLag time.Duration
		expectReady func(ledgerLag time.Duration) bool
	}{
		{name: "lag at the bound", constantLag: testLedgerLagBound, expectReady: func(ledgerLag time.Duration) bool { return ledgerLag == testLedgerLagBound }},
		{name: "lag one second above the bound", constantLag: testLedgerLagBound + time.Second, expectReady: func(time.Duration) bool { return false }},
		{name: "lag one poll interval above the bound", constantLag: testLedgerLagBound + ProductionLedgerEndPollInterval, expectReady: func(time.Duration) bool { return false }},
		{name: "lag far above the bound", constantLag: 90 * time.Second, expectReady: func(time.Duration) bool { return false }},
		{name: "lag below the bound by more than one poll interval", constantLag: 30 * time.Second, expectReady: func(time.Duration) bool { return true }},
	}
	for _, testCase := range testCases {
		t.Run(testCase.name, func(t *testing.T) {
			served, clock := newTestServedIndex(t)
			index := newBootstrappedTestIndex(t, testLargeIndexByteCap)
			swapTestIndex(served, index, clock)
			served.setStreamConnected(true)
			elapsedTimes, readinessAfterEachStep := simulateConstantLedgerLag(t, served, clock, index, testCase.constantLag, 400*time.Second)
			checkedStepCount := 0
			readyStepCount := 0
			for stepIndex, readiness := range readinessAfterEachStep {
				elapsed := elapsedTimes[stepIndex]
				if elapsed < testCase.constantLag+ProductionLedgerEndPollInterval {
					continue
				}
				checkedStepCount++
				require.GreaterOrEqual(t, readiness.LedgerLag, testCase.constantLag, "after %s", elapsed)
				require.Less(t, readiness.LedgerLag, testCase.constantLag+ProductionLedgerEndPollInterval, "after %s", elapsed)
				require.Equal(t, testCase.expectReady(readiness.LedgerLag), readiness.IsReady(), "after %s with lag %s", elapsed, readiness.LedgerLag)
				if readiness.IsReady() {
					readyStepCount++
				}
			}
			require.Greater(t, checkedStepCount, 200)
			if testCase.constantLag > testLedgerLagBound {
				require.Zero(t, readyStepCount)
			}
			if testCase.constantLag == testLedgerLagBound {
				require.Greater(t, readyStepCount, 0)
				require.Less(t, readyStepCount, checkedStepCount)
			}
		})
	}
}

func TestLedgerLagRecoversWhenTheIndexCatchesUp(t *testing.T) {
	served, clock := newTestServedIndex(t)
	index := newBootstrappedTestIndex(t, testLargeIndexByteCap)
	swapTestIndex(served, index, clock)
	served.setStreamConnected(true)
	_, readinessAfterEachStep := simulateConstantLedgerLag(t, served, clock, index, 90*time.Second, 200*time.Second)
	require.Equal(t, ServiceReadinessStatusLedgerLagAboveBound, readinessAfterEachStep[len(readinessAfterEachStep)-1].Status())

	latestLedgerEnd := testBootstrapOffset + int64(200*time.Second/ProductionLedgerEndPollInterval)
	require.NoError(t, index.AdvanceOffset(latestLedgerEnd))
	readiness := served.ServiceReadiness()
	require.Equal(t, ServiceReadinessStatusReady, readiness.Status())
	require.Equal(t, time.Duration(0), readiness.LedgerLag)
	require.Empty(t, served.unreachedLedgerEnds)
}

func TestLedgerEndObservationQueueFullMergesIntoTheNewestEntry(t *testing.T) {
	served, clock := newTestServedIndex(t)
	swapTestIndex(served, newBootstrappedTestIndex(t, testLargeIndexByteCap), clock)
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
	swapTestIndex(served, newBootstrappedTestIndex(t, testLargeIndexByteCap), clock)
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
	require.Error(t, CheckLedgerLagBound(2*ProductionLedgerEndPollInterval-time.Nanosecond))
	require.NoError(t, CheckLedgerLagBound(2*ProductionLedgerEndPollInterval))
}

// Idle offset checkpoint cadence that the LocalNet suite measured on a Canton 3.7.3 participant: the filtered update
// stream of guardianGovernance sends a checkpoint about every 45 s, and each checkpoint reaches the ledger end of
// about 30 s before. Other parties advance the ledger end meanwhile. Measured largest lag: 74.6 s.
const (
	measuredIdleCheckpointInterval = 45 * time.Second
	measuredCheckpointOffsetDelay  = 30 * time.Second
)

// simulateIdleCheckpointCadence advances the ledger end each second, reads it each poll interval, and advances the index
// to the ledger end of measuredCheckpointOffsetDelay before at each measuredIdleCheckpointInterval. Returns the
// readiness after each poll.
func simulateIdleCheckpointCadence(t *testing.T, ledgerLagBound time.Duration, runDuration time.Duration) []ServiceReadiness {
	t.Helper()
	clock := &testClock{current: time.Date(2026, 11, 24, 12, 0, 0, 0, time.UTC)}
	served, err := newServedIndexWithClock(ledgerLagBound, clock.now)
	require.NoError(t, err)
	index := newBootstrappedTestIndex(t, testLargeIndexByteCap)
	swapTestIndex(served, index, clock)
	served.setStreamConnected(true)
	startedAt := clock.current
	readinessAfterEachPoll := []ServiceReadiness{}
	for elapsed := time.Second; elapsed <= runDuration; elapsed += time.Second {
		clock.current = startedAt.Add(elapsed)
		ledgerEnd := testBootstrapOffset + int64(elapsed/time.Second)
		if elapsed%measuredIdleCheckpointInterval == 0 && elapsed > measuredCheckpointOffsetDelay {
			require.NoError(t, index.AdvanceOffset(testBootstrapOffset+int64((elapsed-measuredCheckpointOffsetDelay)/time.Second)))
		}
		if elapsed%ProductionLedgerEndPollInterval == 0 {
			served.recordLedgerEnd(ledgerEnd, clock.current)
			served.setReadPartyHosted(true)
			readinessAfterEachPoll = append(readinessAfterEachPoll, served.ServiceReadiness())
		}
	}
	return readinessAfterEachPoll
}

func TestDefaultLedgerLagBoundAdmitsTheIdleCheckpointCadence(t *testing.T) {
	testCases := []struct {
		name              string
		ledgerLagBound    time.Duration
		expectAlwaysReady bool
	}{
		{name: "default bound", ledgerLagBound: DefaultLedgerLagBound, expectAlwaysReady: true},
		{name: "one minute bound", ledgerLagBound: time.Minute, expectAlwaysReady: false},
	}
	for _, testCase := range testCases {
		t.Run(testCase.name, func(t *testing.T) {
			readinessAfterEachPoll := simulateIdleCheckpointCadence(t, testCase.ledgerLagBound, 30*time.Minute)
			largestLag := time.Duration(0)
			notReadyPolls := 0
			for _, readiness := range readinessAfterEachPoll {
				largestLag = max(largestLag, readiness.LedgerLag)
				if !readiness.IsReady() {
					notReadyPolls++
				}
			}
			require.Greater(t, largestLag, time.Minute, "the simulated cadence reproduces the measured lag above one minute")
			require.Equal(t, testCase.expectAlwaysReady, notReadyPolls == 0, "%d of %d polls not ready, largest lag %s", notReadyPolls, len(readinessAfterEachPoll), largestLag)
		})
	}
}
