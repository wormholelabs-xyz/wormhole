package cantondisclosure

import (
	"fmt"
	"sync"
	"sync/atomic"
	"time"

	"github.com/certusone/wormhole/node/pkg/cantonclient"
)

// Bounds of the ledger lag bound of /v1/readyz.
const (
	minLedgerLagBound = time.Second
	maxLedgerLagBound = time.Hour
)

// ServiceReadinessStatus is the fixed /v1/readyz status text.
type ServiceReadinessStatus string

const (
	ServiceReadinessStatusReady               ServiceReadinessStatus = "ready"
	ServiceReadinessStatusReadyByteCapReached ServiceReadinessStatus = "ready_byte_cap_reached"
	ServiceReadinessStatusBootstrapping       ServiceReadinessStatus = "bootstrapping"
	ServiceReadinessStatusStreamDisconnected  ServiceReadinessStatus = "stream_disconnected"
	ServiceReadinessStatusLedgerLagAboveBound ServiceReadinessStatus = "ledger_lag_above_bound"
	serviceReadinessStatusIndexPrefix                                = "index_"
)

// ServiceReadiness is a point-in-time copy of the service state that /v1/readyz reports.
type ServiceReadiness struct {
	BootstrapCompleted bool
	StreamConnected    bool
	// LedgerLag bounds how long ago the index offset last reached an observed ledger end.
	LedgerLag      time.Duration
	LedgerLagBound time.Duration
	IndexReadiness IndexReadiness
}

// Status is ready only after the bootstrap, with a connected stream, a ledger lag
// within the bound, and an index that serves singleton lookups.
func (readiness ServiceReadiness) Status() ServiceReadinessStatus {
	switch {
	case !readiness.BootstrapCompleted:
		return ServiceReadinessStatusBootstrapping
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
	// latestLedgerEnd is the last ledger end read. unreachedLedgerEnd is the oldest read ledger end above the index offset.
	latestLedgerEnd       observedLedgerEnd
	hasLatestLedgerEnd    bool
	unreachedLedgerEnd    observedLedgerEnd
	hasUnreachedLedgerEnd bool
}

// NewServedIndex fails each lookup with IndexReadinessBootstrapping until the writer swaps in the first index.
func NewServedIndex(ledgerLagBound time.Duration) (*ServedIndex, error) {
	return newServedIndexWithClock(ledgerLagBound, time.Now)
}

func newServedIndexWithClock(ledgerLagBound time.Duration, now func() time.Time) (*ServedIndex, error) {
	if ledgerLagBound < minLedgerLagBound || ledgerLagBound > maxLedgerLagBound {
		return nil, fmt.Errorf("cantondisclosure: ledger lag bound %s is outside [%s, %s]", ledgerLagBound, minLedgerLagBound, maxLedgerLagBound)
	}
	if now == nil {
		return nil, fmt.Errorf("cantondisclosure: missing clock")
	}
	return &ServedIndex{ledgerLagBound: ledgerLagBound, now: now}, nil
}

// swapIndex serves index from now on. index must be bootstrapped.
func (served *ServedIndex) swapIndex(index *CoreContractIndex) {
	if index == nil {
		panic("cantondisclosure: swap to a nil index")
	}
	if index.Status().Readiness == IndexReadinessBootstrapping {
		panic("cantondisclosure: swap to an index without a bootstrap snapshot")
	}
	served.currentIndex.Store(index)
}

func (served *ServedIndex) setStreamConnected(connected bool) {
	served.mutex.Lock()
	defer served.mutex.Unlock()
	served.streamConnected = connected
}

// recordLedgerEnd stores a ledger end read at observedAt.
func (served *ServedIndex) recordLedgerEnd(ledgerEnd int64, observedAt time.Time) {
	if ledgerEnd < 0 {
		panic(fmt.Sprintf("cantondisclosure: negative ledger end %d", ledgerEnd))
	}
	served.mutex.Lock()
	defer served.mutex.Unlock()
	if served.hasLatestLedgerEnd && observedAt.Before(served.latestLedgerEnd.observedAt) {
		panic("cantondisclosure: ledger end observations go back in time")
	}
	served.latestLedgerEnd = observedLedgerEnd{offset: ledgerEnd, observedAt: observedAt}
	served.hasLatestLedgerEnd = true
	served.clearReachedLedgerEndLocked(served.indexOffset())
	if !served.hasUnreachedLedgerEnd && ledgerEnd > served.indexOffset() {
		served.unreachedLedgerEnd = served.latestLedgerEnd
		served.hasUnreachedLedgerEnd = true
	}
}

// indexOffset is -1 before the first swap.
func (served *ServedIndex) indexOffset() int64 {
	index := served.currentIndex.Load()
	if index == nil {
		return -1
	}
	return index.Status().Offset
}

func (served *ServedIndex) clearReachedLedgerEndLocked(indexOffset int64) {
	if served.hasUnreachedLedgerEnd && indexOffset >= served.unreachedLedgerEnd.offset {
		served.hasUnreachedLedgerEnd = false
	}
}

// ledgerLagLocked is the age of the latest ledger end read, or the age of the oldest unreached ledger end if larger.
func (served *ServedIndex) ledgerLagLocked(now time.Time, indexOffset int64) time.Duration {
	if !served.hasLatestLedgerEnd {
		return maxLedgerLagBound + time.Second
	}
	served.clearReachedLedgerEndLocked(indexOffset)
	ledgerLag := now.Sub(served.latestLedgerEnd.observedAt)
	if served.hasUnreachedLedgerEnd {
		ledgerLag = max(ledgerLag, now.Sub(served.unreachedLedgerEnd.observedAt))
	}
	return max(ledgerLag, 0)
}

// LedgerOffsetsBehind is the latest read ledger end minus the index offset, at least 0.
func (served *ServedIndex) LedgerOffsetsBehind() int64 {
	indexOffset := served.indexOffset()
	served.mutex.Lock()
	defer served.mutex.Unlock()
	if !served.hasLatestLedgerEnd || indexOffset < 0 {
		return 0
	}
	return max(served.latestLedgerEnd.offset-indexOffset, 0)
}

// ServiceReadiness is a copy of the readiness inputs.
func (served *ServedIndex) ServiceReadiness() ServiceReadiness {
	index := served.currentIndex.Load()
	served.mutex.Lock()
	defer served.mutex.Unlock()
	readiness := ServiceReadiness{
		StreamConnected: served.streamConnected,
		LedgerLagBound:  served.ledgerLagBound,
		IndexReadiness:  IndexReadinessBootstrapping,
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
