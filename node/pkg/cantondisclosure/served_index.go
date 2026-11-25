package cantondisclosure

import (
	"fmt"
	"slices"
	"sync"
	"sync/atomic"
	"time"

	"github.com/certusone/wormhole/node/pkg/cantonclient"
)

// Bounds of the ledger lag bound of /v1/readyz. The minimum is twice the production ledger end poll interval,
// because an index at the ledger end reports a lag of up to one poll interval.
const (
	minLedgerLagBound = 10 * time.Second
	maxLedgerLagBound = time.Hour
)

// ProductionLedgerEndPollInterval is the interval of the ledger end reads of the lag measurement.
const ProductionLedgerEndPollInterval = 5 * time.Second

// ledgerEndObservationCapacity holds one unreached ledger end read per poll over the largest lag bound.
const ledgerEndObservationCapacity = int(maxLedgerLagBound/ProductionLedgerEndPollInterval) + 1

// Compile-time assertion: minLedgerLagBound >= 2 * ProductionLedgerEndPollInterval.
const _ uint64 = uint64(minLedgerLagBound - 2*ProductionLedgerEndPollInterval)

// DefaultLedgerLagBound is the default ledger lag bound of /v1/readyz. Without core traffic, the index offset moves only
// on offset checkpoints; on Canton 3.7.3 the lag then reaches about 75 s (LocalNet measurement).
const DefaultLedgerLagBound = 3 * time.Minute

// CheckLedgerLagBound requires ledgerLagBound in [10s, 1h] and at least twice ProductionLedgerEndPollInterval.
func CheckLedgerLagBound(ledgerLagBound time.Duration) error {
	if ledgerLagBound < 2*ProductionLedgerEndPollInterval {
		return fmt.Errorf("cantondisclosure: ledger lag bound %s is below twice the ledger end poll interval %s", ledgerLagBound, ProductionLedgerEndPollInterval)
	}
	if ledgerLagBound < minLedgerLagBound || ledgerLagBound > maxLedgerLagBound {
		return fmt.Errorf("cantondisclosure: ledger lag bound %s is outside [%s, %s]", ledgerLagBound, minLedgerLagBound, maxLedgerLagBound)
	}
	return nil
}

// ServiceReadinessStatus is the fixed /v1/readyz status text.
type ServiceReadinessStatus string

const (
	ServiceReadinessStatusReady               ServiceReadinessStatus = "ready"
	ServiceReadinessStatusReadyByteCapReached ServiceReadinessStatus = "ready_byte_cap_reached"
	ServiceReadinessStatusBootstrapping       ServiceReadinessStatus = "bootstrapping"
	ServiceReadinessStatusStreamDisconnected  ServiceReadinessStatus = "stream_disconnected"
	ServiceReadinessStatusLedgerLagAboveBound ServiceReadinessStatus = "ledger_lag_above_bound"
	ServiceReadinessStatusReadPartyNotHosted  ServiceReadinessStatus = "read_party_not_hosted"
	// ServiceReadinessStatusReadPartyHostingUnknown: the last successful hosting read is older than the ledger lag bound.
	ServiceReadinessStatusReadPartyHostingUnknown ServiceReadinessStatus = "read_party_hosting_unknown"
	// ServiceReadinessStatusReadPartyRehosted: the read party is hosted again, and a replacement index is not yet served.
	ServiceReadinessStatusReadPartyRehosted ServiceReadinessStatus = "read_party_rehosted"
	serviceReadinessStatusIndexPrefix                              = "index_"
)

// ServiceReadiness is a point-in-time copy of the service state that /v1/readyz reports.
type ServiceReadiness struct {
	BootstrapCompleted bool
	// ReadPartyHosted is true after the latest successful hosting read found a connected synchronizer that hosts the read party.
	ReadPartyHosted bool
	// ReadPartyHostingReadAge is the age of the latest successful hosting read.
	ReadPartyHostingReadAge time.Duration
	// ReadPartyRehostedBeforeServedSnapshot is true when the read party became hosted again after the snapshot of the served index.
	ReadPartyRehostedBeforeServedSnapshot bool
	StreamConnected                       bool
	// LedgerLag is the age of the newest ledger end read that the index offset reached: an upper bound
	// of the time since the index last held every core contract update of the ledger.
	LedgerLag      time.Duration
	LedgerLagBound time.Duration
	IndexReadiness IndexReadiness
}

// readPartyHostingStatus is the not-ready status of the read party hosting, and false when the hosting admits lookups.
func (readiness ServiceReadiness) readPartyHostingStatus() (ServiceReadinessStatus, bool) {
	switch {
	case readiness.ReadPartyHostingReadAge > readiness.LedgerLagBound:
		return ServiceReadinessStatusReadPartyHostingUnknown, true
	case !readiness.ReadPartyHosted:
		return ServiceReadinessStatusReadPartyNotHosted, true
	case !readiness.ReadPartyRehostedBeforeServedSnapshot:
		return "", false
	default:
		return ServiceReadinessStatusReadPartyRehosted, true
	}
}

// Status is ready only after the bootstrap, with a hosted read party read within the lag bound, a served snapshot
// read after the last re-hosting, a connected stream, a ledger lag within the bound, and an index that serves singleton lookups.
func (readiness ServiceReadiness) Status() ServiceReadinessStatus {
	hostingStatus, hostingRefusesLookups := readiness.readPartyHostingStatus()
	switch {
	case !readiness.BootstrapCompleted:
		return ServiceReadinessStatusBootstrapping
	case hostingRefusesLookups:
		return hostingStatus
	case !readiness.StreamConnected:
		return ServiceReadinessStatusStreamDisconnected
	case readiness.LedgerLag > readiness.LedgerLagBound:
		return ServiceReadinessStatusLedgerLagAboveBound
	case !readiness.IndexReadiness.ServesSingletonLookups():
		return ServiceReadinessStatus(serviceReadinessStatusIndexPrefix + readiness.IndexReadiness.String())
	case readiness.IndexReadiness == IndexReadinessByteCapReached:
		return ServiceReadinessStatusReadyByteCapReached
	default:
		return ServiceReadinessStatusReady
	}
}

// IsReady is true for ServiceReadinessStatusReady and ServiceReadinessStatusReadyByteCapReached.
func (readiness ServiceReadiness) IsReady() bool {
	status := readiness.Status()
	return status == ServiceReadinessStatusReady || status == ServiceReadinessStatusReadyByteCapReached
}

// observedLedgerEnd is a ledger end and the time the writer read it.
type observedLedgerEnd struct {
	offset     int64
	observedAt time.Time
}

// ServedIndex is the index that the HTTP handlers read. The writer swaps in each rebuilt index atomically.
type ServedIndex struct {
	currentIndex   atomic.Pointer[CoreContractIndex]
	ledgerLagBound time.Duration
	now            func() time.Time

	mutex           sync.Mutex
	streamConnected bool
	// readPartyHosted is false until a hosting read finds the read party hosted.
	readPartyHosted bool
	// readPartyHostingReadAt is the time of the last successful hosting read; zero before the first.
	readPartyHostingReadAt time.Time
	// readPartyRehostCount counts the hosted-again transitions after the first swap. servedIndexRehostCount is
	// the count before the snapshot of the served index: a lower value means a snapshot of a frozen ACS.
	readPartyRehostCount   uint64
	servedIndexRehostCount uint64
	// latestLedgerEnd is the last ledger end read. unreachedLedgerEnds holds the reads above the index offset,
	// oldest first, with strictly increasing offsets, at most ledgerEndObservationCapacity.
	// newestReachedLedgerEnd is the newest read at or below the index offset.
	latestLedgerEnd           observedLedgerEnd
	hasLatestLedgerEnd        bool
	unreachedLedgerEnds       []observedLedgerEnd
	newestReachedLedgerEnd    observedLedgerEnd
	hasNewestReachedLedgerEnd bool
}

// NewServedIndex fails each lookup with IndexReadinessBootstrapping until the writer swaps in the first index.
func NewServedIndex(ledgerLagBound time.Duration) (*ServedIndex, error) {
	return newServedIndexWithClock(ledgerLagBound, time.Now)
}

func newServedIndexWithClock(ledgerLagBound time.Duration, now func() time.Time) (*ServedIndex, error) {
	if err := CheckLedgerLagBound(ledgerLagBound); err != nil {
		return nil, err
	}
	if now == nil {
		return nil, fmt.Errorf("cantondisclosure: missing clock")
	}
	return &ServedIndex{ledgerLagBound: ledgerLagBound, now: now, unreachedLedgerEnds: make([]observedLedgerEnd, 0, ledgerEndObservationCapacity)}, nil
}

// swapIndex serves index from now on. index must be bootstrapped, and its offset must not be below the served offset.
// bootstrapLedgerEnd is the ledger end read of its bootstrap snapshot, which the index reached. rehostCount is
// readPartyRehostCount before that snapshot.
func (served *ServedIndex) swapIndex(index *CoreContractIndex, bootstrapLedgerEnd observedLedgerEnd, rehostCount uint64) {
	if index == nil {
		panic("cantondisclosure: swap to a nil index")
	}
	status := index.Status()
	if status.Readiness == IndexReadinessBootstrapping {
		panic("cantondisclosure: swap to an index without a bootstrap snapshot")
	}
	if bootstrapLedgerEnd.offset > status.Offset {
		panic("cantondisclosure: swap with a bootstrap ledger end above the index offset")
	}
	served.mutex.Lock()
	defer served.mutex.Unlock()
	if status.Offset < served.indexOffset() {
		panic("cantondisclosure: swap to an index below the served offset")
	}
	if rehostCount > served.readPartyRehostCount {
		panic("cantondisclosure: swap with a rehost count above the current count")
	}
	served.currentIndex.Store(index)
	// A replacement that started before a later swap keeps the higher count.
	served.servedIndexRehostCount = max(served.servedIndexRehostCount, rehostCount)
	served.recordReachedLedgerEndLocked(bootstrapLedgerEnd)
	served.removeReachedLedgerEndsLocked(status.Offset)
}

// recordReachedLedgerEndLocked keeps the newest reached read.
func (served *ServedIndex) recordReachedLedgerEndLocked(reached observedLedgerEnd) {
	if served.hasNewestReachedLedgerEnd && !reached.observedAt.After(served.newestReachedLedgerEnd.observedAt) {
		return
	}
	served.newestReachedLedgerEnd = reached
	served.hasNewestReachedLedgerEnd = true
}

func (served *ServedIndex) setStreamConnected(connected bool) {
	served.mutex.Lock()
	defer served.mutex.Unlock()
	served.streamConnected = connected
}

// readPartyHostingTransition is the change of one successful hosting read.
type readPartyHostingTransition int

const (
	readPartyHostingTransitionUnchanged readPartyHostingTransition = iota + 1
	readPartyHostingTransitionLost
	readPartyHostingTransitionRegained
)

// setReadPartyHosted records a successful hosting read at the clock time. A regained hosting after the first swap
// increments readPartyRehostCount, so that the served index stays not ready until a snapshot read after it serves.
func (served *ServedIndex) setReadPartyHosted(hosted bool) readPartyHostingTransition {
	served.mutex.Lock()
	defer served.mutex.Unlock()
	readAt := served.now()
	if readAt.Before(served.readPartyHostingReadAt) {
		panic("cantondisclosure: hosting reads go back in time")
	}
	served.readPartyHostingReadAt = readAt
	wasHosted := served.readPartyHosted
	served.readPartyHosted = hosted
	switch {
	case wasHosted == hosted:
		return readPartyHostingTransitionUnchanged
	case !hosted:
		return readPartyHostingTransitionLost
	}
	if served.currentIndex.Load() != nil {
		served.readPartyRehostCount++
	}
	return readPartyHostingTransitionRegained
}

// currentReadPartyRehostCount is the count that a snapshot read from now on passes to swapIndex.
func (served *ServedIndex) currentReadPartyRehostCount() uint64 {
	served.mutex.Lock()
	defer served.mutex.Unlock()
	return served.readPartyRehostCount
}

// readPartyRehostReplacementDue is true while the read party is hosted, read within the lag bound, and hosted again
// after the snapshot of the served index.
func (served *ServedIndex) readPartyRehostReplacementDue() bool {
	readiness := served.ServiceReadiness()
	status, refusesLookups := readiness.readPartyHostingStatus()
	return refusesLookups && status == ServiceReadinessStatusReadPartyRehosted
}

// recordLedgerEnd stores a ledger end read at observedAt. A full queue merges the read into its newest entry,
// so that the oldest unreached read keeps its time.
func (served *ServedIndex) recordLedgerEnd(ledgerEnd int64, observedAt time.Time) {
	if ledgerEnd < 0 {
		panic(fmt.Sprintf("cantondisclosure: negative ledger end %d", ledgerEnd))
	}
	served.mutex.Lock()
	defer served.mutex.Unlock()
	indexOffset := served.indexOffset()
	if served.hasLatestLedgerEnd && observedAt.Before(served.latestLedgerEnd.observedAt) {
		panic("cantondisclosure: ledger end observations go back in time")
	}
	served.latestLedgerEnd = observedLedgerEnd{offset: ledgerEnd, observedAt: observedAt}
	served.hasLatestLedgerEnd = true
	served.removeReachedLedgerEndsLocked(indexOffset)
	if ledgerEnd <= indexOffset {
		served.recordReachedLedgerEndLocked(served.latestLedgerEnd)
		return
	}
	queuedCount := len(served.unreachedLedgerEnds)
	if queuedCount > 0 && ledgerEnd <= served.unreachedLedgerEnds[queuedCount-1].offset {
		return
	}
	if queuedCount == ledgerEndObservationCapacity {
		served.unreachedLedgerEnds[queuedCount-1].offset = ledgerEnd
		return
	}
	served.unreachedLedgerEnds = append(served.unreachedLedgerEnds, served.latestLedgerEnd)
	if len(served.unreachedLedgerEnds) > ledgerEndObservationCapacity {
		panic("cantondisclosure: unreached ledger end queue exceeds its capacity")
	}
}

// indexOffset is -1 before the first swap. Callers that compare it with the reached reads hold served.mutex,
// so that the offset is read after every reached read that it covers.
func (served *ServedIndex) indexOffset() int64 {
	index := served.currentIndex.Load()
	if index == nil {
		return -1
	}
	return index.Status().Offset
}

// removeReachedLedgerEndsLocked moves the queued reads at or below indexOffset to the newest reached read.
func (served *ServedIndex) removeReachedLedgerEndsLocked(indexOffset int64) {
	reachedCount := 0
	for reachedCount < len(served.unreachedLedgerEnds) && served.unreachedLedgerEnds[reachedCount].offset <= indexOffset {
		served.recordReachedLedgerEndLocked(served.unreachedLedgerEnds[reachedCount])
		reachedCount++
	}
	served.unreachedLedgerEnds = slices.Delete(served.unreachedLedgerEnds, 0, reachedCount)
	if len(served.unreachedLedgerEnds) > 0 && served.unreachedLedgerEnds[0].offset <= indexOffset {
		panic("cantondisclosure: a reached ledger end stays queued")
	}
}

// unknownLedgerLag is the lag before the index reached any ledger end read.
const unknownLedgerLag = maxLedgerLagBound + time.Second

// ledgerLagLocked is the age of the newest read that indexOffset reached. Under a constant lag D it is D at
// each poll and grows to D plus one poll interval before the next, so a lag above the bound always fails readiness.
func (served *ServedIndex) ledgerLagLocked(now time.Time, indexOffset int64) time.Duration {
	served.removeReachedLedgerEndsLocked(indexOffset)
	if !served.hasNewestReachedLedgerEnd {
		return unknownLedgerLag
	}
	if served.newestReachedLedgerEnd.offset > indexOffset {
		panic("cantondisclosure: the newest reached ledger end is above the index offset")
	}
	return max(now.Sub(served.newestReachedLedgerEnd.observedAt), 0)
}

// LedgerOffsetsBehind is the latest read ledger end minus the index offset, at least 0.
func (served *ServedIndex) LedgerOffsetsBehind() int64 {
	served.mutex.Lock()
	defer served.mutex.Unlock()
	return served.ledgerOffsetsBehindLocked(served.indexOffset())
}

func (served *ServedIndex) ledgerOffsetsBehindLocked(indexOffset int64) int64 {
	if !served.hasLatestLedgerEnd || indexOffset < 0 {
		return 0
	}
	return max(served.latestLedgerEnd.offset-indexOffset, 0)
}

// ServiceReadiness is a copy of the readiness inputs.
func (served *ServedIndex) ServiceReadiness() ServiceReadiness {
	served.mutex.Lock()
	defer served.mutex.Unlock()
	index := served.currentIndex.Load()
	if index == nil {
		return served.serviceReadinessLocked(served.now(), IndexStatus{}, false)
	}
	return served.serviceReadinessLocked(served.now(), index.Status(), true)
}

func (served *ServedIndex) serviceReadinessLocked(now time.Time, indexStatus IndexStatus, hasIndex bool) ServiceReadiness {
	readiness := served.readPartyHostingReadinessLocked(now)
	readiness.StreamConnected = served.streamConnected
	readiness.IndexReadiness = IndexReadinessBootstrapping
	if !hasIndex {
		return readiness
	}
	readiness.BootstrapCompleted = true
	readiness.IndexReadiness = indexStatus.Readiness
	readiness.LedgerLag = served.ledgerLagLocked(now, indexStatus.Offset)
	return readiness
}

// servedIndexMetricsSnapshot is one consistent read of the gauge inputs; indexStatus is zero before the first swap.
type servedIndexMetricsSnapshot struct {
	readiness           ServiceReadiness
	indexStatus         IndexStatus
	ledgerOffsetsBehind int64
}

// readMetricsSnapshot reads the index status once under served.mutex for every gauge of one scrape.
func (served *ServedIndex) readMetricsSnapshot() servedIndexMetricsSnapshot {
	served.mutex.Lock()
	defer served.mutex.Unlock()
	index := served.currentIndex.Load()
	if index == nil {
		return servedIndexMetricsSnapshot{readiness: served.serviceReadinessLocked(served.now(), IndexStatus{}, false)}
	}
	indexStatus := index.Status()
	return servedIndexMetricsSnapshot{
		readiness:           served.serviceReadinessLocked(served.now(), indexStatus, true),
		indexStatus:         indexStatus,
		ledgerOffsetsBehind: served.ledgerOffsetsBehindLocked(indexStatus.Offset),
	}
}

// readPartyHostingReadinessLocked fills the hosting fields and the lag bound. A served index before any hosting
// read reports an unknown hosting.
func (served *ServedIndex) readPartyHostingReadinessLocked(now time.Time) ServiceReadiness {
	hostingReadAge := unknownLedgerLag
	if !served.readPartyHostingReadAt.IsZero() {
		hostingReadAge = max(now.Sub(served.readPartyHostingReadAt), 0)
	}
	if served.servedIndexRehostCount > served.readPartyRehostCount {
		panic("cantondisclosure: served rehost count above the current count")
	}
	return ServiceReadiness{
		ReadPartyHosted:                       served.readPartyHosted,
		ReadPartyHostingReadAge:               hostingReadAge,
		ReadPartyRehostedBeforeServedSnapshot: served.servedIndexRehostCount < served.readPartyRehostCount,
		LedgerLagBound:                        served.ledgerLagBound,
	}
}

// ReadPartyHostingNotReadyError fails every lookup while the read party hosting refuses lookups.
type ReadPartyHostingNotReadyError struct {
	Status ServiceReadinessStatus
}

func (notReady ReadPartyHostingNotReadyError) Error() string {
	return fmt.Sprintf("%s: %s", ErrIndexNotReady, notReady.Status)
}

func (notReady ReadPartyHostingNotReadyError) Is(target error) bool {
	return target == ErrIndexNotReady
}

// servedIndexForLookup is the served index, or an error before the first swap or while the hosting refuses lookups.
// SECURITY: an unhosted participant keeps a frozen ACS, so its blobs can be stale.
func (served *ServedIndex) servedIndexForLookup() (*CoreContractIndex, error) {
	index := served.currentIndex.Load()
	if index == nil {
		return nil, errNoServedIndex
	}
	served.mutex.Lock()
	readiness := served.readPartyHostingReadinessLocked(served.now())
	served.mutex.Unlock()
	if status, refusesLookups := readiness.readPartyHostingStatus(); refusesLookups {
		return nil, ReadPartyHostingNotReadyError{Status: status}
	}
	return index, nil
}

// IndexStatus returns false before the first swap.
func (served *ServedIndex) IndexStatus() (IndexStatus, bool) {
	index := served.currentIndex.Load()
	if index == nil {
		return IndexStatus{}, false
	}
	return index.Status(), true
}

var errNoServedIndex = IndexNotReadyError{Readiness: IndexReadinessBootstrapping}

func (served *ServedIndex) LookupSingleton(template cantonclient.CoreContractTemplate) (CoreContractLookupResult, error) {
	index, err := served.servedIndexForLookup()
	if err != nil {
		return CoreContractLookupResult{}, err
	}
	return index.LookupSingleton(template)
}

func (served *ServedIndex) LookupCoveringReplayNode(consumer cantonclient.PartyID, namespace cantonclient.ReplayNamespace, digest ReplayDigest) (CoreContractLookupResult, error) {
	index, err := served.servedIndexForLookup()
	if err != nil {
		return CoreContractLookupResult{}, err
	}
	return index.LookupCoveringReplayNode(consumer, namespace, digest)
}

func (served *ServedIndex) LookupClaimedRootsNode(consumer cantonclient.PartyID, namespace cantonclient.ReplayNamespace) (CoreContractLookupResult, error) {
	index, err := served.servedIndexForLookup()
	if err != nil {
		return CoreContractLookupResult{}, err
	}
	return index.LookupClaimedRootsNode(consumer, namespace)
}
