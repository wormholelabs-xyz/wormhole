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

// DefaultLedgerLagBound is the default ledger lag bound of /v1/readyz.
const DefaultLedgerLagBound = time.Minute

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
	serviceReadinessStatusIndexPrefix                                = "index_"
)

// ServiceReadiness is a point-in-time copy of the service state that /v1/readyz reports.
type ServiceReadiness struct {
	BootstrapCompleted bool
	// ReadPartyUnhosted is true after the latest hosting read found no connected synchronizer that hosts the read party.
	ReadPartyUnhosted bool
	StreamConnected   bool
	// LedgerLag is the age of the newest ledger end read that the index offset reached: an upper bound
	// of the time since the index last held every core contract update of the ledger.
	LedgerLag      time.Duration
	LedgerLagBound time.Duration
	IndexReadiness IndexReadiness
}

// Status is ready only after the bootstrap, with a hosted read party, a connected stream, a ledger lag
// within the bound, and an index that serves singleton lookups.
func (readiness ServiceReadiness) Status() ServiceReadinessStatus {
	switch {
	case !readiness.BootstrapCompleted:
		return ServiceReadinessStatusBootstrapping
	case readiness.ReadPartyUnhosted:
		return ServiceReadinessStatusReadPartyNotHosted
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

	mutex             sync.Mutex
	streamConnected   bool
	readPartyUnhosted bool
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
// bootstrapLedgerEnd is the ledger end read of its bootstrap snapshot, which the index reached.
func (served *ServedIndex) swapIndex(index *CoreContractIndex, bootstrapLedgerEnd observedLedgerEnd) {
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
	served.currentIndex.Store(index)
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

// setReadPartyHosted returns true when the hosting state changes.
func (served *ServedIndex) setReadPartyHosted(hosted bool) bool {
	served.mutex.Lock()
	defer served.mutex.Unlock()
	changed := served.readPartyUnhosted == hosted
	served.readPartyUnhosted = !hosted
	return changed
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
	indexOffset := served.indexOffset()
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
	readiness := ServiceReadiness{
		StreamConnected:   served.streamConnected,
		ReadPartyUnhosted: served.readPartyUnhosted,
		LedgerLagBound:    served.ledgerLagBound,
		IndexReadiness:    IndexReadinessBootstrapping,
	}
	if index == nil {
		return readiness
	}
	status := index.Status()
	readiness.BootstrapCompleted = true
	readiness.IndexReadiness = status.Readiness
	readiness.LedgerLag = served.ledgerLagLocked(served.now(), status.Offset)
	return readiness
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
	index := served.currentIndex.Load()
	if index == nil {
		return CoreContractLookupResult{}, errNoServedIndex
	}
	return index.LookupSingleton(template)
}

func (served *ServedIndex) LookupCoveringReplayNode(consumer cantonclient.PartyID, namespace cantonclient.ReplayNamespace, digest ReplayDigest) (CoreContractLookupResult, error) {
	index := served.currentIndex.Load()
	if index == nil {
		return CoreContractLookupResult{}, errNoServedIndex
	}
	return index.LookupCoveringReplayNode(consumer, namespace, digest)
}

func (served *ServedIndex) LookupClaimedRootsNode(consumer cantonclient.PartyID, namespace cantonclient.ReplayNamespace) (CoreContractLookupResult, error) {
	index := served.currentIndex.Load()
	if index == nil {
		return CoreContractLookupResult{}, errNoServedIndex
	}
	return index.LookupClaimedRootsNode(consumer, namespace)
}
