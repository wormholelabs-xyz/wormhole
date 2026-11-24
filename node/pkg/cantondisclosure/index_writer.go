package cantondisclosure

import (
	"context"
	"errors"
	"fmt"
	"io"
	"time"

	"go.uber.org/zap"
	"google.golang.org/grpc/codes"
	"google.golang.org/grpc/status"

	"github.com/certusone/wormhole/node/pkg/cantonclient"
)

// CoreContractLedgerReader is the Ledger API surface of the index writer. cantonclient.CoreContractReader implements it.
type CoreContractLedgerReader interface {
	GetLedgerEnd(ctx context.Context) (int64, error)
	ReadActiveCoreContracts(ctx context.Context, activeAtOffset int64, maxContracts int) ([]cantonclient.CreatedCoreContract, error)
	SubscribeCoreContractUpdates(ctx context.Context, beginExclusive int64, out chan<- cantonclient.CoreContractLedgerAdvance) (*cantonclient.Subscription, error)
}

// Bounds of the writer timing.
const (
	minResnapshotInterval      = time.Minute
	maxResnapshotInterval      = 7 * 24 * time.Hour
	maxConsecutiveAttemptLimit = 64
	// advanceChannelCapacity bounds the advances that the stream goroutine buffers while the writer applies.
	advanceChannelCapacity = 64
	// subscriptionStopTimeout bounds the wait for the stream goroutine after Unsubscribe.
	subscriptionStopTimeout = 10 * time.Second
)

// ProductionResnapshotInterval is the default interval of the periodic snapshot comparison.
const ProductionResnapshotInterval = time.Hour

// IndexWriterTiming holds the intervals, delays and attempt limits of the writer.
type IndexWriterTiming struct {
	LedgerEndPollInterval time.Duration
	ResnapshotInterval    time.Duration
	SnapshotReadTimeout   time.Duration

	ReconnectInitialDelay           time.Duration
	ReconnectMaxDelay               time.Duration
	MaxConsecutiveReconnectAttempts int

	RebuildInitialDelay           time.Duration
	RebuildMaxDelay               time.Duration
	MaxConsecutiveRebuildAttempts int
	// StableFollowDuration is how long an index must follow the ledger before its rebuild resets the rebuild attempt count.
	StableFollowDuration time.Duration
}

// ProductionIndexWriterTiming is the production timing with resnapshotInterval.
func ProductionIndexWriterTiming(resnapshotInterval time.Duration) IndexWriterTiming {
	return IndexWriterTiming{
		LedgerEndPollInterval:           5 * time.Second,
		ResnapshotInterval:              resnapshotInterval,
		SnapshotReadTimeout:             15 * time.Minute,
		ReconnectInitialDelay:           500 * time.Millisecond,
		ReconnectMaxDelay:               30 * time.Second,
		MaxConsecutiveReconnectAttempts: 8,
		RebuildInitialDelay:             time.Second,
		RebuildMaxDelay:                 2 * time.Minute,
		MaxConsecutiveRebuildAttempts:   8,
		StableFollowDuration:            10 * time.Minute,
	}
}

// CheckProductionResnapshotInterval requires resnapshotInterval in [1m, 7d].
func CheckProductionResnapshotInterval(resnapshotInterval time.Duration) error {
	if resnapshotInterval < minResnapshotInterval || resnapshotInterval > maxResnapshotInterval {
		return fmt.Errorf("cantondisclosure: resnapshot interval %s is outside [%s, %s]", resnapshotInterval, minResnapshotInterval, maxResnapshotInterval)
	}
	return nil
}

func (timing IndexWriterTiming) check() error {
	durations := []struct {
		name     string
		duration time.Duration
	}{
		{"ledger end poll interval", timing.LedgerEndPollInterval},
		{"resnapshot interval", timing.ResnapshotInterval},
		{"snapshot read timeout", timing.SnapshotReadTimeout},
		{"reconnect initial delay", timing.ReconnectInitialDelay},
		{"reconnect maximum delay", timing.ReconnectMaxDelay},
		{"rebuild initial delay", timing.RebuildInitialDelay},
		{"rebuild maximum delay", timing.RebuildMaxDelay},
		{"stable follow duration", timing.StableFollowDuration},
	}
	for _, named := range durations {
		if named.duration <= 0 {
			return fmt.Errorf("cantondisclosure: %s %s is not positive", named.name, named.duration)
		}
	}
	if timing.ReconnectInitialDelay > timing.ReconnectMaxDelay {
		return errors.New("cantondisclosure: reconnect initial delay exceeds the maximum delay")
	}
	if timing.RebuildInitialDelay > timing.RebuildMaxDelay {
		return errors.New("cantondisclosure: rebuild initial delay exceeds the maximum delay")
	}
	for _, attemptLimit := range []int{timing.MaxConsecutiveReconnectAttempts, timing.MaxConsecutiveRebuildAttempts} {
		if attemptLimit < 1 || attemptLimit > maxConsecutiveAttemptLimit {
			return fmt.Errorf("cantondisclosure: attempt limit %d is outside [1, %d]", attemptLimit, maxConsecutiveAttemptLimit)
		}
	}
	return nil
}

// backoffDelay is initialDelay doubled per earlier attempt, at most maxDelay. attempt starts at 1.
func backoffDelay(attempt int, initialDelay time.Duration, maxDelay time.Duration) time.Duration {
	if attempt < 1 {
		panic(fmt.Sprintf("cantondisclosure: backoff attempt %d is below 1", attempt))
	}
	delay := initialDelay
	for doubling := 1; doubling < attempt && delay < maxDelay; doubling++ {
		delay *= 2
	}
	return min(delay, maxDelay)
}

// waitForDelay is false when ctx ends first.
func waitForDelay(ctx context.Context, delay time.Duration) bool {
	timer := time.NewTimer(delay)
	defer timer.Stop()
	select {
	case <-ctx.Done():
		return false
	case <-timer.C:
		return true
	}
}

// CoreContractIndexWriter keeps the served index on the ledger: bootstrap, stream, reconnect, resnapshot and rebuild.
type CoreContractIndexWriter struct {
	reader             CoreContractLedgerReader
	served             *ServedIndex
	guardianGovernance cantonclient.PartyID
	byteCapBytes       int64
	timing             IndexWriterTiming
	logger             *zap.Logger
}

// NewCoreContractIndexWriter checks every parameter. One writer per ServedIndex.
func NewCoreContractIndexWriter(reader CoreContractLedgerReader, served *ServedIndex, guardianGovernance cantonclient.PartyID, byteCapBytes int64, timing IndexWriterTiming, logger *zap.Logger) (*CoreContractIndexWriter, error) {
	if reader == nil || served == nil || logger == nil {
		return nil, errors.New("cantondisclosure: index writer needs a reader, a served index and a logger")
	}
	if _, err := NewCoreContractIndex(guardianGovernance, byteCapBytes); err != nil {
		return nil, err
	}
	if err := timing.check(); err != nil {
		return nil, err
	}
	return &CoreContractIndexWriter{
		reader:             reader,
		served:             served,
		guardianGovernance: guardianGovernance,
		byteCapBytes:       byteCapBytes,
		timing:             timing,
		logger:             logger,
	}, nil
}

// maxSnapshotContracts is the largest contract count that fits the byte cap, at most MaxActiveCoreContractsPerSnapshot.
func (writer *CoreContractIndexWriter) maxSnapshotContracts() int {
	return int(min(writer.byteCapBytes/indexEntryOverheadBytes, int64(cantonclient.MaxActiveCoreContractsPerSnapshot)))
}

// ErrRebuildAttemptsExhausted: the writer stopped after MaxConsecutiveRebuildAttempts unstable rebuilds.
var ErrRebuildAttemptsExhausted = errors.New("cantondisclosure: index rebuild attempts exhausted")

// Run builds and follows indexes until ctx ends. It returns nil when ctx ends, and
// ErrRebuildAttemptsExhausted after MaxConsecutiveRebuildAttempts consecutive builds that failed
// or followed the ledger for less than StableFollowDuration.
func (writer *CoreContractIndexWriter) Run(ctx context.Context) error {
	consecutiveUnstableBuilds := 0
	// Runs until ctx ends; consecutiveUnstableBuilds bounds the failures.
	for {
		if consecutiveUnstableBuilds > 0 {
			if consecutiveUnstableBuilds >= writer.timing.MaxConsecutiveRebuildAttempts {
				return fmt.Errorf("%w: %d consecutive", ErrRebuildAttemptsExhausted, consecutiveUnstableBuilds)
			}
			if !waitForDelay(ctx, backoffDelay(consecutiveUnstableBuilds, writer.timing.RebuildInitialDelay, writer.timing.RebuildMaxDelay)) {
				return nil
			}
		}
		index, err := writer.buildIndex(ctx)
		if ctx.Err() != nil {
			return nil
		}
		if err != nil {
			indexBuildFailuresCounter.Inc()
			consecutiveUnstableBuilds++
			writer.logger.Error("core disclosure index build failed", zap.Int("consecutiveUnstableBuilds", consecutiveUnstableBuilds), zap.Error(err))
			continue
		}
		followStartedAt := time.Now()
		rebuildReason, err := writer.followLedger(ctx, index)
		if ctx.Err() != nil {
			return nil
		}
		indexRebuildsCounter.WithLabelValues(string(rebuildReason)).Inc()
		if time.Since(followStartedAt) >= writer.timing.StableFollowDuration {
			consecutiveUnstableBuilds = 0
		} else {
			consecutiveUnstableBuilds++
		}
		writer.logger.Warn("rebuilding the core disclosure index", zap.String("reason", string(rebuildReason)), zap.Int64("indexOffset", index.Status().Offset), zap.Int("consecutiveUnstableBuilds", consecutiveUnstableBuilds), zap.Error(err))
	}
}

// buildIndex bootstraps a new index from the active contracts at the ledger end and swaps it in.
func (writer *CoreContractIndexWriter) buildIndex(ctx context.Context) (*CoreContractIndex, error) {
	ledgerEnd, err := writer.reader.GetLedgerEnd(ctx)
	if err != nil {
		return nil, err
	}
	observedAt := time.Now()
	if ledgerEnd < 0 {
		return nil, fmt.Errorf("cantondisclosure: negative ledger end %d", ledgerEnd)
	}
	snapshotCtx, cancel := context.WithTimeout(ctx, writer.timing.SnapshotReadTimeout)
	defer cancel()
	snapshot, err := writer.reader.ReadActiveCoreContracts(snapshotCtx, ledgerEnd, writer.maxSnapshotContracts())
	if err != nil {
		return nil, err
	}
	index, err := NewCoreContractIndex(writer.guardianGovernance, writer.byteCapBytes)
	if err != nil {
		return nil, err
	}
	if err := index.ApplyBootstrapSnapshot(ledgerEnd, snapshot); err != nil {
		return nil, err
	}
	status := index.Status()
	if status.Readiness == IndexReadinessInconsistent || status.Readiness == IndexReadinessBootstrapping {
		return nil, fmt.Errorf("cantondisclosure: bootstrap at offset %d left readiness %s", ledgerEnd, status.Readiness)
	}
	if status.Offset != ledgerEnd {
		panic("cantondisclosure: bootstrapped index offset is not the snapshot offset")
	}
	writer.served.swapIndex(index)
	writer.served.recordLedgerEnd(ledgerEnd, observedAt)
	publishIndexMetrics(writer.served)
	writer.logger.Info("core disclosure index bootstrapped", zap.Int64("offset", ledgerEnd), zap.Int("liveContracts", status.LiveContractCount), zap.Int64("usedBytes", status.UsedBytes), zap.String("readiness", status.Readiness.String()))
	return index, nil
}

// streamFailureAction is what the writer does after the update stream fails.
type streamFailureAction int

const (
	streamFailureActionReconnect streamFailureAction = iota + 1
	streamFailureActionRebuild
)

// classifyStreamFailure rebuilds on a pruned offset, any other FAILED_PRECONDITION and a decoder refusal; it reconnects otherwise.
func classifyStreamFailure(err error) (streamFailureAction, IndexRebuildReason) {
	if err == nil {
		panic("cantondisclosure: classify a nil stream failure")
	}
	if cantonclient.IsPrunedOffsetError(err) {
		return streamFailureActionRebuild, IndexRebuildReasonPrunedOffset
	}
	if errors.Is(err, cantonclient.ErrCoreContractStreamDecode) {
		return streamFailureActionRebuild, IndexRebuildReasonStreamDecodeFailure
	}
	if errors.Is(err, io.EOF) {
		return streamFailureActionReconnect, ""
	}
	if grpcStatus, hasStatus := status.FromError(err); hasStatus && grpcStatus.Code() == codes.FailedPrecondition {
		return streamFailureActionRebuild, IndexRebuildReasonFailedPrecondition
	}
	return streamFailureActionReconnect, ""
}

// followLedger subscribes from the index offset and applies every advance until a rebuild is due or ctx ends.
func (writer *CoreContractIndexWriter) followLedger(ctx context.Context, index *CoreContractIndex) (IndexRebuildReason, error) {
	consecutiveReconnectAttempts := 0
	resnapshotTicker := time.NewTicker(writer.timing.ResnapshotInterval)
	defer resnapshotTicker.Stop()
	ledgerEndTicker := time.NewTicker(writer.timing.LedgerEndPollInterval)
	defer ledgerEndTicker.Stop()
	// Runs until ctx ends or a rebuild is due; consecutiveReconnectAttempts bounds the failures.
	for {
		if consecutiveReconnectAttempts > 0 {
			if consecutiveReconnectAttempts > writer.timing.MaxConsecutiveReconnectAttempts {
				return IndexRebuildReasonReconnectsExhausted, nil
			}
			streamReconnectsCounter.Inc()
			if !waitForDelay(ctx, backoffDelay(consecutiveReconnectAttempts, writer.timing.ReconnectInitialDelay, writer.timing.ReconnectMaxDelay)) {
				return "", ctx.Err()
			}
		}
		advances := make(chan cantonclient.CoreContractLedgerAdvance, advanceChannelCapacity)
		subscription, err := writer.reader.SubscribeCoreContractUpdates(ctx, index.Status().Offset, advances)
		if err != nil {
			if ctx.Err() != nil {
				return "", ctx.Err()
			}
			if action, rebuildReason := classifyStreamFailure(err); action == streamFailureActionRebuild {
				return rebuildReason, err
			}
			consecutiveReconnectAttempts++
			writer.logger.Warn("core contract update stream subscribe failed", zap.Int("consecutiveReconnectAttempts", consecutiveReconnectAttempts), zap.Error(err))
			continue
		}
		writer.served.setStreamConnected(true)
		publishIndexMetrics(writer.served)
		outcome := writer.consumeSubscription(ctx, index, subscription, advances, resnapshotTicker.C, ledgerEndTicker.C)
		subscription.Unsubscribe()
		writer.waitForSubscriptionStop(subscription)
		writer.served.setStreamConnected(false)
		publishIndexMetrics(writer.served)
		if ctx.Err() != nil {
			return "", ctx.Err()
		}
		if outcome.rebuildReason != "" {
			return outcome.rebuildReason, outcome.err
		}
		if outcome.appliedAdvanceCount > 0 {
			consecutiveReconnectAttempts = 0
		}
		consecutiveReconnectAttempts++
		writer.logger.Warn("core contract update stream failed", zap.Int64("indexOffset", index.Status().Offset), zap.Int("consecutiveReconnectAttempts", consecutiveReconnectAttempts), zap.Error(outcome.err))
	}
}

func (writer *CoreContractIndexWriter) waitForSubscriptionStop(subscription *cantonclient.Subscription) {
	timer := time.NewTimer(subscriptionStopTimeout)
	defer timer.Stop()
	select {
	case <-subscription.Done():
	case <-timer.C:
		writer.logger.Error("core contract update stream goroutine did not stop", zap.Duration("timeout", subscriptionStopTimeout))
	}
}

// subscriptionOutcome: a non-empty rebuildReason ends the index; otherwise err is a stream failure to reconnect after.
type subscriptionOutcome struct {
	rebuildReason       IndexRebuildReason
	err                 error
	appliedAdvanceCount int
}

// consumeSubscription applies advances, polls the ledger end and runs the resnapshot until the subscription ends.
func (writer *CoreContractIndexWriter) consumeSubscription(
	ctx context.Context,
	index *CoreContractIndex,
	subscription *cantonclient.Subscription,
	advances <-chan cantonclient.CoreContractLedgerAdvance,
	resnapshotTicks <-chan time.Time,
	ledgerEndTicks <-chan time.Time,
) subscriptionOutcome {
	outcome := subscriptionOutcome{}
	// Runs until ctx ends, the subscription ends or a rebuild is due.
	for {
		select {
		case <-ctx.Done():
			return outcome
		case advance := <-advances:
			if rebuildReason, err := writer.applyAdvance(index, advance); rebuildReason != "" {
				return subscriptionOutcome{rebuildReason: rebuildReason, err: err, appliedAdvanceCount: outcome.appliedAdvanceCount}
			}
			outcome.appliedAdvanceCount++
		case <-subscription.Done():
			// The stream goroutine sends every decoded advance before it fails.
			for bufferedAdvanceCount := len(advances); bufferedAdvanceCount > 0; bufferedAdvanceCount-- {
				if rebuildReason, err := writer.applyAdvance(index, <-advances); rebuildReason != "" {
					return subscriptionOutcome{rebuildReason: rebuildReason, err: err, appliedAdvanceCount: outcome.appliedAdvanceCount}
				}
				outcome.appliedAdvanceCount++
			}
			streamErr := errors.New("cantondisclosure: core contract update stream ended without an error")
			select {
			case subscriptionErr := <-subscription.Err():
				streamErr = subscriptionErr
			default:
			}
			if action, rebuildReason := classifyStreamFailure(streamErr); action == streamFailureActionRebuild {
				outcome.rebuildReason = rebuildReason
			}
			outcome.err = streamErr
			return outcome
		case <-ledgerEndTicks:
			writer.recordLedgerEnd(ctx)
		case <-resnapshotTicks:
			if rebuildReason, err := writer.resnapshotAndCompare(ctx, index); rebuildReason != "" {
				return subscriptionOutcome{rebuildReason: rebuildReason, err: err, appliedAdvanceCount: outcome.appliedAdvanceCount}
			}
		}
	}
}

// applyAdvance returns a rebuild reason when the index refuses the advance or becomes inconsistent.
func (writer *CoreContractIndexWriter) applyAdvance(index *CoreContractIndex, advance cantonclient.CoreContractLedgerAdvance) (IndexRebuildReason, error) {
	offsetBefore := index.Status().Offset
	var err error
	if advance.IsOffsetCheckpoint() {
		err = index.AdvanceOffset(advance.Offset())
	} else {
		err = index.ApplyTransaction(advance.Offset(), advance.Updates())
	}
	publishIndexMetrics(writer.served)
	status := index.Status()
	if status.Readiness == IndexReadinessInconsistent {
		return IndexRebuildReasonInconsistentIndex, err
	}
	if err != nil {
		return IndexRebuildReasonIndexUpdateRefused, err
	}
	if status.Offset != advance.Offset() || status.Offset <= offsetBefore {
		panic("cantondisclosure: applied advance did not move the index offset to the advance offset")
	}
	return "", nil
}

func (writer *CoreContractIndexWriter) recordLedgerEnd(ctx context.Context) {
	ledgerEnd, err := writer.reader.GetLedgerEnd(ctx)
	if err != nil {
		ledgerEndReadFailuresCounter.Inc()
		writer.logger.Warn("ledger end read failed", zap.Error(err))
		return
	}
	writer.served.recordLedgerEnd(ledgerEnd, time.Now())
	publishIndexMetrics(writer.served)
}

// resnapshotAndCompare reads the active contracts at the index offset and compares them with the index.
// Applies wait meanwhile. At the byte cap it requests a rebuild instead.
func (writer *CoreContractIndexWriter) resnapshotAndCompare(ctx context.Context, index *CoreContractIndex) (IndexRebuildReason, error) {
	indexStatus := index.Status()
	if indexStatus.ReplayNodesDroppedAtByteCap {
		return IndexRebuildReasonByteCapRetry, nil
	}
	snapshotCtx, cancel := context.WithTimeout(ctx, writer.timing.SnapshotReadTimeout)
	defer cancel()
	snapshot, err := writer.reader.ReadActiveCoreContracts(snapshotCtx, indexStatus.Offset, writer.maxSnapshotContracts())
	if err != nil {
		if ctx.Err() != nil {
			return "", nil
		}
		if cantonclient.IsPrunedOffsetError(err) {
			return IndexRebuildReasonPrunedOffset, err
		}
		resnapshotFailuresCounter.Inc()
		writer.logger.Warn("resnapshot read failed", zap.Int64("indexOffset", indexStatus.Offset), zap.Error(err))
		return "", nil
	}
	comparison, err := index.CompareWithSnapshot(indexStatus.Offset, snapshot)
	if err != nil {
		resnapshotFailuresCounter.Inc()
		writer.logger.Error("resnapshot comparison failed", zap.Int64("indexOffset", indexStatus.Offset), zap.Error(err))
		return IndexRebuildReasonResnapshotDifference, err
	}
	if comparison.Offset() != indexStatus.Offset {
		panic("cantondisclosure: snapshot comparison at another offset than the index")
	}
	recordSnapshotComparisonMetrics(comparison)
	if comparison.MatchesIndex() {
		writer.logger.Info("resnapshot matches the index", zap.Int64("indexOffset", indexStatus.Offset), zap.Int("snapshotContracts", len(snapshot)))
		return "", nil
	}
	writer.logger.Warn("resnapshot differs from the index",
		zap.Int64("indexOffset", indexStatus.Offset),
		zap.Int("missingFromIndex", comparison.MissingFromIndexCount()),
		zap.Int("staleInIndex", comparison.StaleInIndexCount()),
		zap.Int("contentMismatch", comparison.ContentMismatchCount()),
		zap.Any("firstDifferences", firstSnapshotDifferencesForLog(comparison)),
	)
	if comparison.MissingFromIndexCount() > 0 || comparison.StaleInIndexCount() > 0 {
		return IndexRebuildReasonResnapshotDifference, nil
	}
	return "", nil
}

// maxLoggedSnapshotDifferences bounds the differences in one log line.
const maxLoggedSnapshotDifferences = 16

func firstSnapshotDifferencesForLog(comparison SnapshotComparison) []string {
	reportedDifferences := comparison.ReportedDifferences()
	loggedDifferences := make([]string, 0, min(len(reportedDifferences), maxLoggedSnapshotDifferences))
	for _, difference := range reportedDifferences[:min(len(reportedDifferences), maxLoggedSnapshotDifferences)] {
		loggedDifferences = append(loggedDifferences, difference.Kind.String()+" "+difference.Template.String()+" "+difference.ContractID.String())
	}
	return loggedDifferences
}
