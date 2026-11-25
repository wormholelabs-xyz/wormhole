package cantondisclosure

import (
	"errors"
	"fmt"

	"github.com/prometheus/client_golang/prometheus"
	"github.com/prometheus/client_golang/prometheus/collectors"
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
	// IndexRebuildReasonReadPartyRehosted: the read party became hosted again after the snapshot of the served index.
	IndexRebuildReasonReadPartyRehosted IndexRebuildReason = "read_party_rehosted"
)

// NewMetricsRegistry returns a new registry with the disclosure metrics of served and the Go runtime and process
// metrics. The service serves it on its status address; only these registries hold the disclosure metrics.
// Each call returns a separate registry; the counters are shared by every registry of the process.
func (served *ServedIndex) NewMetricsRegistry() (*prometheus.Registry, error) {
	if served == nil {
		return nil, errors.New("cantondisclosure: metrics registry of a nil served index")
	}
	registry := prometheus.NewRegistry()
	registeredCollectors := []prometheus.Collector{
		servedIndexMetricsCollector{served: served},
		collectors.NewGoCollector(),
		collectors.NewProcessCollector(collectors.ProcessCollectorOpts{}),
		readPartyHostingReadFailuresCounter,
		indexRebuildsCounter,
		indexBuildFailuresCounter,
		streamReconnectsCounter,
		resnapshotDifferencesCounter,
		resnapshotFailuresCounter,
		ledgerEndReadFailuresCounter,
	}
	for _, collector := range registeredCollectors {
		if err := registry.Register(collector); err != nil {
			return nil, fmt.Errorf("cantondisclosure: register a disclosure metric: %w", err)
		}
	}
	return registry, nil
}

func newGaugeDesc(name string, help string, variableLabels ...string) *prometheus.Desc {
	return prometheus.NewDesc(name, help, variableLabels, nil)
}

var (
	indexUsedBytesDesc               = newGaugeDesc("wormhole_canton_disclosure_index_used_bytes", "Bytes that the served core disclosure index charges against its byte cap")
	indexByteCapBytesDesc            = newGaugeDesc("wormhole_canton_disclosure_index_byte_cap_bytes", "Byte cap of the core disclosure index")
	indexOffsetDesc                  = newGaugeDesc("wormhole_canton_disclosure_index_offset", "Participant offset of the served core disclosure index")
	indexLiveContractsDesc           = newGaugeDesc("wormhole_canton_disclosure_index_live_contracts", "Live core contracts in the served core disclosure index")
	indexIgnoredUnknownArchivesDesc  = newGaugeDesc("wormhole_canton_disclosure_index_ignored_unknown_archives", "Archives of unknown contract ids that the served index ignored since its bootstrap")
	indexDroppedReplayNodeUpdateDesc = newGaugeDesc("wormhole_canton_disclosure_index_dropped_replay_node_updates", "ReplayNode creates and archives that the served index skipped at the byte cap since its bootstrap")
	indexReadinessDesc               = newGaugeDesc("wormhole_canton_disclosure_index_readiness", "1 for the readiness of the served core disclosure index, 0 for each other readiness", "readiness")
	serviceReadinessDesc             = newGaugeDesc("wormhole_canton_disclosure_service_readiness", "1 for the /v1/readyz status of the disclosure service, 0 for each other status", "status")
	ledgerLagSecondsDesc             = newGaugeDesc("wormhole_canton_disclosure_ledger_lag_seconds", "Age of the newest ledger end read that the index offset reached")
	streamConnectedDesc              = newGaugeDesc("wormhole_canton_disclosure_stream_connected", "1 while the core contract update stream is connected")
	ledgerLagOffsetsDesc             = newGaugeDesc("wormhole_canton_disclosure_ledger_lag_offsets", "Latest observed ledger end minus the index offset")
	readPartyHostedDesc              = newGaugeDesc("wormhole_canton_disclosure_read_party_hosted", "1 while a connected synchronizer hosts the read party on the participant")
)

// servedIndexMetricsCollector reads the served index once per scrape, so that the gauges of one scrape agree.
type servedIndexMetricsCollector struct {
	served *ServedIndex
}

func (collector servedIndexMetricsCollector) Describe(descs chan<- *prometheus.Desc) {
	for _, desc := range []*prometheus.Desc{
		indexUsedBytesDesc, indexByteCapBytesDesc, indexOffsetDesc, indexLiveContractsDesc, indexIgnoredUnknownArchivesDesc,
		indexDroppedReplayNodeUpdateDesc, indexReadinessDesc, serviceReadinessDesc, ledgerLagSecondsDesc, streamConnectedDesc,
		ledgerLagOffsetsDesc, readPartyHostedDesc,
	} {
		descs <- desc
	}
}

// Collect emits a zero index status before the first swap.
func (collector servedIndexMetricsCollector) Collect(metrics chan<- prometheus.Metric) {
	snapshot := collector.served.readMetricsSnapshot()
	gauge := func(desc *prometheus.Desc, gaugeValue float64, labelValues ...string) {
		metrics <- prometheus.MustNewConstMetric(desc, prometheus.GaugeValue, gaugeValue, labelValues...)
	}
	gauge(indexUsedBytesDesc, float64(snapshot.indexStatus.UsedBytes))
	gauge(indexByteCapBytesDesc, float64(snapshot.indexStatus.ByteCapBytes))
	gauge(indexOffsetDesc, float64(snapshot.indexStatus.Offset))
	gauge(indexLiveContractsDesc, float64(snapshot.indexStatus.LiveContractCount))
	gauge(indexIgnoredUnknownArchivesDesc, float64(snapshot.indexStatus.IgnoredUnknownArchiveCount))
	gauge(indexDroppedReplayNodeUpdateDesc, float64(snapshot.indexStatus.DroppedReplayNodeUpdateCount))
	for _, indexReadiness := range allIndexReadinessValues {
		gauge(indexReadinessDesc, gaugeValueOf(snapshot.readiness.IndexReadiness == indexReadiness), indexReadiness.String())
	}
	serviceStatus := snapshot.readiness.Status()
	for _, listedStatus := range allServiceReadinessStatuses() {
		gauge(serviceReadinessDesc, gaugeValueOf(serviceStatus == listedStatus), string(listedStatus))
	}
	gauge(ledgerLagSecondsDesc, snapshot.readiness.LedgerLag.Seconds())
	gauge(streamConnectedDesc, gaugeValueOf(snapshot.readiness.StreamConnected))
	gauge(ledgerLagOffsetsDesc, float64(snapshot.ledgerOffsetsBehind))
	gauge(readPartyHostedDesc, gaugeValueOf(snapshot.readiness.ReadPartyHosted))
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
		ServiceReadinessStatusReadPartyNotHosted,
		ServiceReadinessStatusReadPartyHostingUnknown,
		ServiceReadinessStatusReadPartyRehosted,
	}
	for _, indexReadiness := range allIndexReadinessValues {
		if indexReadiness == IndexReadinessBootstrapping || indexReadiness.ServesSingletonLookups() {
			continue
		}
		statuses = append(statuses, ServiceReadinessStatus(serviceReadinessStatusIndexPrefix+indexReadiness.String()))
	}
	return statuses
}

// The counters are registered by NewMetricsRegistry only.
var (
	readPartyHostingReadFailuresCounter = prometheus.NewCounter(prometheus.CounterOpts{
		Name: "wormhole_canton_disclosure_read_party_hosting_read_failures_total",
		Help: "Failed reads of the hosting of the read party",
	})
	indexRebuildsCounter = prometheus.NewCounterVec(prometheus.CounterOpts{
		Name: "wormhole_canton_disclosure_index_rebuilds_total",
		Help: "Core disclosure index rebuilds by reason",
	}, []string{"reason"})
	indexBuildFailuresCounter = prometheus.NewCounter(prometheus.CounterOpts{
		Name: "wormhole_canton_disclosure_index_build_failures_total",
		Help: "Failed attempts to build a core disclosure index from a snapshot",
	})
	streamReconnectsCounter = prometheus.NewCounter(prometheus.CounterOpts{
		Name: "wormhole_canton_disclosure_stream_reconnects_total",
		Help: "Reconnects of the core contract update stream",
	})
	resnapshotDifferencesCounter = prometheus.NewCounterVec(prometheus.CounterOpts{
		Name: "wormhole_canton_disclosure_resnapshot_differences_total",
		Help: "Differences between a periodic snapshot and the live index, by kind",
	}, []string{"kind"})
	resnapshotFailuresCounter = prometheus.NewCounter(prometheus.CounterOpts{
		Name: "wormhole_canton_disclosure_resnapshot_failures_total",
		Help: "Periodic snapshots that failed to read or compare",
	})
	ledgerEndReadFailuresCounter = prometheus.NewCounter(prometheus.CounterOpts{
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
