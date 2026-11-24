package cantondisclosure

import (
	"sync/atomic"

	"github.com/prometheus/client_golang/prometheus"
	"github.com/prometheus/client_golang/prometheus/promauto"
)

// IndexRebuildReason is the metric label of one index rebuild.
type IndexRebuildReason string

const (
	IndexRebuildReasonInconsistentIndex    IndexRebuildReason = "inconsistent_index"
	IndexRebuildReasonIndexUpdateRefused   IndexRebuildReason = "index_update_refused"
	IndexRebuildReasonPrunedOffset         IndexRebuildReason = "pruned_offset"
	IndexRebuildReasonFailedPrecondition   IndexRebuildReason = "failed_precondition"
	IndexRebuildReasonStreamDecodeFailure  IndexRebuildReason = "stream_decode_failure"
	IndexRebuildReasonReconnectsExhausted  IndexRebuildReason = "reconnects_exhausted"
	IndexRebuildReasonResnapshotDifference IndexRebuildReason = "resnapshot_difference"
	IndexRebuildReasonByteCapRetry         IndexRebuildReason = "byte_cap_retry"
	// IndexRebuildReasonSnapshotReplayNodeLimit: a resnapshot had more ReplayNodes than the byte cap admits.
	IndexRebuildReasonSnapshotReplayNodeLimit IndexRebuildReason = "snapshot_replay_node_limit"
)

// metricsServedIndex is the served index that the gauges read at scrape time; nil reads as zero.
var metricsServedIndex atomic.Pointer[ServedIndex]

// ExportMetrics makes the disclosure gauges read served at each scrape. Call it once per process.
func (served *ServedIndex) ExportMetrics() {
	if served == nil {
		panic("cantondisclosure: export the metrics of a nil served index")
	}
	metricsServedIndex.Store(served)
}

// exportedIndexStatus is the status of the exported served index, and false before its first swap.
func exportedIndexStatus() (IndexStatus, bool) {
	served := metricsServedIndex.Load()
	if served == nil {
		return IndexStatus{}, false
	}
	return served.IndexStatus()
}

// exportedServiceReadiness is the readiness of the exported served index, and false before ExportMetrics.
func exportedServiceReadiness() (ServiceReadiness, bool) {
	served := metricsServedIndex.Load()
	if served == nil {
		return ServiceReadiness{}, false
	}
	return served.ServiceReadiness(), true
}

func indexStatusGaugeFunc(name string, help string, valueOf func(IndexStatus) float64) {
	promauto.NewGaugeFunc(prometheus.GaugeOpts{Name: name, Help: help}, func() float64 {
		status, hasIndex := exportedIndexStatus()
		if !hasIndex {
			return 0
		}
		return valueOf(status)
	})
}

func serviceReadinessGaugeFunc(opts prometheus.GaugeOpts, valueOf func(ServiceReadiness) float64) {
	promauto.NewGaugeFunc(opts, func() float64 {
		readiness, isExported := exportedServiceReadiness()
		if !isExported {
			return 0
		}
		return valueOf(readiness)
	})
}

func gaugeValueOf(condition bool) float64 {
	if condition {
		return 1
	}
	return 0
}

// allServiceReadinessStatuses lists every status that ServiceReadiness.Status returns.
func allServiceReadinessStatuses() []ServiceReadinessStatus {
	statuses := []ServiceReadinessStatus{
		ServiceReadinessStatusReady,
		ServiceReadinessStatusReadyByteCapReached,
		ServiceReadinessStatusBootstrapping,
		ServiceReadinessStatusStreamDisconnected,
		ServiceReadinessStatusLedgerLagAboveBound,
	}
	for _, indexReadiness := range allIndexReadinessValues {
		if indexReadiness == IndexReadinessBootstrapping || indexReadiness.ServesSingletonLookups() {
			continue
		}
		statuses = append(statuses, ServiceReadinessStatus(serviceReadinessStatusIndexPrefix+indexReadiness.String()))
	}
	return statuses
}

func init() {
	indexStatusGaugeFunc("wormhole_canton_disclosure_index_used_bytes", "Bytes that the served core disclosure index charges against its byte cap",
		func(status IndexStatus) float64 { return float64(status.UsedBytes) })
	indexStatusGaugeFunc("wormhole_canton_disclosure_index_byte_cap_bytes", "Byte cap of the core disclosure index",
		func(status IndexStatus) float64 { return float64(status.ByteCapBytes) })
	indexStatusGaugeFunc("wormhole_canton_disclosure_index_offset", "Participant offset of the served core disclosure index",
		func(status IndexStatus) float64 { return float64(status.Offset) })
	indexStatusGaugeFunc("wormhole_canton_disclosure_index_live_contracts", "Live core contracts in the served core disclosure index",
		func(status IndexStatus) float64 { return float64(status.LiveContractCount) })
	indexStatusGaugeFunc("wormhole_canton_disclosure_index_ignored_unknown_archives", "Archives of unknown contract ids that the served index ignored since its bootstrap",
		func(status IndexStatus) float64 { return float64(status.IgnoredUnknownArchiveCount) })
	indexStatusGaugeFunc("wormhole_canton_disclosure_index_dropped_replay_node_updates", "ReplayNode creates and archives that the served index skipped at the byte cap since its bootstrap",
		func(status IndexStatus) float64 { return float64(status.DroppedReplayNodeUpdateCount) })
	for _, indexReadiness := range allIndexReadinessValues {
		serviceReadinessGaugeFunc(prometheus.GaugeOpts{
			Name:        "wormhole_canton_disclosure_index_readiness",
			Help:        "1 for the readiness of the served core disclosure index, 0 for each other readiness",
			ConstLabels: prometheus.Labels{"readiness": indexReadiness.String()},
		}, func(readiness ServiceReadiness) float64 {
			return gaugeValueOf(readiness.IndexReadiness == indexReadiness)
		})
	}
	for _, serviceStatus := range allServiceReadinessStatuses() {
		serviceReadinessGaugeFunc(prometheus.GaugeOpts{
			Name:        "wormhole_canton_disclosure_service_readiness",
			Help:        "1 for the /v1/readyz status of the disclosure service, 0 for each other status",
			ConstLabels: prometheus.Labels{"status": string(serviceStatus)},
		}, func(readiness ServiceReadiness) float64 { return gaugeValueOf(readiness.Status() == serviceStatus) })
	}
	serviceReadinessGaugeFunc(prometheus.GaugeOpts{
		Name: "wormhole_canton_disclosure_ledger_lag_seconds",
		Help: "Age of the newest ledger end read that the index offset reached",
	}, func(readiness ServiceReadiness) float64 { return readiness.LedgerLag.Seconds() })
	serviceReadinessGaugeFunc(prometheus.GaugeOpts{
		Name: "wormhole_canton_disclosure_stream_connected",
		Help: "1 while the core contract update stream is connected",
	}, func(readiness ServiceReadiness) float64 { return gaugeValueOf(readiness.StreamConnected) })
	promauto.NewGaugeFunc(prometheus.GaugeOpts{
		Name: "wormhole_canton_disclosure_ledger_lag_offsets",
		Help: "Latest observed ledger end minus the index offset",
	}, func() float64 {
		served := metricsServedIndex.Load()
		if served == nil {
			return 0
		}
		return float64(served.LedgerOffsetsBehind())
	})
}

var (
	indexRebuildsCounter = promauto.NewCounterVec(prometheus.CounterOpts{
		Name: "wormhole_canton_disclosure_index_rebuilds_total",
		Help: "Core disclosure index rebuilds by reason",
	}, []string{"reason"})
	indexBuildFailuresCounter = promauto.NewCounter(prometheus.CounterOpts{
		Name: "wormhole_canton_disclosure_index_build_failures_total",
		Help: "Failed attempts to build a core disclosure index from a snapshot",
	})
	streamReconnectsCounter = promauto.NewCounter(prometheus.CounterOpts{
		Name: "wormhole_canton_disclosure_stream_reconnects_total",
		Help: "Reconnects of the core contract update stream",
	})
	resnapshotDifferencesCounter = promauto.NewCounterVec(prometheus.CounterOpts{
		Name: "wormhole_canton_disclosure_resnapshot_differences_total",
		Help: "Differences between a periodic snapshot and the live index, by kind",
	}, []string{"kind"})
	resnapshotFailuresCounter = promauto.NewCounter(prometheus.CounterOpts{
		Name: "wormhole_canton_disclosure_resnapshot_failures_total",
		Help: "Periodic snapshots that failed to read or compare",
	})
	ledgerEndReadFailuresCounter = promauto.NewCounter(prometheus.CounterOpts{
		Name: "wormhole_canton_disclosure_ledger_end_read_failures_total",
		Help: "Failed ledger end reads of the lag measurement",
	})
)

// allIndexReadinessValues lists every IndexReadiness for the readiness gauges.
var allIndexReadinessValues = [...]IndexReadiness{
	IndexReadinessBootstrapping,
	IndexReadinessReady,
	IndexReadinessReadyNearByteCap,
	IndexReadinessSingletonMissing,
	IndexReadinessSingletonConflict,
	IndexReadinessByteCapReached,
	IndexReadinessInconsistent,
	IndexReadinessResnapshotDifference,
}

func recordSnapshotComparisonMetrics(comparison SnapshotComparison) {
	resnapshotDifferencesCounter.WithLabelValues(SnapshotDifferenceMissingFromIndex.String()).Add(float64(comparison.MissingFromIndexCount()))
	resnapshotDifferencesCounter.WithLabelValues(SnapshotDifferenceStaleInIndex.String()).Add(float64(comparison.StaleInIndexCount()))
	resnapshotDifferencesCounter.WithLabelValues(SnapshotDifferenceContentMismatch.String()).Add(float64(comparison.ContentMismatchCount()))
}
