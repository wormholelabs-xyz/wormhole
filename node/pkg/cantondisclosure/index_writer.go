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
	ReadPartyIsHosted(ctx context.Context) (bool, error)
	ReadActiveCoreContracts(ctx context.Context, activeAtOffset int64, replayNodeLimit cantonclient.ReplayNodeSnapshotLimit) (cantonclient.ActiveCoreContracts, error)
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
	// backgroundStopTimeout bounds the wait for the ledger end poller and a replacement build after cancellation.
	backgroundStopTimeout = 10 * time.Second
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
	// StalledStreamTimeout is how long the ledger lag stays above the bound, with no index offset progress,
	// before the writer reconnects an open stream.
	StalledStreamTimeout time.Duration
}

// ProductionIndexWriterTiming is the production timing with resnapshotInterval.
func ProductionIndexWriterTiming(resnapshotInterval time.Duration) IndexWriterTiming {
	return IndexWriterTiming{
		LedgerEndPollInterval:           ProductionLedgerEndPollInterval,
		ResnapshotInterval:              resnapshotInterval,
		SnapshotReadTimeout:             15 * time.Minute,
		ReconnectInitialDelay:           500 * time.Millisecond,
		ReconnectMaxDelay:               30 * time.Second,
		MaxConsecutiveReconnectAttempts: 8,
		RebuildInitialDelay:             time.Second,
		RebuildMaxDelay:                 2 * time.Minute,
		MaxConsecutiveRebuildAttempts:   8,
		StableFollowDuration:            10 * time.Minute,
		StalledStreamTimeout:            2 * time.Minute,
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
		{"stalled stream timeout", timing.StalledStreamTimeout},
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
	// snapshotReplayNodeLimit charges each ReplayNode as the index does, so a snapshot holds at most byteCapBytes of nodes.
	snapshotReplayNodeLimit cantonclient.ReplayNodeSnapshotLimit
	timing                  IndexWriterTiming
	logger                  *zap.Logger
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
	if 2*timing.LedgerEndPollInterval > served.ledgerLagBound {
		return nil, fmt.Errorf("cantondisclosure: ledger end poll interval %s is above half the ledger lag bound %s", timing.LedgerEndPollInterval, served.ledgerLagBound)
	}
	maxSnapshotReplayNodeCount := int(min(byteCapBytes/indexEntryOverheadBytes, int64(cantonclient.MaxActiveCoreContractsPerSnapshot)))
	snapshotReplayNodeLimit, err := cantonclient.NewReplayNodeSnapshotLimit(maxSnapshotReplayNodeCount, byteCapBytes, indexEntryOverheadBytes)
	if err != nil {
		return nil, fmt.Errorf("cantondisclosure: snapshot replay node limit: %w", err)
	}
	return &CoreContractIndexWriter{
		reader:                  reader,
		served:                  served,
		guardianGovernance:      guardianGovernance,
		byteCapBytes:            byteCapBytes,
		snapshotReplayNodeLimit: snapshotReplayNodeLimit,
		timing:                  timing,
		logger:                  logger,
	}, nil
}

// ErrRebuildAttemptsExhausted: the writer stopped after MaxConsecutiveRebuildAttempts unstable rebuilds.
var ErrRebuildAttemptsExhausted = errors.New("cantondisclosure: index rebuild attempts exhausted")

// errStalledStream: the ledger lag stayed above the bound for StalledStreamTimeout without index offset progress.
var errStalledStream = errors.New("cantondisclosure: core contract update stream stalled")

// followedIndex is the index that the writer applies advances to. With swapPending, the served index is
// the index it replaces, and the writer swaps when the followed offset reaches the served offset.
type followedIndex struct {
	index              *CoreContractIndex
	bootstrapLedgerEnd observedLedgerEnd
	// readPartyRehostCount is ServedIndex.readPartyRehostCount before the bootstrap snapshot.
	readPartyRehostCount uint64
	swapPending          bool
}

// Run builds and follows indexes until ctx ends. It returns nil when ctx ends, and
// ErrRebuildAttemptsExhausted after MaxConsecutiveRebuildAttempts consecutive builds that failed
// or followed the ledger for less than StableFollowDuration. A byte cap retry counts as stable.
// A ledger end poller runs beside it for the whole call.
func (writer *CoreContractIndexWriter) Run(ctx context.Context) error {
	pollerCtx, cancelPoller := context.WithCancel(ctx)
	pollerDone := make(chan struct{})
	go func() {
		defer close(pollerDone)
		writer.pollLedgerEnd(pollerCtx)
	}()
	defer func() {
		cancelPoller()
		writer.waitForBackgroundStop(pollerDone, "ledger end poller")
	}()

	consecutiveUnstableBuilds := 0
	var replacement *followedIndex
	// Runs until ctx ends; consecutiveUnstableBuilds bounds the failures.
	for {
		if consecutiveUnstableBuilds > 0 {
			if consecutiveUnstableBuilds >= writer.timing.MaxConsecutiveRebuildAttempts {
				return fmt.Errorf("%w: %d consecutive", ErrRebuildAttemptsExhausted, consecutiveUnstableBuilds)
			}
			// A ready replacement is caught up and needs no snapshot read, so it follows at once.
			if replacement == nil && !waitForDelay(ctx, backoffDelay(consecutiveUnstableBuilds, writer.timing.RebuildInitialDelay, writer.timing.RebuildMaxDelay)) {
				return nil
			}
		}
		followed := replacement
		replacement = nil
		if followed == nil {
			built, err := writer.buildAndSwapIndex(ctx)
			if ctx.Err() != nil {
				return nil
			}
			if err != nil {
				indexBuildFailuresCounter.Inc()
				consecutiveUnstableBuilds++
				writer.logger.Error("core disclosure index build failed", zap.Int("consecutiveUnstableBuilds", consecutiveUnstableBuilds), zap.Error(err))
				continue
			}
			followed = &built
		}
		followStartedAt := time.Now()
		outcome := writer.followLedger(ctx, followed)
		if ctx.Err() != nil {
			return nil
		}
		if outcome.rebuildReason == "" {
			panic("cantondisclosure: follow ended without a rebuild reason")
		}
		replacement = outcome.replacement
		indexRebuildsCounter.WithLabelValues(string(outcome.rebuildReason)).Inc()
		if outcome.rebuildReason == IndexRebuildReasonByteCapRetry || time.Since(followStartedAt) >= writer.timing.StableFollowDuration {
			consecutiveUnstableBuilds = 0
		} else {
			consecutiveUnstableBuilds++
		}
		writer.logger.Warn("rebuilding the core disclosure index", zap.String("reason", string(outcome.rebuildReason)), zap.Int64("indexOffset", followed.index.Status().Offset), zap.Bool("replacementReady", replacement != nil), zap.Int("consecutiveUnstableBuilds", consecutiveUnstableBuilds), zap.Error(outcome.err))
	}
}

func (writer *CoreContractIndexWriter) waitForBackgroundStop(done <-chan struct{}, name string) {
	timer := time.NewTimer(backgroundStopTimeout)
	defer timer.Stop()
	select {
	case <-done:
	case <-timer.C:
		writer.logger.Error("core disclosure background task did not stop", zap.String("task", name), zap.Duration("timeout", backgroundStopTimeout))
	}
}

// pollLedgerEnd records a ledger end read every LedgerEndPollInterval until ctx ends, also while the writer
// builds, resnapshots or reconnects.
func (writer *CoreContractIndexWriter) pollLedgerEnd(ctx context.Context) {
	ticker := time.NewTicker(writer.timing.LedgerEndPollInterval)
	defer ticker.Stop()
	// Runs until ctx ends.
	for {
		select {
		case <-ctx.Done():
			return
		case <-ticker.C:
			observed, err := writer.readObservedLedgerEnd(ctx)
			if ctx.Err() != nil {
				return
			}
			if err != nil {
				ledgerEndReadFailuresCounter.Inc()
				writer.logger.Warn("ledger end read failed", zap.Error(err))
			} else {
				writer.served.recordLedgerEnd(observed.offset, observed.observedAt)
			}
			writer.recordReadPartyHosting(ctx)
		}
	}
}

// readObservedLedgerEnd times the read at its start: the ledger end then was at most the returned offset,
// so the lag measured from that time is an upper bound.
func (writer *CoreContractIndexWriter) readObservedLedgerEnd(ctx context.Context) (observedLedgerEnd, error) {
	readStartedAt := time.Now()
	ledgerEnd, err := writer.reader.GetLedgerEnd(ctx)
	if err != nil {
		return observedLedgerEnd{}, err
	}
	if ledgerEnd < 0 {
		return observedLedgerEnd{}, fmt.Errorf("cantondisclosure: negative ledger end %d", ledgerEnd)
	}
	return observedLedgerEnd{offset: ledgerEnd, observedAt: readStartedAt}, nil
}

// readBootstrappedIndex bootstraps a new index from the active contracts at the ledger end. The caller serves it.
func (writer *CoreContractIndexWriter) readBootstrappedIndex(ctx context.Context) (followedIndex, error) {
	readPartyRehostCount := writer.served.currentReadPartyRehostCount()
	bootstrapLedgerEnd, err := writer.readObservedLedgerEnd(ctx)
	if err != nil {
		return followedIndex{}, err
	}
	ledgerEnd := bootstrapLedgerEnd.offset
	snapshotCtx, cancel := context.WithTimeout(ctx, writer.timing.SnapshotReadTimeout)
	defer cancel()
	snapshot, err := writer.reader.ReadActiveCoreContracts(snapshotCtx, ledgerEnd, writer.snapshotReplayNodeLimit)
	if err != nil {
		return followedIndex{}, err
	}
	index, err := NewCoreContractIndex(writer.guardianGovernance, writer.byteCapBytes)
	if err != nil {
		return followedIndex{}, err
	}
	if snapshot.ReplayNodesTruncated() {
		writer.logger.Warn("snapshot has more ReplayNodes than the byte cap admits; serving singletons only", zap.Int64("offset", ledgerEnd), zap.Int("maxReplayNodeCount", writer.snapshotReplayNodeLimit.MaxReplayNodeCount()), zap.Int64("maxReplayNodeBytes", writer.snapshotReplayNodeLimit.MaxChargedBytes()))
		err = index.ApplyBootstrapSnapshotWithoutReplayNodes(ledgerEnd, snapshot.Contracts())
	} else {
		err = index.ApplyBootstrapSnapshot(ledgerEnd, snapshot.Contracts())
	}
	if err != nil {
		return followedIndex{}, err
	}
	status := index.Status()
	if status.Readiness == IndexReadinessInconsistent || status.Readiness == IndexReadinessBootstrapping {
		return followedIndex{}, fmt.Errorf("cantondisclosure: bootstrap at offset %d left readiness %s", ledgerEnd, status.Readiness)
	}
	if status.Offset != ledgerEnd {
		panic("cantondisclosure: bootstrapped index offset is not the snapshot offset")
	}
	writer.logger.Info("core disclosure index bootstrapped", zap.Int64("offset", ledgerEnd), zap.Int("liveContracts", status.LiveContractCount), zap.Int64("usedBytes", status.UsedBytes), zap.String("readiness", status.Readiness.String()))
	return followedIndex{index: index, bootstrapLedgerEnd: bootstrapLedgerEnd, readPartyRehostCount: readPartyRehostCount}, nil
}

// buildAndSwapIndex bootstraps a new index and serves it at once. The ledger end is at or above every served offset.
func (writer *CoreContractIndexWriter) buildAndSwapIndex(ctx context.Context) (followedIndex, error) {
	built, err := writer.readBootstrappedIndex(ctx)
	if err != nil {
		return followedIndex{}, err
	}
	servedOffset := writer.served.indexOffset()
	if built.bootstrapLedgerEnd.offset < servedOffset {
		return followedIndex{}, fmt.Errorf("cantondisclosure: ledger end %d is below the served index offset %d", built.bootstrapLedgerEnd.offset, servedOffset)
	}
	readPartyHosted, err := writer.reader.ReadPartyIsHosted(ctx)
	if err != nil {
		return followedIndex{}, err
	}
	writer.logReadPartyHostingTransition(writer.served.setReadPartyHosted(readPartyHosted))
	writer.served.swapIndex(built.index, built.bootstrapLedgerEnd, built.readPartyRehostCount)
	return built, nil
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

// followOutcome ends one followed index. A non-nil replacement is a caught-up index to follow next.
type followOutcome struct {
	rebuildReason IndexRebuildReason
	err           error
	replacement   *followedIndex
}

// replacementBuild is a background build of an index that replaces the followed index while the writer keeps following it.
type replacementBuild struct {
	reason IndexRebuildReason
	cancel context.CancelFunc
	// result receives exactly one value; done closes after it.
	result chan replacementBuildResult
	done   chan struct{}
}

type replacementBuildResult struct {
	replacement followedIndex
	err         error
}

// ledgerFollowSession is the state of one followLedger call. Only the writer goroutine uses it.
type ledgerFollowSession struct {
	writer      *CoreContractIndexWriter
	followed    *followedIndex
	replacement *replacementBuild
	// stallStartedAt is the first stall check that saw the lag above the bound at stallStartOffset; zero otherwise.
	stallStartedAt   time.Time
	stallStartOffset int64
}

// followLedger subscribes from the followed index offset and applies every advance until a rebuild is due or ctx ends.
func (writer *CoreContractIndexWriter) followLedger(ctx context.Context, followed *followedIndex) followOutcome {
	session := &ledgerFollowSession{writer: writer, followed: followed}
	defer session.stopReplacementBuild()
	session.swapServedIndexWhenCaughtUp()
	consecutiveReconnectAttempts := 0
	resnapshotTicker := time.NewTicker(writer.timing.ResnapshotInterval)
	defer resnapshotTicker.Stop()
	stallCheckTicker := time.NewTicker(writer.timing.LedgerEndPollInterval)
	defer stallCheckTicker.Stop()
	// Runs until ctx ends or a rebuild is due; consecutiveReconnectAttempts bounds the failures.
	for {
		if consecutiveReconnectAttempts > 0 {
			if consecutiveReconnectAttempts > writer.timing.MaxConsecutiveReconnectAttempts {
				return followOutcome{rebuildReason: IndexRebuildReasonReconnectsExhausted}
			}
			streamReconnectsCounter.Inc()
			if !waitForDelay(ctx, backoffDelay(consecutiveReconnectAttempts, writer.timing.ReconnectInitialDelay, writer.timing.ReconnectMaxDelay)) {
				return followOutcome{err: ctx.Err()}
			}
		}
		advances := make(chan cantonclient.CoreContractLedgerAdvance, advanceChannelCapacity)
		subscription, err := writer.reader.SubscribeCoreContractUpdates(ctx, followed.index.Status().Offset, advances)
		if err != nil {
			writer.served.setStreamConnected(false)
			if ctx.Err() != nil {
				return followOutcome{err: ctx.Err()}
			}
			if action, rebuildReason := classifyStreamFailure(err); action == streamFailureActionRebuild {
				return followOutcome{rebuildReason: rebuildReason, err: err}
			}
			consecutiveReconnectAttempts++
			writer.logger.Warn("core contract update stream subscribe failed", zap.Int("consecutiveReconnectAttempts", consecutiveReconnectAttempts), zap.Error(err))
			continue
		}
		writer.served.setStreamConnected(true)
		session.stallStartedAt = time.Time{}
		outcome := session.consumeSubscription(ctx, subscription, advances, resnapshotTicker.C, stallCheckTicker.C)
		subscription.Unsubscribe()
		writer.waitForBackgroundStop(subscription.Done(), "core contract update stream")
		if outcome.replacement == nil {
			// A replacement subscribes at once; the readiness keeps the stream connected through the handover.
			writer.served.setStreamConnected(false)
		}
		if ctx.Err() != nil {
			return followOutcome{err: ctx.Err()}
		}
		if outcome.rebuildReason != "" {
			return followOutcome{rebuildReason: outcome.rebuildReason, err: outcome.err, replacement: outcome.replacement}
		}
		if outcome.appliedAdvanceCount > 0 {
			consecutiveReconnectAttempts = 0
		}
		consecutiveReconnectAttempts++
		writer.logger.Warn("core contract update stream failed", zap.Int64("indexOffset", followed.index.Status().Offset), zap.Int("consecutiveReconnectAttempts", consecutiveReconnectAttempts), zap.Error(outcome.err))
	}
}

// subscriptionOutcome: a non-empty rebuildReason ends the followed index; otherwise err is a stream failure to reconnect after.
type subscriptionOutcome struct {
	rebuildReason       IndexRebuildReason
	err                 error
	replacement         *followedIndex
	appliedAdvanceCount int
}

// consumeSubscription applies advances, runs the resnapshot, checks for a stalled stream and collects
// a replacement build until the subscription ends or a rebuild is due.
func (session *ledgerFollowSession) consumeSubscription(
	ctx context.Context,
	subscription *cantonclient.Subscription,
	advances <-chan cantonclient.CoreContractLedgerAdvance,
	resnapshotTicks <-chan time.Time,
	stallCheckTicks <-chan time.Time,
) subscriptionOutcome {
	writer := session.writer
	outcome := subscriptionOutcome{}
	// Runs until ctx ends, the subscription ends or a rebuild is due.
	for {
		var replacementResults <-chan replacementBuildResult
		if session.replacement != nil {
			replacementResults = session.replacement.result
		}
		select {
		case <-ctx.Done():
			return outcome
		case advance := <-advances:
			if rebuildReason, err := session.applyAdvance(advance); rebuildReason != "" {
				return subscriptionOutcome{rebuildReason: rebuildReason, err: err, appliedAdvanceCount: outcome.appliedAdvanceCount}
			}
			outcome.appliedAdvanceCount++
		case <-subscription.Done():
			return session.applyAdvancesBufferedBeforeDone(subscription, advances, outcome.appliedAdvanceCount)
		case <-stallCheckTicks:
			// A tick can wait in the channel while an apply runs, so the check reads the clock itself.
			if session.streamStalled(time.Now()) {
				outcome.err = errStalledStream
				return outcome
			}
			session.startRehostReplacementBuildWhenDue(ctx)
		case <-resnapshotTicks:
			if session.replacement != nil || session.followed.swapPending {
				continue
			}
			if rebuildReason, err := writer.resnapshotAndCompare(ctx, session.followed.index); rebuildReason != "" {
				writer.logger.Warn("building a replacement core disclosure index", zap.String("reason", string(rebuildReason)), zap.Error(err))
				session.startReplacementBuild(ctx, rebuildReason)
			}
		case result := <-replacementResults:
			reason := session.replacement.reason
			session.replacement = nil
			if result.err == nil {
				return subscriptionOutcome{rebuildReason: reason, replacement: &result.replacement, appliedAdvanceCount: outcome.appliedAdvanceCount}
			}
			indexBuildFailuresCounter.Inc()
			writer.logger.Error("replacement core disclosure index build failed", zap.String("reason", string(reason)), zap.Error(result.err))
			if !session.followed.index.Status().Readiness.ServesSingletonLookups() {
				return subscriptionOutcome{rebuildReason: reason, err: result.err, appliedAdvanceCount: outcome.appliedAdvanceCount}
			}
		}
	}
}

// applyAdvancesBufferedBeforeDone applies every advance buffered in advances after subscription ended, then
// classifies the stream error. The stream goroutine sends every decoded advance before it fails.
func (session *ledgerFollowSession) applyAdvancesBufferedBeforeDone(subscription *cantonclient.Subscription, advances <-chan cantonclient.CoreContractLedgerAdvance, appliedAdvanceCount int) subscriptionOutcome {
	select {
	case <-subscription.Done():
	default:
		panic("cantondisclosure: drain the advances of a running subscription")
	}
	for bufferedAdvanceCount := len(advances); bufferedAdvanceCount > 0; bufferedAdvanceCount-- {
		if rebuildReason, err := session.applyAdvance(<-advances); rebuildReason != "" {
			return subscriptionOutcome{rebuildReason: rebuildReason, err: err, appliedAdvanceCount: appliedAdvanceCount}
		}
		appliedAdvanceCount++
	}
	if len(advances) != 0 {
		panic("cantondisclosure: advances arrived after the subscription ended")
	}
	outcome := subscriptionOutcome{appliedAdvanceCount: appliedAdvanceCount, err: errors.New("cantondisclosure: core contract update stream ended without an error")}
	select {
	case subscriptionErr := <-subscription.Err():
		outcome.err = subscriptionErr
	default:
	}
	if action, rebuildReason := classifyStreamFailure(outcome.err); action == streamFailureActionRebuild {
		outcome.rebuildReason = rebuildReason
	}
	return outcome
}

// applyAdvance applies advance to the followed index and swaps it in once it reaches the served offset.
func (session *ledgerFollowSession) applyAdvance(advance cantonclient.CoreContractLedgerAdvance) (IndexRebuildReason, error) {
	rebuildReason, err := session.writer.applyAdvance(session.followed.index, advance)
	if rebuildReason != "" {
		return rebuildReason, err
	}
	session.swapServedIndexWhenCaughtUp()
	return "", nil
}

// swapServedIndexWhenCaughtUp serves a pending replacement once its offset reaches the frozen served offset.
func (session *ledgerFollowSession) swapServedIndexWhenCaughtUp() {
	if !session.followed.swapPending {
		return
	}
	if session.followed.index.Status().Offset < session.writer.served.indexOffset() {
		return
	}
	session.writer.served.swapIndex(session.followed.index, session.followed.bootstrapLedgerEnd, session.followed.readPartyRehostCount)
	session.followed.swapPending = false
	session.writer.logger.Info("replacement core disclosure index serves", zap.Int64("indexOffset", session.followed.index.Status().Offset))
}

// streamStalled is true when the served ledger lag stayed above the bound for StalledStreamTimeout
// while the ledger end was above the served offset and the followed index offset did not move.
// A lag above the bound with no offset behind comes from failed ledger end reads on a quiet ledger; a reconnect cannot fix it.
// The lag bound must stay above the idle offset checkpoint interval of the participant, or an idle stream reconnects.
func (session *ledgerFollowSession) streamStalled(checkedAt time.Time) bool {
	readiness := session.writer.served.ServiceReadiness()
	followedOffset := session.followed.index.Status().Offset
	if readiness.LedgerLag <= readiness.LedgerLagBound || session.writer.served.LedgerOffsetsBehind() == 0 {
		session.stallStartedAt = time.Time{}
		return false
	}
	if session.stallStartedAt.IsZero() || followedOffset != session.stallStartOffset {
		session.stallStartedAt = checkedAt
		session.stallStartOffset = followedOffset
		return false
	}
	return checkedAt.Sub(session.stallStartedAt) >= session.writer.timing.StalledStreamTimeout
}

// startRehostReplacementBuildWhenDue builds a replacement from a snapshot read after the read party became hosted again.
// The index missed the updates of the unhosted period, so the service stays not ready until the replacement serves.
// A failed build starts again at the next check.
func (session *ledgerFollowSession) startRehostReplacementBuildWhenDue(ctx context.Context) {
	if session.replacement != nil || session.followed.swapPending || !session.writer.served.readPartyRehostReplacementDue() {
		return
	}
	session.writer.logger.Warn("building a replacement core disclosure index", zap.String("reason", string(IndexRebuildReasonReadPartyRehosted)))
	session.startReplacementBuild(ctx, IndexRebuildReasonReadPartyRehosted)
}

// startReplacementBuild bootstraps a new index in the background and follows it until it reaches the served offset.
func (session *ledgerFollowSession) startReplacementBuild(ctx context.Context, reason IndexRebuildReason) {
	if session.replacement != nil {
		panic("cantondisclosure: second replacement build")
	}
	buildCtx, cancel := context.WithCancel(ctx)
	build := &replacementBuild{reason: reason, cancel: cancel, result: make(chan replacementBuildResult, 1), done: make(chan struct{})}
	writer := session.writer
	go func() {
		defer close(build.done)
		replacement, err := writer.readBootstrappedIndex(buildCtx)
		if err == nil {
			replacement.swapPending = true
			err = writer.catchUpReplacementIndex(buildCtx, replacement.index)
		}
		build.result <- replacementBuildResult{replacement: replacement, err: err}
	}()
	session.replacement = build
}

func (session *ledgerFollowSession) stopReplacementBuild() {
	if session.replacement == nil {
		return
	}
	session.replacement.cancel()
	session.writer.waitForBackgroundStop(session.replacement.done, "replacement index build")
	session.replacement = nil
}

// catchUpReplacementIndex applies advances to replacement until its offset reaches the served offset,
// within SnapshotReadTimeout.
func (writer *CoreContractIndexWriter) catchUpReplacementIndex(ctx context.Context, replacement *CoreContractIndex) error {
	if replacement.Status().Offset >= writer.served.indexOffset() {
		return nil
	}
	catchUpCtx, cancel := context.WithTimeout(ctx, writer.timing.SnapshotReadTimeout)
	defer cancel()
	advances := make(chan cantonclient.CoreContractLedgerAdvance, advanceChannelCapacity)
	subscription, err := writer.reader.SubscribeCoreContractUpdates(catchUpCtx, replacement.Status().Offset, advances)
	if err != nil {
		return fmt.Errorf("cantondisclosure: replacement catch-up subscribe: %w", err)
	}
	defer func() {
		subscription.Unsubscribe()
		writer.waitForBackgroundStop(subscription.Done(), "replacement catch-up stream")
	}()
	// Runs until the replacement reaches the served offset, the stream fails or catchUpCtx ends.
	for {
		select {
		case <-catchUpCtx.Done():
			return fmt.Errorf("cantondisclosure: replacement catch-up at offset %d: %w", replacement.Status().Offset, catchUpCtx.Err())
		case <-subscription.Done():
			if len(advances) == 0 {
				streamErr := errors.New("cantondisclosure: replacement catch-up stream ended without an error")
				select {
				case subscriptionErr := <-subscription.Err():
					streamErr = subscriptionErr
				default:
				}
				return fmt.Errorf("cantondisclosure: replacement catch-up stream: %w", streamErr)
			}
		case advance := <-advances:
			if rebuildReason, err := writer.applyAdvance(replacement, advance); rebuildReason != "" {
				return fmt.Errorf("cantondisclosure: replacement catch-up %s: %w", rebuildReason, err)
			}
			if replacement.Status().Offset >= writer.served.indexOffset() {
				return nil
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
	status := index.Status()
	if status.Readiness == IndexReadinessInconsistent {
		return IndexRebuildReasonInconsistentIndex, err
	}
	if err != nil {
		// The refused update is missing from index, so its lookups stop until the rebuilt index serves.
		index.SuspendLookupsUntilReplaced()
		return IndexRebuildReasonIndexUpdateRefused, err
	}
	if status.Offset != advance.Offset() || status.Offset <= offsetBefore {
		panic("cantondisclosure: applied advance did not move the index offset to the advance offset")
	}
	return "", nil
}

// recordReadPartyHosting keeps the last known hosting state when the read fails; after the ledger lag bound
// without a successful read, /v1/readyz reports read_party_hosting_unknown.
// SECURITY: a participant that stops hosting the read party keeps its ACS and its offset checkpoints, so neither the
// ledger lag nor the resnapshot comparison detects the frozen index. Only this check makes /v1/readyz fail.
func (writer *CoreContractIndexWriter) recordReadPartyHosting(ctx context.Context) {
	readPartyHosted, err := writer.reader.ReadPartyIsHosted(ctx)
	if ctx.Err() != nil {
		return
	}
	if err != nil {
		readPartyHostingReadFailuresCounter.Inc()
		writer.logger.Warn("read party hosting read failed", zap.Error(err))
		return
	}
	writer.logReadPartyHostingTransition(writer.served.setReadPartyHosted(readPartyHosted))
}

func (writer *CoreContractIndexWriter) logReadPartyHostingTransition(transition readPartyHostingTransition) {
	switch transition {
	case readPartyHostingTransitionUnchanged:
	case readPartyHostingTransitionLost:
		writer.logger.Warn("the participant no longer hosts the read party; /v1/readyz reports read_party_not_hosted", zap.String("guardianGovernance", writer.guardianGovernance.String()))
	case readPartyHostingTransitionRegained:
		writer.logger.Info("the participant hosts the read party again; a replacement index must serve before /v1/readyz reports ready", zap.String("guardianGovernance", writer.guardianGovernance.String()))
	default:
		panic(fmt.Sprintf("cantondisclosure: unknown hosting transition %d", int(transition)))
	}
}

// resnapshotAndCompare reads the active contracts at the index offset and compares them with the index.
// Applies wait meanwhile. A non-empty reason requests a replacement build. A stale or missing contract
// suspends every lookup of index until the replacement serves.
func (writer *CoreContractIndexWriter) resnapshotAndCompare(ctx context.Context, index *CoreContractIndex) (IndexRebuildReason, error) {
	indexStatus := index.Status()
	if indexStatus.ReplayNodesDroppedAtByteCap {
		return IndexRebuildReasonByteCapRetry, nil
	}
	snapshotCtx, cancel := context.WithTimeout(ctx, writer.timing.SnapshotReadTimeout)
	defer cancel()
	snapshot, err := writer.reader.ReadActiveCoreContracts(snapshotCtx, indexStatus.Offset, writer.snapshotReplayNodeLimit)
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
	if snapshot.ReplayNodesTruncated() {
		return IndexRebuildReasonSnapshotReplayNodeLimit, nil
	}
	snapshotContracts := snapshot.Contracts()
	comparison, err := index.CompareWithSnapshot(indexStatus.Offset, snapshotContracts)
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
		writer.logger.Info("resnapshot matches the index", zap.Int64("indexOffset", indexStatus.Offset), zap.Int("snapshotContracts", len(snapshotContracts)))
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
		index.SuspendLookupsUntilReplaced()
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
