package cantondisclosure

import (
	"strings"
	"sync"
	"testing"
	"time"

	"github.com/prometheus/client_golang/prometheus"
	dto "github.com/prometheus/client_model/go"
	"github.com/stretchr/testify/require"
)

const disclosureMetricPrefix = "wormhole_canton_disclosure_"

func newTestMetricsRegistry(t *testing.T, served *ServedIndex) *prometheus.Registry {
	t.Helper()
	registry, err := served.NewMetricsRegistry()
	require.NoError(t, err)
	return registry
}

// gatheredGaugeValue is the value of the gauge name with label labelName=labelValue, or without labels when labelName is empty.
func gatheredGaugeValue(t *testing.T, registry *prometheus.Registry, name string, labelName string, labelValue string) float64 {
	t.Helper()
	metricFamilies, err := registry.Gather()
	require.NoError(t, err)
	for _, metricFamily := range metricFamilies {
		if metricFamily.GetName() != name {
			continue
		}
		for _, metric := range metricFamily.GetMetric() {
			if labelName == "" {
				return metric.GetGauge().GetValue()
			}
			for _, label := range metric.GetLabel() {
				if label.GetName() == labelName && label.GetValue() == labelValue {
					return metric.GetGauge().GetValue()
				}
			}
		}
	}
	t.Fatalf("gauge %s{%s=%q} not gathered", name, labelName, labelValue)
	return 0
}

func TestGaugesReadTheServedIndexAtScrapeTime(t *testing.T) {
	served, clock := newTestServedIndex(t)
	registry := newTestMetricsRegistry(t, served)
	require.Equal(t, 1.0, gatheredGaugeValue(t, registry, "wormhole_canton_disclosure_service_readiness", "status", string(ServiceReadinessStatusBootstrapping)))

	swapTestIndex(served, newBootstrappedTestIndex(t, testLargeIndexByteCap), clock)
	served.setStreamConnected(true)
	require.Equal(t, 1.0, gatheredGaugeValue(t, registry, "wormhole_canton_disclosure_service_readiness", "status", string(ServiceReadinessStatusReady)))
	require.Equal(t, 0.0, gatheredGaugeValue(t, registry, "wormhole_canton_disclosure_ledger_lag_seconds", "", ""))
	require.Equal(t, float64(testBootstrapOffset), gatheredGaugeValue(t, registry, "wormhole_canton_disclosure_index_offset", "", ""))
	require.Equal(t, 1.0, gatheredGaugeValue(t, registry, "wormhole_canton_disclosure_index_readiness", "readiness", IndexReadinessReady.String()))
	require.Equal(t, 1.0, gatheredGaugeValue(t, registry, "wormhole_canton_disclosure_stream_connected", "", ""))
	require.Equal(t, 1.0, gatheredGaugeValue(t, registry, "wormhole_canton_disclosure_read_party_hosted", "", ""))

	served.setReadPartyHosted(false)
	require.Equal(t, 0.0, gatheredGaugeValue(t, registry, "wormhole_canton_disclosure_read_party_hosted", "", ""))
	require.Equal(t, 1.0, gatheredGaugeValue(t, registry, "wormhole_canton_disclosure_service_readiness", "status", string(ServiceReadinessStatusReadPartyNotHosted)))
	served.setReadPartyHosted(true)
	require.Equal(t, 1.0, gatheredGaugeValue(t, registry, "wormhole_canton_disclosure_service_readiness", "status", string(ServiceReadinessStatusReadPartyRehosted)))
	swapTestIndex(served, newBootstrappedTestIndex(t, testLargeIndexByteCap), clock)

	clock.current = clock.current.Add(90 * time.Second)
	served.recordLedgerEnd(testBootstrapOffset+4, clock.current)
	served.setReadPartyHosted(true)
	require.Equal(t, 90.0, gatheredGaugeValue(t, registry, "wormhole_canton_disclosure_ledger_lag_seconds", "", ""))
	require.Equal(t, 4.0, gatheredGaugeValue(t, registry, "wormhole_canton_disclosure_ledger_lag_offsets", "", ""))
	require.Equal(t, 1.0, gatheredGaugeValue(t, registry, "wormhole_canton_disclosure_service_readiness", "status", string(ServiceReadinessStatusLedgerLagAboveBound)))
	require.Equal(t, 0.0, gatheredGaugeValue(t, registry, "wormhole_canton_disclosure_service_readiness", "status", string(ServiceReadinessStatusReady)))

	served.setStreamConnected(false)
	require.Equal(t, 0.0, gatheredGaugeValue(t, registry, "wormhole_canton_disclosure_stream_connected", "", ""))
	require.Equal(t, 1.0, gatheredGaugeValue(t, registry, "wormhole_canton_disclosure_service_readiness", "status", string(ServiceReadinessStatusStreamDisconnected)))
}

func TestDefaultRegistryHoldsNoDisclosureMetric(t *testing.T) {
	served, _ := newTestServedIndex(t)
	newTestMetricsRegistry(t, served)
	indexRebuildsCounter.WithLabelValues(string(IndexRebuildReasonByteCapRetry)).Add(0)
	metricFamilies, err := prometheus.DefaultGatherer.Gather()
	require.NoError(t, err)
	for _, metricFamily := range metricFamilies {
		require.False(t, strings.HasPrefix(metricFamily.GetName(), disclosureMetricPrefix), "guardiand node exports %s", metricFamily.GetName())
	}
}

func TestEachMetricsRegistryGathersEveryDisclosureMetric(t *testing.T) {
	served, _ := newTestServedIndex(t)
	indexRebuildsCounter.WithLabelValues(string(IndexRebuildReasonByteCapRetry)).Add(0)
	resnapshotDifferencesCounter.WithLabelValues(SnapshotDifferenceStaleInIndex.String()).Add(0)
	expectedNames := []string{
		"wormhole_canton_disclosure_index_used_bytes", "wormhole_canton_disclosure_index_byte_cap_bytes", "wormhole_canton_disclosure_index_offset",
		"wormhole_canton_disclosure_index_live_contracts", "wormhole_canton_disclosure_index_ignored_unknown_archives",
		"wormhole_canton_disclosure_index_dropped_replay_node_updates", "wormhole_canton_disclosure_index_readiness",
		"wormhole_canton_disclosure_service_readiness", "wormhole_canton_disclosure_ledger_lag_seconds", "wormhole_canton_disclosure_stream_connected",
		"wormhole_canton_disclosure_ledger_lag_offsets", "wormhole_canton_disclosure_read_party_hosted",
		"wormhole_canton_disclosure_read_party_hosting_read_failures_total", "wormhole_canton_disclosure_index_rebuilds_total",
		"wormhole_canton_disclosure_index_build_failures_total", "wormhole_canton_disclosure_stream_reconnects_total",
		"wormhole_canton_disclosure_resnapshot_differences_total", "wormhole_canton_disclosure_resnapshot_failures_total",
		"wormhole_canton_disclosure_ledger_end_read_failures_total",
	}
	for registryIndex := 0; registryIndex < 2; registryIndex++ {
		metricFamilies, err := newTestMetricsRegistry(t, served).Gather()
		require.NoError(t, err)
		gatheredNames := []string{}
		hasGoMetrics := false
		for _, metricFamily := range metricFamilies {
			if strings.HasPrefix(metricFamily.GetName(), disclosureMetricPrefix) {
				gatheredNames = append(gatheredNames, metricFamily.GetName())
			}
			hasGoMetrics = hasGoMetrics || metricFamily.GetName() == "go_goroutines"
		}
		require.ElementsMatch(t, expectedNames, gatheredNames, "registry %d", registryIndex)
		require.True(t, hasGoMetrics, "registry %d", registryIndex)
	}
	_, err := (*ServedIndex)(nil).NewMetricsRegistry()
	require.Error(t, err)
}

// requireOneSeriesAtOne requires exactly one series of the family at 1 and every other series at 0.
func requireOneSeriesAtOne(t *testing.T, metricFamily *dto.MetricFamily) {
	t.Helper()
	seriesAtOne := 0
	for _, metric := range metricFamily.GetMetric() {
		switch metric.GetGauge().GetValue() {
		case 1:
			seriesAtOne++
		case 0:
		default:
			t.Fatalf("%s has a series at %v", metricFamily.GetName(), metric.GetGauge().GetValue())
		}
	}
	require.Equal(t, 1, seriesAtOne, metricFamily.GetName())
}

func TestReadinessFamiliesAgreeWithinOneScrapeDuringTransitions(t *testing.T) {
	served, clock := newTestServedIndex(t)
	swapTestIndex(served, newBootstrappedTestIndex(t, testLargeIndexByteCap), clock)
	registry := newTestMetricsRegistry(t, served)
	const transitionCount = 2000
	var transitions sync.WaitGroup
	transitions.Add(1)
	go func() {
		defer transitions.Done()
		for transitionIndex := 0; transitionIndex < transitionCount; transitionIndex++ {
			served.setStreamConnected(transitionIndex%2 == 0)
			served.setReadPartyHosted(transitionIndex%3 != 0)
		}
	}()
	for scrapeIndex := 0; scrapeIndex < 200; scrapeIndex++ {
		metricFamilies, err := registry.Gather()
		require.NoError(t, err)
		for _, metricFamily := range metricFamilies {
			switch metricFamily.GetName() {
			case "wormhole_canton_disclosure_service_readiness", "wormhole_canton_disclosure_index_readiness":
				requireOneSeriesAtOne(t, metricFamily)
			}
		}
	}
	transitions.Wait()
}

func TestAllServiceReadinessStatuses(t *testing.T) {
	require.Equal(t, []ServiceReadinessStatus{
		"ready", "ready_byte_cap_reached", "bootstrapping", "stream_disconnected", "ledger_lag_above_bound",
		"read_party_not_hosted", "read_party_hosting_unknown", "read_party_rehosted",
		"index_singleton_missing", "index_singleton_conflict", "index_inconsistent", "index_resnapshot_difference",
	}, allServiceReadinessStatuses())
}
