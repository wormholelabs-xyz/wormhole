package cantonclient

import (
	"github.com/prometheus/client_golang/prometheus"
	"github.com/prometheus/client_golang/prometheus/promauto"
	"go.uber.org/zap"
)

var publishesWithoutReadPartyStakeholder = promauto.NewCounter(
	prometheus.CounterOpts{
		Name: "wormhole_canton_publish_without_read_party_stakeholder_total",
		Help: "PublishMessage exercises on the pinned template that the watcher dropped because the read-as party is not a stakeholder of the Emitter",
	})

// recordPublishWithoutReadPartyStakeholder logs and counts a publish that the
// read-as party only witnesses. Callers drop the exercise.
func recordPublishWithoutReadPartyStakeholder(updateID string, logger *zap.Logger) {
	publishesWithoutReadPartyStakeholder.Inc()
	if logger == nil {
		return
	}
	logger.Warn("canton: dropped PublishMessage on an Emitter of which the read-as party is not a stakeholder",
		zap.String("updateId", updateID))
}
