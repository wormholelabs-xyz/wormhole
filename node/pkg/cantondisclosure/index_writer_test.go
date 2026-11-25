package cantondisclosure

import (
	"context"
	"errors"
	"fmt"
	"sync"
	"sync/atomic"
	"testing"
	"time"

	"github.com/prometheus/client_golang/prometheus/testutil"
	"github.com/stretchr/testify/require"
	"go.uber.org/zap"
	"google.golang.org/grpc/codes"
	"google.golang.org/grpc/status"

	"github.com/certusone/wormhole/node/pkg/cantonclient"
	"github.com/certusone/wormhole/node/pkg/cantonclient/cantonclienttest"
)

type fakeSnapshotResponse struct {
	contracts []cantonclient.CreatedCoreContract
	err       error
}

// fakeSubscriptionScript: subscribeErr fails the subscribe call. Otherwise the stream sends advances,
// then fails with finalErr, or forwards advanceFeed until cancellation when finalErr is nil.
type fakeSubscriptionScript struct {
	subscribeErr error
	advances     []cantonclient.CoreContractLedgerAdvance
	finalErr     error
	advanceFeed  <-chan cantonclient.CoreContractLedgerAdvance
}

// fakeCoreContractLedgerReader replays scripted answers. The last snapshot response repeats; a subscribe
// after the last script gets a stream that stays open.
type fakeCoreContractLedgerReader struct {
	mutex                    sync.Mutex
	ledgerEnd                int64
	ledgerEndErr             error
	snapshotResponses        []fakeSnapshotResponse
	subscriptionScripts      []fakeSubscriptionScript
	snapshotOffsets          []int64
	snapshotReplayNodeLimits []cantonclient.ReplayNodeSnapshotLimit
	subscribeBeginOffsets    []int64
	ledgerEndReadCount       int
	// snapshotReadGates[i], when not nil, holds snapshot read i until the gate closes.
	snapshotReadGates     []chan struct{}
	readPartyUnhosted     bool
	readPartyHostingErr   error
	readPartyHostingReads int
	// ledgerEndReadDuration delays each GetLedgerEnd answer; ledgerEndReadStartedAt is the entry time of the last call.
	ledgerEndReadDuration  time.Duration
	ledgerEndReadStartedAt time.Time
}

func (reader *fakeCoreContractLedgerReader) ReadPartyIsHosted(context.Context) (bool, error) {
	reader.mutex.Lock()
	defer reader.mutex.Unlock()
	reader.readPartyHostingReads++
	if reader.readPartyHostingErr != nil {
		return false, reader.readPartyHostingErr
	}
	return !reader.readPartyUnhosted, nil
}

func (reader *fakeCoreContractLedgerReader) setReadPartyHosting(unhosted bool, hostingErr error) {
	reader.mutex.Lock()
	defer reader.mutex.Unlock()
	reader.readPartyUnhosted = unhosted
	reader.readPartyHostingErr = hostingErr
}

func (reader *fakeCoreContractLedgerReader) readPartyHostingReadCount() int {
	reader.mutex.Lock()
	defer reader.mutex.Unlock()
	return reader.readPartyHostingReads
}

func (reader *fakeCoreContractLedgerReader) GetLedgerEnd(context.Context) (int64, error) {
	reader.mutex.Lock()
	reader.ledgerEndReadStartedAt = time.Now()
	readDuration := reader.ledgerEndReadDuration
	reader.mutex.Unlock()
	time.Sleep(readDuration)
	reader.mutex.Lock()
	defer reader.mutex.Unlock()
	reader.ledgerEndReadCount++
	return reader.ledgerEnd, reader.ledgerEndErr
}

// ReadActiveCoreContracts applies the limits of the gRPC reader through cantonclient.CollectActiveCoreContractsForTest.
func (reader *fakeCoreContractLedgerReader) ReadActiveCoreContracts(ctx context.Context, activeAtOffset int64, replayNodeLimit cantonclient.ReplayNodeSnapshotLimit) (cantonclient.ActiveCoreContracts, error) {
	reader.mutex.Lock()
	readIndex := len(reader.snapshotOffsets)
	reader.snapshotOffsets = append(reader.snapshotOffsets, activeAtOffset)
	if readIndex < len(reader.snapshotReadGates) && reader.snapshotReadGates[readIndex] != nil {
		gate := reader.snapshotReadGates[readIndex]
		reader.mutex.Unlock()
		select {
		case <-gate:
		case <-ctx.Done():
			return cantonclient.ActiveCoreContracts{}, ctx.Err()
		}
		reader.mutex.Lock()
	}
	defer reader.mutex.Unlock()
	reader.snapshotReplayNodeLimits = append(reader.snapshotReplayNodeLimits, replayNodeLimit)
	if len(reader.snapshotResponses) == 0 {
		return cantonclient.ActiveCoreContracts{}, errors.New("fake reader: no scripted snapshot")
	}
	response := reader.snapshotResponses[0]
	if len(reader.snapshotResponses) > 1 {
		reader.snapshotResponses = reader.snapshotResponses[1:]
	}
	if response.err != nil {
		return cantonclient.ActiveCoreContracts{}, response.err
	}
	return cantonclient.CollectActiveCoreContractsForTest(response.contracts, replayNodeLimit)
}

func (reader *fakeCoreContractLedgerReader) SubscribeCoreContractUpdates(ctx context.Context, beginExclusive int64, out chan<- cantonclient.CoreContractLedgerAdvance) (*cantonclient.Subscription, error) {
	reader.mutex.Lock()
	reader.subscribeBeginOffsets = append(reader.subscribeBeginOffsets, beginExclusive)
	script := fakeSubscriptionScript{}
	if len(reader.subscriptionScripts) > 0 {
		script = reader.subscriptionScripts[0]
		reader.subscriptionScripts = reader.subscriptionScripts[1:]
	}
	reader.mutex.Unlock()
	if script.subscribeErr != nil {
		return nil, script.subscribeErr
	}
	streamCtx, cancel := context.WithCancel(ctx)
	subscription := cantonclient.NewSubscription(cancel)
	go func() {
		defer subscription.Close()
		for _, advance := range script.advances {
			select {
			case <-streamCtx.Done():
				return
			case out <- advance:
			}
		}
		if script.finalErr != nil {
			subscription.Fail(script.finalErr)
			return
		}
		// Runs until cancellation.
		for {
			select {
			case <-streamCtx.Done():
				return
			case advance := <-script.advanceFeed:
				select {
				case <-streamCtx.Done():
					return
				case out <- advance:
				}
			}
		}
	}()
	return subscription, nil
}

func (reader *fakeCoreContractLedgerReader) callCounts() (snapshotReads int, subscribes int) {
	reader.mutex.Lock()
	defer reader.mutex.Unlock()
	return len(reader.snapshotOffsets), len(reader.subscribeBeginOffsets)
}

func (reader *fakeCoreContractLedgerReader) recordedSubscribeBeginOffsets() []int64 {
	reader.mutex.Lock()
	defer reader.mutex.Unlock()
	return append([]int64(nil), reader.subscribeBeginOffsets...)
}

func testWriterTiming() IndexWriterTiming {
	return IndexWriterTiming{
		LedgerEndPollInterval:           5 * time.Millisecond,
		ResnapshotInterval:              time.Hour,
		SnapshotReadTimeout:             time.Second,
		ReconnectInitialDelay:           time.Millisecond,
		ReconnectMaxDelay:               2 * time.Millisecond,
		MaxConsecutiveReconnectAttempts: 2,
		RebuildInitialDelay:             time.Millisecond,
		RebuildMaxDelay:                 2 * time.Millisecond,
		MaxConsecutiveRebuildAttempts:   3,
		StableFollowDuration:            time.Hour,
		StalledStreamTimeout:            time.Hour,
	}
}

// runTestWriter starts Run and returns the served index and a function that stops Run and returns its result.
func runTestWriter(t *testing.T, reader *fakeCoreContractLedgerReader, byteCapBytes int64, timing IndexWriterTiming) (*ServedIndex, func() error) {
	t.Helper()
	served, err := NewServedIndex(time.Minute)
	require.NoError(t, err)
	return served, runTestWriterOn(t, reader, served, byteCapBytes, timing)
}

func runTestWriterOn(t *testing.T, reader *fakeCoreContractLedgerReader, served *ServedIndex, byteCapBytes int64, timing IndexWriterTiming) func() error {
	t.Helper()
	writer, err := NewCoreContractIndexWriter(reader, served, testPartyID(t, testGuardianGovernance), byteCapBytes, timing, zap.NewNop())
	require.NoError(t, err)
	ctx, cancel := context.WithCancel(context.Background())
	runResult := make(chan error, 1)
	go func() { runResult <- writer.Run(ctx) }()
	stop := func() error {
		cancel()
		select {
		case err := <-runResult:
			return err
		case <-time.After(5 * time.Second):
			t.Fatal("writer did not stop")
			return nil
		}
	}
	t.Cleanup(func() { cancel() })
	return stop
}

func requireEventuallyIndexOffset(t *testing.T, served *ServedIndex, expectedOffset int64) {
	t.Helper()
	require.Eventually(t, func() bool {
		status, hasIndex := served.IndexStatus()
		return hasIndex && status.Offset == expectedOffset
	}, 5*time.Second, time.Millisecond, "index offset %d", expectedOffset)
}

func rebuildCount(reason IndexRebuildReason) float64 {
	return testutil.ToFloat64(indexRebuildsCounter.WithLabelValues(string(reason)))
}

func TestWriterBootstrapsAndAppliesOneTransactionPerAdvance(t *testing.T) {
	ledger := testDecodingLedger()
	root := decodedReplayNodeEvent(1, "")
	splitChanges := []cantonclienttest.CoreContractChange{cantonclienttest.Archived(root)}
	for nibbleIndex, nibble := range "0123456789abcdef" {
		splitChanges = append(splitChanges, cantonclienttest.Created(decodedReplayNodeEvent(100+nibbleIndex, string(nibble))))
	}
	reader := &fakeCoreContractLedgerReader{
		ledgerEnd:         testBootstrapOffset,
		snapshotResponses: []fakeSnapshotResponse{{contracts: decodedGenesisSnapshot(t, ledger, root)}},
		subscriptionScripts: []fakeSubscriptionScript{{advances: []cantonclient.CoreContractLedgerAdvance{
			ledger.DecodeTransaction(t, testBootstrapOffset, 11, splitChanges...),
			ledger.DecodeOffsetCheckpoint(t, 11, 15),
		}}},
	}
	served, stop := runTestWriter(t, reader, testLargeIndexByteCap, testWriterTiming())
	requireEventuallyIndexOffset(t, served, 15)

	result, err := served.LookupCoveringReplayNode(testPartyID(t, testConsumer), testReplayNamespace(t, testNamespace), testReplayDigest(t, testCoveredDigestHexText))
	require.NoError(t, err)
	require.Equal(t, testContractIDHex(110), result.Contract().ContractID().String())
	require.Equal(t, int64(15), result.LiveAtIndexOffset())
	status, _ := served.IndexStatus()
	require.Equal(t, 16, status.LiveReplayNodeCount)
	require.Eventually(t, func() bool { return served.ServiceReadiness().Status() == ServiceReadinessStatusReady }, 5*time.Second, time.Millisecond)

	require.NoError(t, stop())
	snapshotReads, _ := reader.callCounts()
	require.Equal(t, 1, snapshotReads)
	require.Equal(t, []int64{testBootstrapOffset}, reader.recordedSubscribeBeginOffsets())
	require.False(t, served.ServiceReadiness().StreamConnected)
}

func TestWriterReconnectsFromTheLastAppliedOffset(t *testing.T) {
	ledger := testDecodingLedger()
	reader := &fakeCoreContractLedgerReader{
		ledgerEnd:         testBootstrapOffset,
		snapshotResponses: []fakeSnapshotResponse{{contracts: decodedGenesisSnapshot(t, ledger)}},
		subscriptionScripts: []fakeSubscriptionScript{
			{advances: []cantonclient.CoreContractLedgerAdvance{ledger.DecodeTransaction(t, testBootstrapOffset, 11, cantonclienttest.Created(decodedReplayNodeEvent(1, "")))}, finalErr: status.Error(codes.Unavailable, "connection reset")},
			{subscribeErr: status.Error(codes.Unavailable, "connection refused")},
			{advances: []cantonclient.CoreContractLedgerAdvance{ledger.DecodeOffsetCheckpoint(t, 11, 12)}, finalErr: fmt.Errorf("GetUpdates stream ended: %w", errors.New("EOF"))},
			{advances: []cantonclient.CoreContractLedgerAdvance{ledger.DecodeOffsetCheckpoint(t, 12, 13)}},
		},
	}
	served, stop := runTestWriter(t, reader, testLargeIndexByteCap, testWriterTiming())
	requireEventuallyIndexOffset(t, served, 13)
	require.NoError(t, stop())
	snapshotReads, _ := reader.callCounts()
	require.Equal(t, 1, snapshotReads)
	require.Equal(t, []int64{testBootstrapOffset, 11, 11, 12}, reader.recordedSubscribeBeginOffsets())
}

func TestWriterRebuilds(t *testing.T) {
	ledger := testDecodingLedger()
	coreState := decodedSingletonEvent(testGenesisSerialBase, cantonclient.CoreContractTemplateCoreState)
	prunedErr := fmt.Errorf("GetUpdates stream ended: %w", status.Error(codes.FailedPrecondition, "PARTICIPANT_PRUNED_DATA_ACCESSED(9,0): Transactions request from 10 to 20 precedes pruned offset 15"))
	unavailableErr := status.Error(codes.Unavailable, "connection refused")
	testCases := []struct {
		name                string
		subscriptionScripts func(t *testing.T) []fakeSubscriptionScript
		expectedReason      IndexRebuildReason
	}{
		{
			name: "pruned offset",
			subscriptionScripts: func(t *testing.T) []fakeSubscriptionScript {
				return []fakeSubscriptionScript{{finalErr: prunedErr}}
			},
			expectedReason: IndexRebuildReasonPrunedOffset,
		},
		{
			name: "pruned offset at subscribe",
			subscriptionScripts: func(t *testing.T) []fakeSubscriptionScript {
				return []fakeSubscriptionScript{{subscribeErr: prunedErr}}
			},
			expectedReason: IndexRebuildReasonPrunedOffset,
		},
		{
			name: "other failed precondition",
			subscriptionScripts: func(t *testing.T) []fakeSubscriptionScript {
				return []fakeSubscriptionScript{{finalErr: status.Error(codes.FailedPrecondition, "SOME_OTHER_PRECONDITION")}}
			},
			expectedReason: IndexRebuildReasonFailedPrecondition,
		},
		{
			name: "decoder refusal",
			subscriptionScripts: func(t *testing.T) []fakeSubscriptionScript {
				return []fakeSubscriptionScript{{finalErr: fmt.Errorf("GetUpdates: %w: unapproved package", cantonclient.ErrCoreContractStreamDecode)}}
			},
			expectedReason: IndexRebuildReasonStreamDecodeFailure,
		},
		{
			name: "terminal index readiness after a duplicate create",
			subscriptionScripts: func(t *testing.T) []fakeSubscriptionScript {
				return []fakeSubscriptionScript{{advances: []cantonclient.CoreContractLedgerAdvance{ledger.DecodeTransaction(t, testBootstrapOffset, 11, cantonclienttest.Created(coreState))}}}
			},
			expectedReason: IndexRebuildReasonInconsistentIndex,
		},
		{
			name: "reconnects exhausted",
			subscriptionScripts: func(t *testing.T) []fakeSubscriptionScript {
				return []fakeSubscriptionScript{{subscribeErr: unavailableErr}, {subscribeErr: unavailableErr}, {subscribeErr: unavailableErr}}
			},
			expectedReason: IndexRebuildReasonReconnectsExhausted,
		},
		{
			name: "reconnects exhausted after stream failures",
			subscriptionScripts: func(t *testing.T) []fakeSubscriptionScript {
				return []fakeSubscriptionScript{{finalErr: unavailableErr}, {finalErr: unavailableErr}, {finalErr: unavailableErr}}
			},
			expectedReason: IndexRebuildReasonReconnectsExhausted,
		},
	}
	for _, testCase := range testCases {
		t.Run(testCase.name, func(t *testing.T) {
			reader := &fakeCoreContractLedgerReader{
				ledgerEnd:           testBootstrapOffset,
				snapshotResponses:   []fakeSnapshotResponse{{contracts: decodedGenesisSnapshot(t, ledger)}},
				subscriptionScripts: testCase.subscriptionScripts(t),
			}
			rebuildsBefore := rebuildCount(testCase.expectedReason)
			served, stop := runTestWriter(t, reader, testLargeIndexByteCap, testWriterTiming())
			require.Eventually(t, func() bool {
				snapshotReads, _ := reader.callCounts()
				return snapshotReads == 2 && served.ServiceReadiness().StreamConnected
			}, 5*time.Second, time.Millisecond)
			require.NoError(t, stop())
			require.Equal(t, rebuildsBefore+1, rebuildCount(testCase.expectedReason))
			status, hasIndex := served.IndexStatus()
			require.True(t, hasIndex)
			require.Equal(t, IndexReadinessReady, status.Readiness)
			require.Equal(t, testBootstrapOffset, status.Offset)
		})
	}
}

func TestWriterStopsAfterBoundedRebuildAttempts(t *testing.T) {
	ledger := testDecodingLedger()
	prunedErr := status.Error(codes.FailedPrecondition, "PARTICIPANT_PRUNED_DATA_ACCESSED(9,0): pruned")
	testCases := []struct {
		name                   string
		reader                 func(t *testing.T) *fakeCoreContractLedgerReader
		expectedSnapshotReads  int
		expectedLedgerEndReads int
	}{
		{
			name: "ledger end read fails",
			reader: func(t *testing.T) *fakeCoreContractLedgerReader {
				return &fakeCoreContractLedgerReader{ledgerEndErr: status.Error(codes.Unavailable, "down")}
			},
			expectedSnapshotReads:  0,
			expectedLedgerEndReads: 3,
		},
		{
			name: "snapshot read fails",
			reader: func(t *testing.T) *fakeCoreContractLedgerReader {
				return &fakeCoreContractLedgerReader{ledgerEnd: testBootstrapOffset, snapshotResponses: []fakeSnapshotResponse{{err: status.Error(codes.Internal, "boom")}}}
			},
			expectedSnapshotReads:  3,
			expectedLedgerEndReads: 3,
		},
		{
			name: "every stream fails on a pruned offset",
			reader: func(t *testing.T) *fakeCoreContractLedgerReader {
				return &fakeCoreContractLedgerReader{
					ledgerEnd:           testBootstrapOffset,
					snapshotResponses:   []fakeSnapshotResponse{{contracts: decodedGenesisSnapshot(t, ledger)}},
					subscriptionScripts: []fakeSubscriptionScript{{subscribeErr: prunedErr}, {subscribeErr: prunedErr}, {subscribeErr: prunedErr}, {subscribeErr: prunedErr}},
				}
			},
			expectedSnapshotReads: 3,
		},
	}
	for _, testCase := range testCases {
		t.Run(testCase.name, func(t *testing.T) {
			reader := testCase.reader(t)
			served, err := NewServedIndex(time.Minute)
			require.NoError(t, err)
			writer, err := NewCoreContractIndexWriter(reader, served, testPartyID(t, testGuardianGovernance), testLargeIndexByteCap, testWriterTiming(), zap.NewNop())
			require.NoError(t, err)
			ctx, cancel := context.WithTimeout(context.Background(), 5*time.Second)
			defer cancel()
			err = writer.Run(ctx)
			require.ErrorIs(t, err, ErrRebuildAttemptsExhausted)
			require.NoError(t, ctx.Err())
			snapshotReads, _ := reader.callCounts()
			require.Equal(t, testCase.expectedSnapshotReads, snapshotReads)
			if testCase.expectedLedgerEndReads > 0 {
				require.Equal(t, testCase.expectedLedgerEndReads, reader.ledgerEndReadCount)
			}
		})
	}
}

func TestWriterResnapshot(t *testing.T) {
	ledger := testDecodingLedger()
	node := decodedReplayNodeEvent(1, "a")
	nodeWithOtherBlob := node
	nodeWithOtherBlob.CreatedEventBlob = testBlob(9, testReplayNodeBlobBytes)
	testCases := []struct {
		name                     string
		resnapshot               func(t *testing.T) fakeSnapshotResponse
		expectRebuild            bool
		expectedDifferenceKind   SnapshotDifferenceKind
		expectedDifferenceCount  float64
		expectedResnapshotFailed bool
	}{
		{name: "snapshot matches", resnapshot: func(t *testing.T) fakeSnapshotResponse {
			return fakeSnapshotResponse{contracts: decodedGenesisSnapshot(t, ledger, node)}
		}},
		{name: "contract stale in the index", resnapshot: func(t *testing.T) fakeSnapshotResponse {
			return fakeSnapshotResponse{contracts: decodedGenesisSnapshot(t, ledger)}
		}, expectRebuild: true, expectedDifferenceKind: SnapshotDifferenceStaleInIndex, expectedDifferenceCount: 1},
		{name: "contract missing from the index", resnapshot: func(t *testing.T) fakeSnapshotResponse {
			return fakeSnapshotResponse{contracts: decodedGenesisSnapshot(t, ledger, node, decodedReplayNodeEvent(2, "b"))}
		}, expectRebuild: true, expectedDifferenceKind: SnapshotDifferenceMissingFromIndex, expectedDifferenceCount: 1},
		{name: "content mismatch only", resnapshot: func(t *testing.T) fakeSnapshotResponse {
			return fakeSnapshotResponse{contracts: decodedGenesisSnapshot(t, ledger, nodeWithOtherBlob)}
		}, expectedDifferenceKind: SnapshotDifferenceContentMismatch, expectedDifferenceCount: 1},
		{name: "snapshot read fails", resnapshot: func(t *testing.T) fakeSnapshotResponse {
			return fakeSnapshotResponse{err: status.Error(codes.Unavailable, "down")}
		}, expectedResnapshotFailed: true},
		{name: "snapshot offset pruned", resnapshot: func(t *testing.T) fakeSnapshotResponse {
			return fakeSnapshotResponse{err: status.Error(codes.FailedPrecondition, "PARTICIPANT_PRUNED_DATA_ACCESSED(9,0): pruned")}
		}, expectRebuild: true},
	}
	for _, testCase := range testCases {
		t.Run(testCase.name, func(t *testing.T) {
			bootstrapSnapshot := fakeSnapshotResponse{contracts: decodedGenesisSnapshot(t, ledger, node)}
			reader := &fakeCoreContractLedgerReader{
				ledgerEnd:         testBootstrapOffset,
				snapshotResponses: []fakeSnapshotResponse{bootstrapSnapshot, testCase.resnapshot(t), bootstrapSnapshot},
			}
			timing := testWriterTiming()
			timing.ResnapshotInterval = 20 * time.Millisecond
			differencesBefore := 0.0
			if testCase.expectedDifferenceKind != 0 {
				differencesBefore = testutil.ToFloat64(resnapshotDifferencesCounter.WithLabelValues(testCase.expectedDifferenceKind.String()))
			}
			failuresBefore := testutil.ToFloat64(resnapshotFailuresCounter)
			served, stop := runTestWriter(t, reader, testLargeIndexByteCap, timing)
			require.Eventually(t, func() bool {
				snapshotReads, _ := reader.callCounts()
				return snapshotReads >= 4
			}, 5*time.Second, time.Millisecond)
			require.NoError(t, stop())

			reader.mutex.Lock()
			snapshotOffsets := append([]int64(nil), reader.snapshotOffsets...)
			subscribeCount := len(reader.subscribeBeginOffsets)
			reader.mutex.Unlock()
			for _, snapshotOffset := range snapshotOffsets {
				require.Equal(t, testBootstrapOffset, snapshotOffset)
			}
			if testCase.expectRebuild {
				require.GreaterOrEqual(t, subscribeCount, 2)
			} else {
				require.Equal(t, 1, subscribeCount)
			}
			if testCase.expectedDifferenceKind != 0 {
				differencesAfter := testutil.ToFloat64(resnapshotDifferencesCounter.WithLabelValues(testCase.expectedDifferenceKind.String()))
				require.Equal(t, differencesBefore+testCase.expectedDifferenceCount, differencesAfter)
			}
			if testCase.expectedResnapshotFailed {
				require.Greater(t, testutil.ToFloat64(resnapshotFailuresCounter), failuresBefore)
			}
			status, hasIndex := served.IndexStatus()
			require.True(t, hasIndex)
			require.Equal(t, 1, status.LiveReplayNodeCount)
		})
	}
}

func TestWriterRetriesTheByteCapAtTheResnapshotInterval(t *testing.T) {
	ledger := testDecodingLedger()
	const byteCapBytes = int64(20_000)
	oversizedNode := decodedReplayNodeEvent(1, "")
	oversizedNode.CreatedEventBlob = testBlob(1, int(byteCapBytes))
	reader := &fakeCoreContractLedgerReader{
		ledgerEnd: testBootstrapOffset,
		snapshotResponses: []fakeSnapshotResponse{
			{contracts: decodedGenesisSnapshot(t, ledger, oversizedNode)},
			{contracts: decodedGenesisSnapshot(t, ledger, decodedReplayNodeEvent(2, ""))},
		},
	}
	timing := testWriterTiming()
	timing.ResnapshotInterval = 20 * time.Millisecond
	rebuildsBefore := rebuildCount(IndexRebuildReasonByteCapRetry)
	served, stop := runTestWriter(t, reader, byteCapBytes, timing)
	require.Eventually(t, func() bool {
		status, hasIndex := served.IndexStatus()
		return hasIndex && status.Readiness == IndexReadinessReady
	}, 5*time.Second, time.Millisecond)
	require.NoError(t, stop())
	require.Equal(t, rebuildsBefore+1, rebuildCount(IndexRebuildReasonByteCapRetry))
	result, err := served.LookupCoveringReplayNode(testPartyID(t, testConsumer), testReplayNamespace(t, testNamespace), testReplayDigest(t, testCoveredDigestHexText))
	require.NoError(t, err)
	require.Equal(t, testContractIDHex(2), result.Contract().ContractID().String())
}

func TestWriterMeasuresLedgerLag(t *testing.T) {
	ledger := testDecodingLedger()
	reader := &fakeCoreContractLedgerReader{
		ledgerEnd:         testBootstrapOffset,
		snapshotResponses: []fakeSnapshotResponse{{contracts: decodedGenesisSnapshot(t, ledger)}},
	}
	served, stop := runTestWriter(t, reader, testLargeIndexByteCap, testWriterTiming())
	require.Eventually(t, func() bool { return served.ServiceReadiness().IsReady() }, 5*time.Second, time.Millisecond)
	reader.mutex.Lock()
	reader.ledgerEnd = testBootstrapOffset + 40
	reader.mutex.Unlock()
	require.Eventually(t, func() bool { return served.LedgerOffsetsBehind() == 40 }, 5*time.Second, time.Millisecond)
	require.NoError(t, stop())
}

func TestNewCoreContractIndexWriterRejects(t *testing.T) {
	served, err := NewServedIndex(time.Minute)
	require.NoError(t, err)
	reader := &fakeCoreContractLedgerReader{}
	guardianGovernance := testPartyID(t, testGuardianGovernance)
	testCases := []struct {
		name         string
		reader       CoreContractLedgerReader
		served       *ServedIndex
		party        cantonclient.PartyID
		byteCapBytes int64
		timing       func() IndexWriterTiming
	}{
		{name: "missing reader", served: served, party: guardianGovernance, byteCapBytes: testLargeIndexByteCap, timing: testWriterTiming},
		{name: "missing served index", reader: reader, party: guardianGovernance, byteCapBytes: testLargeIndexByteCap, timing: testWriterTiming},
		{name: "zero party", reader: reader, served: served, byteCapBytes: testLargeIndexByteCap, timing: testWriterTiming},
		{name: "byte cap below the entry overhead", reader: reader, served: served, party: guardianGovernance, byteCapBytes: indexEntryOverheadBytes, timing: testWriterTiming},
		{name: "zero poll interval", reader: reader, served: served, party: guardianGovernance, byteCapBytes: testLargeIndexByteCap, timing: func() IndexWriterTiming {
			timing := testWriterTiming()
			timing.LedgerEndPollInterval = 0
			return timing
		}},
		{name: "poll interval equal to the ledger lag bound", reader: reader, served: served, party: guardianGovernance, byteCapBytes: testLargeIndexByteCap, timing: func() IndexWriterTiming {
			timing := testWriterTiming()
			timing.LedgerEndPollInterval = time.Minute
			return timing
		}},
		{name: "reconnect delays reversed", reader: reader, served: served, party: guardianGovernance, byteCapBytes: testLargeIndexByteCap, timing: func() IndexWriterTiming {
			timing := testWriterTiming()
			timing.ReconnectInitialDelay = timing.ReconnectMaxDelay + 1
			return timing
		}},
		{name: "zero rebuild attempts", reader: reader, served: served, party: guardianGovernance, byteCapBytes: testLargeIndexByteCap, timing: func() IndexWriterTiming {
			timing := testWriterTiming()
			timing.MaxConsecutiveRebuildAttempts = 0
			return timing
		}},
		{name: "too many reconnect attempts", reader: reader, served: served, party: guardianGovernance, byteCapBytes: testLargeIndexByteCap, timing: func() IndexWriterTiming {
			timing := testWriterTiming()
			timing.MaxConsecutiveReconnectAttempts = maxConsecutiveAttemptLimit + 1
			return timing
		}},
	}
	for _, testCase := range testCases {
		t.Run(testCase.name, func(t *testing.T) {
			writer, err := NewCoreContractIndexWriter(testCase.reader, testCase.served, testCase.party, testCase.byteCapBytes, testCase.timing(), zap.NewNop())
			require.Error(t, err)
			require.Nil(t, writer)
		})
	}
	writer, err := NewCoreContractIndexWriter(reader, served, guardianGovernance, ProductionIndexByteCapBytes, ProductionIndexWriterTiming(ProductionResnapshotInterval), zap.NewNop())
	require.NoError(t, err)
	require.Equal(t, int(ProductionIndexByteCapBytes/indexEntryOverheadBytes), writer.snapshotReplayNodeLimit.MaxReplayNodeCount())
	require.Equal(t, ProductionIndexByteCapBytes, writer.snapshotReplayNodeLimit.MaxChargedBytes())
}

func TestCheckProductionResnapshotInterval(t *testing.T) {
	testCases := []struct {
		interval    time.Duration
		expectError bool
	}{
		{interval: ProductionResnapshotInterval},
		{interval: minResnapshotInterval},
		{interval: maxResnapshotInterval},
		{interval: minResnapshotInterval - time.Nanosecond, expectError: true},
		{interval: maxResnapshotInterval + time.Nanosecond, expectError: true},
		{interval: 0, expectError: true},
	}
	for _, testCase := range testCases {
		t.Run(testCase.interval.String(), func(t *testing.T) {
			err := CheckProductionResnapshotInterval(testCase.interval)
			require.Equal(t, testCase.expectError, err != nil)
		})
	}
}

func TestBackoffDelay(t *testing.T) {
	testCases := []struct {
		attempt       int
		expectedDelay time.Duration
	}{
		{attempt: 1, expectedDelay: time.Second},
		{attempt: 2, expectedDelay: 2 * time.Second},
		{attempt: 3, expectedDelay: 4 * time.Second},
		{attempt: 6, expectedDelay: 30 * time.Second},
		{attempt: maxConsecutiveAttemptLimit, expectedDelay: 30 * time.Second},
	}
	for _, testCase := range testCases {
		require.Equal(t, testCase.expectedDelay, backoffDelay(testCase.attempt, time.Second, 30*time.Second), "attempt %d", testCase.attempt)
	}
	require.Panics(t, func() { backoffDelay(0, time.Second, 30*time.Second) })
}

func TestClassifyStreamFailure(t *testing.T) {
	testCases := []struct {
		name           string
		err            error
		expectedAction streamFailureAction
		expectedReason IndexRebuildReason
	}{
		{name: "pruned offset", err: status.Error(codes.FailedPrecondition, "PARTICIPANT_PRUNED_DATA_ACCESSED(9,0)"), expectedAction: streamFailureActionRebuild, expectedReason: IndexRebuildReasonPrunedOffset},
		{name: "other failed precondition", err: status.Error(codes.FailedPrecondition, "OTHER"), expectedAction: streamFailureActionRebuild, expectedReason: IndexRebuildReasonFailedPrecondition},
		{name: "decoder refusal", err: fmt.Errorf("GetUpdates: %w: x", cantonclient.ErrCoreContractStreamDecode), expectedAction: streamFailureActionRebuild, expectedReason: IndexRebuildReasonStreamDecodeFailure},
		{name: "unavailable", err: status.Error(codes.Unavailable, "down"), expectedAction: streamFailureActionReconnect},
		{name: "deadline exceeded", err: status.Error(codes.DeadlineExceeded, "slow"), expectedAction: streamFailureActionReconnect},
		{name: "plain error", err: errors.New("stream ended"), expectedAction: streamFailureActionReconnect},
	}
	for _, testCase := range testCases {
		t.Run(testCase.name, func(t *testing.T) {
			action, reason := classifyStreamFailure(testCase.err)
			require.Equal(t, testCase.expectedAction, action)
			require.Equal(t, testCase.expectedReason, reason)
		})
	}
}

// testReplayNodeLimitByteCapBytes admits 13 snapshot ReplayNodes: 20000 / 1536.
const testReplayNodeLimitByteCapBytes = int64(20_000)

func decodedReplayNodesAboveTheSnapshotLimit() []cantonclienttest.CoreContractEvent {
	replayNodeCount := int(testReplayNodeLimitByteCapBytes/indexEntryOverheadBytes) + 1
	replayNodes := make([]cantonclienttest.CoreContractEvent, 0, replayNodeCount)
	for serial := 1; serial <= replayNodeCount; serial++ {
		replayNodes = append(replayNodes, decodedReplayNodeEvent(serial, fmt.Sprintf("%02x", serial)))
	}
	return replayNodes
}

// decodedReplayNodesAboveTheSnapshotByteLimit are three nodes within the count limit whose blobs exceed the byte cap.
func decodedReplayNodesAboveTheSnapshotByteLimit() []cantonclienttest.CoreContractEvent {
	const replayNodeCount = 3
	replayNodes := make([]cantonclienttest.CoreContractEvent, 0, replayNodeCount)
	for serial := 1; serial <= replayNodeCount; serial++ {
		replayNode := decodedReplayNodeEvent(serial, fmt.Sprintf("%x", serial))
		replayNode.CreatedEventBlob = testBlob(serial, int(testReplayNodeLimitByteCapBytes/replayNodeCount))
		replayNodes = append(replayNodes, replayNode)
	}
	return replayNodes
}

func TestWriterServesSingletonsPastTheSnapshotReplayNodeLimit(t *testing.T) {
	ledger := testDecodingLedger()
	require.Equal(t, 13, int(testReplayNodeLimitByteCapBytes/indexEntryOverheadBytes))
	oversizedNode := decodedReplayNodeEvent(1, "")
	oversizedNode.CreatedEventBlob = testBlob(1, int(testReplayNodeLimitByteCapBytes))
	testCases := []struct {
		name                  string
		snapshotResponses     func(t *testing.T) []fakeSnapshotResponse
		expectedRebuildReason IndexRebuildReason
	}{
		{
			name: "above the limit at the bootstrap",
			snapshotResponses: func(t *testing.T) []fakeSnapshotResponse {
				return []fakeSnapshotResponse{{contracts: decodedGenesisSnapshot(t, ledger, decodedReplayNodesAboveTheSnapshotLimit()...)}}
			},
			expectedRebuildReason: IndexRebuildReasonByteCapRetry,
		},
		{
			name: "above the limit at a byte cap retry",
			snapshotResponses: func(t *testing.T) []fakeSnapshotResponse {
				return []fakeSnapshotResponse{
					{contracts: decodedGenesisSnapshot(t, ledger, oversizedNode)},
					{contracts: decodedGenesisSnapshot(t, ledger, decodedReplayNodesAboveTheSnapshotLimit()...)},
				}
			},
			expectedRebuildReason: IndexRebuildReasonByteCapRetry,
		},
		{
			name: "above the limit at a resnapshot",
			snapshotResponses: func(t *testing.T) []fakeSnapshotResponse {
				return []fakeSnapshotResponse{
					{contracts: decodedGenesisSnapshot(t, ledger, decodedReplayNodeEvent(1, ""))},
					{contracts: decodedGenesisSnapshot(t, ledger, decodedReplayNodesAboveTheSnapshotLimit()...)},
				}
			},
			expectedRebuildReason: IndexRebuildReasonSnapshotReplayNodeLimit,
		},
		{
			name: "above the byte limit within the count limit at a resnapshot",
			snapshotResponses: func(t *testing.T) []fakeSnapshotResponse {
				return []fakeSnapshotResponse{
					{contracts: decodedGenesisSnapshot(t, ledger, decodedReplayNodeEvent(1, ""))},
					{contracts: decodedGenesisSnapshot(t, ledger, decodedReplayNodesAboveTheSnapshotByteLimit()...)},
				}
			},
			expectedRebuildReason: IndexRebuildReasonSnapshotReplayNodeLimit,
		},
	}
	for _, testCase := range testCases {
		t.Run(testCase.name, func(t *testing.T) {
			reader := &fakeCoreContractLedgerReader{ledgerEnd: testBootstrapOffset, snapshotResponses: testCase.snapshotResponses(t)}
			timing := testWriterTiming()
			timing.ResnapshotInterval = 10 * time.Millisecond
			rebuildsBefore := rebuildCount(testCase.expectedRebuildReason)
			served, stop := runTestWriter(t, reader, testReplayNodeLimitByteCapBytes, timing)

			minimumSnapshotReads := 2 * timing.MaxConsecutiveRebuildAttempts
			require.Eventually(t, func() bool {
				snapshotReads, _ := reader.callCounts()
				return snapshotReads >= minimumSnapshotReads
			}, 5*time.Second, time.Millisecond)
			status, hasIndex := served.IndexStatus()
			require.True(t, hasIndex)
			require.Equal(t, IndexReadinessByteCapReached, status.Readiness)
			require.True(t, status.ReplayNodesDroppedAtByteCap)
			require.Equal(t, 0, status.LiveReplayNodeCount)
			for _, template := range singletonTemplates {
				_, err := served.LookupSingleton(template)
				require.NoError(t, err, template.String())
			}
			_, err := served.LookupCoveringReplayNode(testPartyID(t, testConsumer), testReplayNamespace(t, testNamespace), testReplayDigest(t, testCoveredDigestHexText))
			requireIndexNotReady(t, err, IndexReadinessByteCapReached)

			require.NoError(t, stop())
			require.Greater(t, rebuildCount(testCase.expectedRebuildReason), rebuildsBefore)
			reader.mutex.Lock()
			for _, replayNodeLimit := range reader.snapshotReplayNodeLimits {
				require.Equal(t, 13, replayNodeLimit.MaxReplayNodeCount())
				require.Equal(t, testReplayNodeLimitByteCapBytes, replayNodeLimit.MaxChargedBytes())
			}
			reader.mutex.Unlock()
		})
	}
}

// requireReadinessStaysReadyUntil samples the readiness every millisecond until reached is true,
// and requires every sample to be ready.
func requireReadinessStaysReadyUntil(t *testing.T, served *ServedIndex, reached func() bool) {
	t.Helper()
	deadline := time.After(5 * time.Second)
	sampleTicker := time.NewTicker(time.Millisecond)
	defer sampleTicker.Stop()
	for !reached() {
		readiness := served.ServiceReadiness()
		require.True(t, readiness.IsReady(), "readiness %s", readiness.Status())
		select {
		case <-deadline:
			t.Fatal("condition not reached")
		case <-sampleTicker.C:
		}
	}
}

func TestWriterKeepsFollowingTheCappedIndexDuringAByteCapRetry(t *testing.T) {
	ledger := testDecodingLedger()
	const byteCapBytes = int64(20_000)
	oversizedNode := decodedReplayNodeEvent(1, "")
	oversizedNode.CreatedEventBlob = testBlob(1, int(byteCapBytes))
	replacementGate := make(chan struct{})
	advanceFeed := make(chan cantonclient.CoreContractLedgerAdvance)
	reader := &fakeCoreContractLedgerReader{
		ledgerEnd: testBootstrapOffset,
		snapshotResponses: []fakeSnapshotResponse{
			{contracts: decodedGenesisSnapshot(t, ledger, oversizedNode)},
			{contracts: decodedGenesisSnapshot(t, ledger, decodedReplayNodeEvent(2, ""))},
		},
		snapshotReadGates: []chan struct{}{nil, replacementGate},
		subscriptionScripts: []fakeSubscriptionScript{
			{advanceFeed: advanceFeed},
			{advances: []cantonclient.CoreContractLedgerAdvance{ledger.DecodeOffsetCheckpoint(t, testBootstrapOffset, testBootstrapOffset+2)}},
		},
	}
	timing := testWriterTiming()
	timing.ResnapshotInterval = 20 * time.Millisecond
	served, stop := runTestWriter(t, reader, byteCapBytes, timing)
	require.Eventually(t, func() bool {
		snapshotReads, _ := reader.callCounts()
		return snapshotReads == 2 && served.ServiceReadiness().Status() == ServiceReadinessStatusReadyByteCapReached
	}, 5*time.Second, time.Millisecond)

	reader.mutex.Lock()
	reader.ledgerEnd = testBootstrapOffset + 2
	reader.mutex.Unlock()
	advanceFeed <- ledger.DecodeOffsetCheckpoint(t, testBootstrapOffset, testBootstrapOffset+2)
	requireReadinessStaysReadyUntil(t, served, func() bool {
		status, _ := served.IndexStatus()
		return status.Offset == testBootstrapOffset+2 && served.LedgerOffsetsBehind() == 0
	})
	require.Equal(t, ServiceReadinessStatusReadyByteCapReached, served.ServiceReadiness().Status())
	for _, template := range singletonTemplates {
		_, err := served.LookupSingleton(template)
		require.NoError(t, err, template.String())
	}

	close(replacementGate)
	requireReadinessStaysReadyUntil(t, served, func() bool { return served.ServiceReadiness().Status() == ServiceReadinessStatusReady })
	result, err := served.LookupCoveringReplayNode(testPartyID(t, testConsumer), testReplayNamespace(t, testNamespace), testReplayDigest(t, testCoveredDigestHexText))
	require.NoError(t, err)
	require.Equal(t, testContractIDHex(2), result.Contract().ContractID().String())
	require.NoError(t, stop())
	snapshotReads, _ := reader.callCounts()
	require.Equal(t, 2, snapshotReads)
	require.Equal(t, []int64{testBootstrapOffset, testBootstrapOffset, testBootstrapOffset + 2}, reader.recordedSubscribeBeginOffsets())
}

func TestWriterSuspendsAStaleIndexAndSwapsAfterTheReplacementCatchesUp(t *testing.T) {
	ledger := testDecodingLedger()
	staleNode := decodedReplayNodeEvent(1, "")
	replacementGate := make(chan struct{})
	advanceFeed := make(chan cantonclient.CoreContractLedgerAdvance)
	reader := &fakeCoreContractLedgerReader{
		ledgerEnd: testBootstrapOffset,
		snapshotResponses: []fakeSnapshotResponse{
			{contracts: decodedGenesisSnapshot(t, ledger, staleNode)},
			{contracts: decodedGenesisSnapshot(t, ledger)},
		},
		snapshotReadGates: []chan struct{}{nil, nil, replacementGate},
		subscriptionScripts: []fakeSubscriptionScript{
			{advanceFeed: advanceFeed},
			{advances: []cantonclient.CoreContractLedgerAdvance{ledger.DecodeOffsetCheckpoint(t, testBootstrapOffset, testBootstrapOffset+2)}},
		},
	}
	timing := testWriterTiming()
	timing.ResnapshotInterval = 20 * time.Millisecond
	// A ready replacement skips the rebuild backoff.
	timing.RebuildInitialDelay = time.Hour
	timing.RebuildMaxDelay = time.Hour
	rebuildsBefore := rebuildCount(IndexRebuildReasonResnapshotDifference)
	served, stop := runTestWriter(t, reader, testLargeIndexByteCap, timing)
	require.Eventually(t, func() bool {
		snapshotReads, _ := reader.callCounts()
		return snapshotReads == 3 && served.ServiceReadiness().Status() == "index_resnapshot_difference"
	}, 5*time.Second, time.Millisecond)
	_, err := served.LookupSingleton(cantonclient.CoreContractTemplateCoreState)
	requireIndexNotReady(t, err, IndexReadinessResnapshotDifference)
	_, err = served.LookupCoveringReplayNode(testPartyID(t, testConsumer), testReplayNamespace(t, testNamespace), testReplayDigest(t, testCoveredDigestHexText))
	requireIndexNotReady(t, err, IndexReadinessResnapshotDifference)

	advanceFeed <- ledger.DecodeOffsetCheckpoint(t, testBootstrapOffset, testBootstrapOffset+2)
	requireEventuallyIndexOffset(t, served, testBootstrapOffset+2)
	close(replacementGate)
	require.Eventually(t, func() bool { return served.ServiceReadiness().Status() == ServiceReadinessStatusReady }, 5*time.Second, time.Millisecond)
	status, _ := served.IndexStatus()
	require.Equal(t, testBootstrapOffset+2, status.Offset)
	require.Equal(t, 0, status.LiveReplayNodeCount)
	_, err = served.LookupCoveringReplayNode(testPartyID(t, testConsumer), testReplayNamespace(t, testNamespace), testReplayDigest(t, testCoveredDigestHexText))
	require.ErrorIs(t, err, ErrNoCoveringReplayNode)
	require.NoError(t, stop())
	require.Equal(t, rebuildsBefore+1, rebuildCount(IndexRebuildReasonResnapshotDifference))
	require.Equal(t, []int64{testBootstrapOffset, testBootstrapOffset, testBootstrapOffset + 2}, reader.recordedSubscribeBeginOffsets())
}

func TestWriterKeepsTheCappedIndexWhenTheReplacementBuildFails(t *testing.T) {
	ledger := testDecodingLedger()
	const byteCapBytes = int64(20_000)
	oversizedNode := decodedReplayNodeEvent(1, "")
	oversizedNode.CreatedEventBlob = testBlob(1, int(byteCapBytes))
	reader := &fakeCoreContractLedgerReader{
		ledgerEnd: testBootstrapOffset,
		snapshotResponses: []fakeSnapshotResponse{
			{contracts: decodedGenesisSnapshot(t, ledger, oversizedNode)},
			{err: status.Error(codes.Unavailable, "down")},
		},
	}
	timing := testWriterTiming()
	timing.ResnapshotInterval = 10 * time.Millisecond
	buildFailuresBefore := testutil.ToFloat64(indexBuildFailuresCounter)
	served, stop := runTestWriter(t, reader, byteCapBytes, timing)
	require.Eventually(t, func() bool {
		snapshotReads, _ := reader.callCounts()
		return snapshotReads >= 4
	}, 5*time.Second, time.Millisecond)
	require.Equal(t, ServiceReadinessStatusReadyByteCapReached, served.ServiceReadiness().Status())
	require.NoError(t, stop())
	require.Greater(t, testutil.ToFloat64(indexBuildFailuresCounter), buildFailuresBefore)
	require.Equal(t, []int64{testBootstrapOffset}, reader.recordedSubscribeBeginOffsets())
}

func TestWriterReconnectsAStalledStream(t *testing.T) {
	ledger := testDecodingLedger()
	reader := &fakeCoreContractLedgerReader{
		ledgerEnd:         testBootstrapOffset,
		snapshotResponses: []fakeSnapshotResponse{{contracts: decodedGenesisSnapshot(t, ledger)}},
	}
	var clockSkew atomic.Int64
	served, err := newServedIndexWithClock(time.Minute, func() time.Time { return time.Now().Add(time.Duration(clockSkew.Load())) })
	require.NoError(t, err)
	timing := testWriterTiming()
	timing.StalledStreamTimeout = 20 * time.Millisecond
	timing.MaxConsecutiveReconnectAttempts = maxConsecutiveAttemptLimit
	stop := runTestWriterOn(t, reader, served, testLargeIndexByteCap, timing)
	require.Eventually(t, func() bool { return served.ServiceReadiness().IsReady() }, 5*time.Second, time.Millisecond)
	<-time.After(10 * timing.StalledStreamTimeout)
	_, subscribes := reader.callCounts()
	require.Equal(t, 1, subscribes, "a stream within the lag bound is not stalled")

	reader.mutex.Lock()
	reader.ledgerEnd = testBootstrapOffset + 1
	reader.mutex.Unlock()
	clockSkew.Store(int64(2 * time.Minute))
	require.Eventually(t, func() bool {
		_, subscribes := reader.callCounts()
		return subscribes >= 2
	}, 5*time.Second, time.Millisecond)
	require.NoError(t, stop())
}

func TestStreamStalled(t *testing.T) {
	served, clock := newTestServedIndex(t)
	index := newBootstrappedTestIndex(t, testLargeIndexByteCap)
	swapTestIndex(served, index, clock)
	timing := testWriterTiming()
	timing.StalledStreamTimeout = 30 * time.Second
	writer, err := NewCoreContractIndexWriter(&fakeCoreContractLedgerReader{}, served, testPartyID(t, testGuardianGovernance), testLargeIndexByteCap, timing, zap.NewNop())
	require.NoError(t, err)
	session := &ledgerFollowSession{writer: writer, followed: &followedIndex{index: index}}
	bootstrappedAt := clock.current
	served.recordLedgerEnd(testBootstrapOffset+10, clock.current)
	steps := []struct {
		name          string
		elapsed       time.Duration
		advanceOffset int64
		expectStalled bool
	}{
		{name: "lag at the bound", elapsed: testLedgerLagBound},
		{name: "first check above the bound", elapsed: testLedgerLagBound + time.Second},
		{name: "above the bound below the timeout", elapsed: testLedgerLagBound + 30*time.Second},
		{name: "above the bound at the timeout", elapsed: testLedgerLagBound + 31*time.Second, expectStalled: true},
		{name: "offset moved restarts the window", elapsed: testLedgerLagBound + 32*time.Second, advanceOffset: testBootstrapOffset + 1},
		{name: "above the bound below the restarted timeout", elapsed: testLedgerLagBound + 61*time.Second},
		{name: "above the bound at the restarted timeout", elapsed: testLedgerLagBound + 62*time.Second, expectStalled: true},
	}
	for _, step := range steps {
		clock.current = bootstrappedAt.Add(step.elapsed)
		if step.advanceOffset != 0 {
			require.NoError(t, index.AdvanceOffset(step.advanceOffset))
		}
		require.Equal(t, step.expectStalled, session.streamStalled(clock.current), step.name)
	}
}

func TestStreamAtTheLedgerEndIsNotStalled(t *testing.T) {
	served, clock := newTestServedIndex(t)
	index := newBootstrappedTestIndex(t, testLargeIndexByteCap)
	swapTestIndex(served, index, clock)
	timing := testWriterTiming()
	timing.StalledStreamTimeout = 30 * time.Second
	writer, err := NewCoreContractIndexWriter(&fakeCoreContractLedgerReader{}, served, testPartyID(t, testGuardianGovernance), testLargeIndexByteCap, timing, zap.NewNop())
	require.NoError(t, err)
	session := &ledgerFollowSession{writer: writer, followed: &followedIndex{index: index}}
	served.recordLedgerEnd(testBootstrapOffset, clock.current)
	for _, elapsed := range []time.Duration{testLedgerLagBound + time.Second, testLedgerLagBound + time.Hour} {
		clock.current = clock.current.Add(elapsed)
		require.Greater(t, served.ServiceReadiness().LedgerLag, testLedgerLagBound)
		require.Zero(t, served.LedgerOffsetsBehind())
		require.False(t, session.streamStalled(clock.current), "failed ledger end reads on a quiet ledger do not stall the stream")
	}
}

func TestReadObservedLedgerEndTimesTheReadAtItsStart(t *testing.T) {
	reader := &fakeCoreContractLedgerReader{ledgerEnd: testBootstrapOffset, ledgerEndReadDuration: 20 * time.Millisecond}
	served, _ := newTestServedIndex(t)
	writer, err := NewCoreContractIndexWriter(reader, served, testPartyID(t, testGuardianGovernance), testLargeIndexByteCap, testWriterTiming(), zap.NewNop())
	require.NoError(t, err)
	observed, err := writer.readObservedLedgerEnd(context.Background())
	require.NoError(t, err)
	require.Equal(t, int64(testBootstrapOffset), observed.offset)
	reader.mutex.Lock()
	readStartedAt := reader.ledgerEndReadStartedAt
	reader.mutex.Unlock()
	require.False(t, observed.observedAt.After(readStartedAt), "observed at %s after the read start %s", observed.observedAt, readStartedAt)

	reader.ledgerEnd = -1
	_, err = writer.readObservedLedgerEnd(context.Background())
	require.ErrorContains(t, err, "negative ledger end")
	reader.ledgerEndErr = errors.New("unavailable")
	_, err = writer.readObservedLedgerEnd(context.Background())
	require.ErrorContains(t, err, "unavailable")
}

func TestRefusedAdvanceSuspendsTheLookupsOfTheIndex(t *testing.T) {
	ledger := testDecodingLedger()
	served, clock := newTestServedIndex(t)
	index := newBootstrappedTestIndex(t, testLargeIndexByteCap)
	swapTestIndex(served, index, clock)
	writer, err := NewCoreContractIndexWriter(&fakeCoreContractLedgerReader{}, served, testPartyID(t, testGuardianGovernance), testLargeIndexByteCap, testWriterTiming(), zap.NewNop())
	require.NoError(t, err)
	require.NoError(t, index.AdvanceOffset(testBootstrapOffset+5))
	_, err = index.LookupSingleton(cantonclient.CoreContractTemplateCoreState)
	require.NoError(t, err)

	rebuildReason, err := writer.applyAdvance(index, ledger.DecodeOffsetCheckpoint(t, testBootstrapOffset, testBootstrapOffset+1))
	require.Equal(t, IndexRebuildReasonIndexUpdateRefused, rebuildReason)
	require.ErrorIs(t, err, ErrOffsetNotIncreasing)
	_, err = index.LookupSingleton(cantonclient.CoreContractTemplateCoreState)
	requireIndexNotReady(t, err, IndexReadinessResnapshotDifference)
}

// endedTestSubscription is a subscription that failed with finalErr after its goroutine sent bufferedAdvances.
func endedTestSubscription(t *testing.T, finalErr error, bufferedAdvances ...cantonclient.CoreContractLedgerAdvance) (*cantonclient.Subscription, chan cantonclient.CoreContractLedgerAdvance) {
	t.Helper()
	advances := make(chan cantonclient.CoreContractLedgerAdvance, advanceChannelCapacity)
	for _, advance := range bufferedAdvances {
		advances <- advance
	}
	subscription := cantonclient.NewSubscription(func() {})
	if finalErr != nil {
		subscription.Fail(finalErr)
	}
	subscription.Close()
	<-subscription.Done()
	return subscription, advances
}

func TestApplyAdvancesBufferedBeforeDone(t *testing.T) {
	ledger := testDecodingLedger()
	coreState := decodedSingletonEvent(testGenesisSerialBase, cantonclient.CoreContractTemplateCoreState)
	unavailableErr := status.Error(codes.Unavailable, "connection reset")
	testCases := []struct {
		name                        string
		finalErr                    error
		bufferedAdvances            func(t *testing.T) []cantonclient.CoreContractLedgerAdvance
		expectedOffset              int64
		expectedAppliedAdvanceCount int
		expectedRebuildReason       IndexRebuildReason
	}{
		{
			name:     "every buffered advance applies before the reconnect",
			finalErr: unavailableErr,
			bufferedAdvances: func(t *testing.T) []cantonclient.CoreContractLedgerAdvance {
				return []cantonclient.CoreContractLedgerAdvance{
					ledger.DecodeTransaction(t, testBootstrapOffset, 11, cantonclienttest.Created(decodedReplayNodeEvent(1, ""))),
					ledger.DecodeOffsetCheckpoint(t, 11, 12),
					ledger.DecodeOffsetCheckpoint(t, 12, 13),
				}
			},
			expectedOffset: 13, expectedAppliedAdvanceCount: 3,
		},
		{
			name:     "stream ended without an error",
			finalErr: nil,
			bufferedAdvances: func(t *testing.T) []cantonclient.CoreContractLedgerAdvance {
				return []cantonclient.CoreContractLedgerAdvance{ledger.DecodeOffsetCheckpoint(t, testBootstrapOffset, 11)}
			},
			expectedOffset: 11, expectedAppliedAdvanceCount: 1,
		},
		{
			name:     "a pruned offset after the buffered advances rebuilds",
			finalErr: status.Error(codes.FailedPrecondition, "PARTICIPANT_PRUNED_DATA_ACCESSED(9,0): pruned"),
			bufferedAdvances: func(t *testing.T) []cantonclient.CoreContractLedgerAdvance {
				return []cantonclient.CoreContractLedgerAdvance{ledger.DecodeOffsetCheckpoint(t, testBootstrapOffset, 11)}
			},
			expectedOffset: 11, expectedAppliedAdvanceCount: 1, expectedRebuildReason: IndexRebuildReasonPrunedOffset,
		},
		{
			name:     "an inconsistent buffered advance stops the drain",
			finalErr: unavailableErr,
			bufferedAdvances: func(t *testing.T) []cantonclient.CoreContractLedgerAdvance {
				return []cantonclient.CoreContractLedgerAdvance{
					ledger.DecodeOffsetCheckpoint(t, testBootstrapOffset, 11),
					ledger.DecodeTransaction(t, 11, 12, cantonclienttest.Created(coreState)),
					ledger.DecodeOffsetCheckpoint(t, 12, 13),
				}
			},
			expectedOffset: 11, expectedAppliedAdvanceCount: 1, expectedRebuildReason: IndexRebuildReasonInconsistentIndex,
		},
	}
	for _, testCase := range testCases {
		t.Run(testCase.name, func(t *testing.T) {
			served, clock := newTestServedIndex(t)
			index, err := NewCoreContractIndex(testPartyID(t, testGuardianGovernance), testLargeIndexByteCap)
			require.NoError(t, err)
			require.NoError(t, index.ApplyBootstrapSnapshot(testBootstrapOffset, decodedGenesisSnapshot(t, ledger)))
			swapTestIndex(served, index, clock)
			writer, err := NewCoreContractIndexWriter(&fakeCoreContractLedgerReader{}, served, testPartyID(t, testGuardianGovernance), testLargeIndexByteCap, testWriterTiming(), zap.NewNop())
			require.NoError(t, err)
			session := &ledgerFollowSession{writer: writer, followed: &followedIndex{index: index}}
			subscription, advances := endedTestSubscription(t, testCase.finalErr, testCase.bufferedAdvances(t)...)

			outcome := session.applyAdvancesBufferedBeforeDone(subscription, advances, 0)
			require.Equal(t, testCase.expectedAppliedAdvanceCount, outcome.appliedAdvanceCount)
			require.Equal(t, testCase.expectedRebuildReason, outcome.rebuildReason)
			require.Error(t, outcome.err)
			if testCase.finalErr != nil && testCase.expectedRebuildReason != IndexRebuildReasonInconsistentIndex {
				require.ErrorIs(t, outcome.err, testCase.finalErr)
			}
			require.Equal(t, testCase.expectedOffset, index.Status().Offset)
		})
	}
	require.Panics(t, func() {
		served, _ := newTestServedIndex(t)
		writer, err := NewCoreContractIndexWriter(&fakeCoreContractLedgerReader{}, served, testPartyID(t, testGuardianGovernance), testLargeIndexByteCap, testWriterTiming(), zap.NewNop())
		require.NoError(t, err)
		session := &ledgerFollowSession{writer: writer, followed: &followedIndex{index: newBootstrappedTestIndex(t, testLargeIndexByteCap)}}
		session.applyAdvancesBufferedBeforeDone(cantonclient.NewSubscription(func() {}), make(chan cantonclient.CoreContractLedgerAdvance), 0)
	})
}

func TestWriterLeavesTheCountTruncatedIndexWhenTheLedgerShrinks(t *testing.T) {
	ledger := testDecodingLedger()
	reader := &fakeCoreContractLedgerReader{
		ledgerEnd: testBootstrapOffset,
		snapshotResponses: []fakeSnapshotResponse{
			{contracts: decodedGenesisSnapshot(t, ledger, decodedReplayNodesAboveTheSnapshotLimit()...)},
			{contracts: decodedGenesisSnapshot(t, ledger, decodedReplayNodeEvent(2, ""))},
		},
	}
	timing := testWriterTiming()
	timing.ResnapshotInterval = 20 * time.Millisecond
	rebuildsBefore := rebuildCount(IndexRebuildReasonByteCapRetry)
	served, stop := runTestWriter(t, reader, testReplayNodeLimitByteCapBytes, timing)
	require.Eventually(t, func() bool {
		return served.ServiceReadiness().Status() == ServiceReadinessStatusReadyByteCapReached
	}, 5*time.Second, time.Millisecond)
	status, _ := served.IndexStatus()
	require.True(t, status.ReplayNodesDroppedAtByteCap)

	requireReadinessStaysReadyUntil(t, served, func() bool { return served.ServiceReadiness().Status() == ServiceReadinessStatusReady })
	status, _ = served.IndexStatus()
	require.False(t, status.ReplayNodesDroppedAtByteCap)
	require.Equal(t, 1, status.LiveReplayNodeCount)
	result, err := served.LookupCoveringReplayNode(testPartyID(t, testConsumer), testReplayNamespace(t, testNamespace), testReplayDigest(t, testCoveredDigestHexText))
	require.NoError(t, err)
	require.Equal(t, testContractIDHex(2), result.Contract().ContractID().String())
	require.NoError(t, stop())
	require.Equal(t, rebuildsBefore+1, rebuildCount(IndexRebuildReasonByteCapRetry))
}

func requireEventuallyServiceStatus(t *testing.T, served *ServedIndex, expectedStatus ServiceReadinessStatus) {
	t.Helper()
	require.Eventually(t, func() bool { return served.ServiceReadiness().Status() == expectedStatus }, 5*time.Second, time.Millisecond, "status %s", expectedStatus)
}

func TestWriterReportsAReadPartyThatLosesItsHosting(t *testing.T) {
	ledger := testDecodingLedger()
	reader := &fakeCoreContractLedgerReader{
		ledgerEnd:         testBootstrapOffset,
		snapshotResponses: []fakeSnapshotResponse{{contracts: decodedGenesisSnapshot(t, ledger, decodedReplayNodeEvent(1, ""))}},
	}
	served, stop := runTestWriter(t, reader, testLargeIndexByteCap, testWriterTiming())
	requireEventuallyServiceStatus(t, served, ServiceReadinessStatusReady)

	reader.setReadPartyHosting(true, nil)
	requireEventuallyServiceStatus(t, served, ServiceReadinessStatusReadPartyNotHosted)
	require.False(t, served.ServiceReadiness().IsReady())

	reader.setReadPartyHosting(false, nil)
	requireEventuallyServiceStatus(t, served, ServiceReadinessStatusReady)
	require.NoError(t, stop())
	snapshotReads, _ := reader.callCounts()
	require.Equal(t, 1, snapshotReads, "a hosting change alone does not rebuild the index")
}

func TestWriterKeepsTheHostingStateWhenTheHostingReadFails(t *testing.T) {
	ledger := testDecodingLedger()
	reader := &fakeCoreContractLedgerReader{
		ledgerEnd:         testBootstrapOffset,
		snapshotResponses: []fakeSnapshotResponse{{contracts: decodedGenesisSnapshot(t, ledger, decodedReplayNodeEvent(1, ""))}},
	}
	served, stop := runTestWriter(t, reader, testLargeIndexByteCap, testWriterTiming())
	requireEventuallyServiceStatus(t, served, ServiceReadinessStatusReady)
	reader.setReadPartyHosting(true, nil)
	requireEventuallyServiceStatus(t, served, ServiceReadinessStatusReadPartyNotHosted)

	readsBeforeFailure := reader.readPartyHostingReadCount()
	reader.setReadPartyHosting(false, errors.New("unavailable"))
	require.Eventually(t, func() bool { return reader.readPartyHostingReadCount() >= readsBeforeFailure+5 }, 5*time.Second, time.Millisecond)
	require.Equal(t, ServiceReadinessStatusReadPartyNotHosted, served.ServiceReadiness().Status())

	reader.setReadPartyHosting(false, nil)
	requireEventuallyServiceStatus(t, served, ServiceReadinessStatusReady)
	require.NoError(t, stop())
}

func TestWriterBootstrapOnAParticipantThatDoesNotHostTheReadParty(t *testing.T) {
	ledger := testDecodingLedger()
	reader := &fakeCoreContractLedgerReader{
		ledgerEnd:         testBootstrapOffset,
		snapshotResponses: []fakeSnapshotResponse{{contracts: decodedGenesisSnapshot(t, ledger, decodedReplayNodeEvent(1, ""))}},
		readPartyUnhosted: true,
	}
	served, stop := runTestWriter(t, reader, testLargeIndexByteCap, testWriterTiming())
	require.Eventually(t, func() bool { _, hasIndex := served.IndexStatus(); return hasIndex }, 5*time.Second, time.Millisecond)
	require.Equal(t, ServiceReadinessStatusReadPartyNotHosted, served.ServiceReadiness().Status())
	require.NoError(t, stop())
}

func TestWriterBuildFailsWhenTheBootstrapHostingReadFails(t *testing.T) {
	ledger := testDecodingLedger()
	reader := &fakeCoreContractLedgerReader{
		ledgerEnd:           testBootstrapOffset,
		snapshotResponses:   []fakeSnapshotResponse{{contracts: decodedGenesisSnapshot(t, ledger, decodedReplayNodeEvent(1, ""))}},
		readPartyHostingErr: errors.New("unavailable"),
	}
	served, stop := runTestWriter(t, reader, testLargeIndexByteCap, testWriterTiming())
	require.Eventually(t, func() bool { return reader.readPartyHostingReadCount() >= 2 }, 5*time.Second, time.Millisecond)
	_, hasIndex := served.IndexStatus()
	require.False(t, hasIndex)
	require.Equal(t, ServiceReadinessStatusBootstrapping, served.ServiceReadiness().Status())
	_ = stop()
}
