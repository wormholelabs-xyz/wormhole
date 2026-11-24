package cantondisclosure

import (
	"testing"
	"time"

	"github.com/prometheus/client_golang/prometheus"
	"github.com/stretchr/testify/require"
)

// gatheredGaugeValue is the value of the gauge name with label labelName=labelValue, or without labels when labelName is empty.
func gatheredGaugeValue(t *testing.T, name string, labelName string, labelValue string) float64 {
	t.Helper()
	metricFamilies, err := prometheus.DefaultGatherer.Gather()
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
	served.ExportMetrics()
	t.Cleanup(func() { metricsServedIndex.Store(nil) })
	require.Equal(t, 1.0, gatheredGaugeValue(t, "wormhole_canton_disclosure_service_readiness", "status", string(ServiceReadinessStatusBootstrapping)))

	swapTestIndex(served, newBootstrappedTestIndex(t, testLargeIndexByteCap), clock)
	served.setStreamConnected(true)
	require.Equal(t, 1.0, gatheredGaugeValue(t, "wormhole_canton_disclosure_service_readiness", "status", string(ServiceReadinessStatusReady)))
	require.Equal(t, 0.0, gatheredGaugeValue(t, "wormhole_canton_disclosure_ledger_lag_seconds", "", ""))
	require.Equal(t, float64(testBootstrapOffset), gatheredGaugeValue(t, "wormhole_canton_disclosure_index_offset", "", ""))
	require.Equal(t, 1.0, gatheredGaugeValue(t, "wormhole_canton_disclosure_index_readiness", "readiness", IndexReadinessReady.String()))
	require.Equal(t, 1.0, gatheredGaugeValue(t, "wormhole_canton_disclosure_stream_connected", "", ""))
	require.Equal(t, 1.0, gatheredGaugeValue(t, "wormhole_canton_disclosure_read_party_hosted", "", ""))

	served.setReadPartyHosted(false)
	require.Equal(t, 0.0, gatheredGaugeValue(t, "wormhole_canton_disclosure_read_party_hosted", "", ""))
	require.Equal(t, 1.0, gatheredGaugeValue(t, "wormhole_canton_disclosure_service_readiness", "status", string(ServiceReadinessStatusReadPartyNotHosted)))
	served.setReadPartyHosted(true)

	clock.current = clock.current.Add(90 * time.Second)
	served.recordLedgerEnd(testBootstrapOffset+4, clock.current)
	require.Equal(t, 90.0, gatheredGaugeValue(t, "wormhole_canton_disclosure_ledger_lag_seconds", "", ""))
	require.Equal(t, 4.0, gatheredGaugeValue(t, "wormhole_canton_disclosure_ledger_lag_offsets", "", ""))
	require.Equal(t, 1.0, gatheredGaugeValue(t, "wormhole_canton_disclosure_service_readiness", "status", string(ServiceReadinessStatusLedgerLagAboveBound)))
	require.Equal(t, 0.0, gatheredGaugeValue(t, "wormhole_canton_disclosure_service_readiness", "status", string(ServiceReadinessStatusReady)))

	served.setStreamConnected(false)
	require.Equal(t, 0.0, gatheredGaugeValue(t, "wormhole_canton_disclosure_stream_connected", "", ""))
	require.Equal(t, 1.0, gatheredGaugeValue(t, "wormhole_canton_disclosure_service_readiness", "status", string(ServiceReadinessStatusStreamDisconnected)))
}

func TestAllServiceReadinessStatuses(t *testing.T) {
	require.Equal(t, []ServiceReadinessStatus{
		"ready", "ready_byte_cap_reached", "bootstrapping", "stream_disconnected", "ledger_lag_above_bound",
		"read_party_not_hosted", "index_singleton_missing", "index_singleton_conflict", "index_inconsistent", "index_resnapshot_difference",
	}, allServiceReadinessStatuses())
}
