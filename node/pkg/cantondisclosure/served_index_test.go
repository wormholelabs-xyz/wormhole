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
