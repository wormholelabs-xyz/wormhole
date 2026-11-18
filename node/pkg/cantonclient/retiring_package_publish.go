package cantonclient

import (
	"time"

	"github.com/prometheus/client_golang/prometheus"
	"github.com/prometheus/client_golang/prometheus/promauto"
	"go.uber.org/zap"
)

// No labels: at most one retiring id is configured.
var retiringPackagePublishesAfterDeadline = promauto.NewCounter(
	prometheus.CounterOpts{
		Name: "wormhole_canton_retiring_package_publish_after_deadline_total",
		Help: "PublishMessage exercises by the retiring core package that the watcher dropped because the transaction record time is at or after --cantonRetiringPackageAcceptedUntil",
	})

var publishesWithoutRecordTime = promauto.NewCounter(
	prometheus.CounterOpts{
		Name: "wormhole_canton_publish_without_record_time_total",
		Help: "PublishMessage exercises by the retiring core package that the watcher dropped because the transaction has no record time",
	})

// recordRetiringPackagePublishAfterDeadline logs and counts a dropped retiring-core publish.
func recordRetiringPackagePublishAfterDeadline(updateID string, executingPackageID string, recordTime time.Time, logger *zap.Logger) {
	retiringPackagePublishesAfterDeadline.Inc()
	if logger == nil {
		return
	}
	logger.Warn("canton: dropped PublishMessage executed by the retiring core package at or after its deadline",
		zap.String("updateId", updateID),
		zap.String("executingPackageId", executingPackageID),
		zap.Time("recordTime", recordTime))
}

// recordPublishWithoutRecordTime logs and counts a retiring-core publish the deadline cannot be checked for.
func recordPublishWithoutRecordTime(updateID string, executingPackageID string, logger *zap.Logger) {
	publishesWithoutRecordTime.Inc()
	if logger == nil {
		return
	}
	logger.Warn("canton: dropped PublishMessage executed by the retiring core package in a transaction without record time",
		zap.String("updateId", updateID),
		zap.String("executingPackageId", executingPackageID))
}
