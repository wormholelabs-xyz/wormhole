package cantondisclosure

import (
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

var (
	indexUsedBytesGauge = promauto.NewGauge(prometheus.GaugeOpts{
		Name: "wormhole_canton_disclosure_index_used_bytes",
		Help: "Bytes that the served core disclosure index charges against its byte cap",
	})
	indexByteCapBytesGauge = promauto.NewGauge(prometheus.GaugeOpts{
		Name: "wormhole_canton_disclosure_index_byte_cap_bytes",
		Help: "Byte cap of the core disclosure index",
	})
	indexReadinessGauge = promauto.NewGaugeVec(prometheus.GaugeOpts{
		Name: "wormhole_canton_disclosure_index_readiness",
		Help: "1 for the readiness of the served core disclosure index, 0 for each other readiness",
	}, []string{"readiness"})
	indexOffsetGauge = promauto.NewGauge(prometheus.GaugeOpts{
		Name: "wormhole_canton_disclosure_index_offset",
		Help: "Participant offset of the served core disclosure index",
	})
	indexLiveContractsGauge = promauto.NewGauge(prometheus.GaugeOpts{
		Name: "wormhole_canton_disclosure_index_live_contracts",
		Help: "Live core contracts in the served core disclosure index",
	})
	indexIgnoredUnknownArchivesGauge = promauto.NewGauge(prometheus.GaugeOpts{
		Name: "wormhole_canton_disclosure_index_ignored_unknown_archives",
		Help: "Archives of unknown contract ids that the served index ignored since its bootstrap",
	})
	indexDroppedReplayNodeUpdatesGauge = promauto.NewGauge(prometheus.GaugeOpts{
		Name: "wormhole_canton_disclosure_index_dropped_replay_node_updates",
		Help: "ReplayNode creates and archives that the served index skipped at the byte cap since its bootstrap",
	})
	ledgerLagSecondsGauge = promauto.NewGauge(prometheus.GaugeOpts{
		Name: "wormhole_canton_disclosure_ledger_lag_seconds",
		Help: "Upper bound of the time since the index offset reached an observed ledger end",
	})
	ledgerLagOffsetsGauge = promauto.NewGauge(prometheus.GaugeOpts{
		Name: "wormhole_canton_disclosure_ledger_lag_offsets",
		Help: "Latest observed ledger end minus the index offset",
	})
	streamConnectedGauge = promauto.NewGauge(prometheus.GaugeOpts{
		Name: "wormhole_canton_disclosure_stream_connected",
		Help: "1 while the core contract update stream is connected",
	})
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

// allIndexReadinessValues lists every IndexReadiness for the readiness gauge.
var allIndexReadinessValues = [...]IndexReadiness{
	IndexReadinessBootstrapping,
	IndexReadinessReady,
	IndexReadinessReadyNearByteCap,
	IndexReadinessSingletonMissing,
	IndexReadinessSingletonConflict,
	IndexReadinessByteCapReached,
	IndexReadinessInconsistent,
}

// publishIndexMetrics sets every disclosure gauge from served.
func publishIndexMetrics(served *ServedIndex) {
	readiness := served.ServiceReadiness()
	if status, hasIndex := served.IndexStatus(); hasIndex {
		indexUsedBytesGauge.Set(float64(status.UsedBytes))
		indexByteCapBytesGauge.Set(float64(status.ByteCapBytes))
		indexOffsetGauge.Set(float64(status.Offset))
		indexLiveContractsGauge.Set(float64(status.LiveContractCount))
		indexIgnoredUnknownArchivesGauge.Set(float64(status.IgnoredUnknownArchiveCount))
		indexDroppedReplayNodeUpdatesGauge.Set(float64(status.DroppedReplayNodeUpdateCount))
	}
	for _, indexReadiness := range allIndexReadinessValues {
		gaugeValue := 0.0
		if indexReadiness == readiness.IndexReadiness {
			gaugeValue = 1
		}
		indexReadinessGauge.WithLabelValues(indexReadiness.String()).Set(gaugeValue)
	}
	ledgerLagSecondsGauge.Set(readiness.LedgerLag.Seconds())
	ledgerLagOffsetsGauge.Set(float64(served.LedgerOffsetsBehind()))
	streamConnectedValue := 0.0
	if readiness.StreamConnected {
		streamConnectedValue = 1
	}
	streamConnectedGauge.Set(streamConnectedValue)
}

func recordSnapshotComparisonMetrics(comparison SnapshotComparison) {
	resnapshotDifferencesCounter.WithLabelValues(SnapshotDifferenceMissingFromIndex.String()).Add(float64(comparison.MissingFromIndexCount()))
	resnapshotDifferencesCounter.WithLabelValues(SnapshotDifferenceStaleInIndex.String()).Add(float64(comparison.StaleInIndexCount()))
	resnapshotDifferencesCounter.WithLabelValues(SnapshotDifferenceContentMismatch.String()).Add(float64(comparison.ContentMismatchCount()))
}
