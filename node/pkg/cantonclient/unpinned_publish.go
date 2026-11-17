package cantonclient

import (
	"sync"

	"github.com/prometheus/client_golang/prometheus"
	"github.com/prometheus/client_golang/prometheus/promauto"
	"go.uber.org/zap"
)

// maxDistinctUnpinnedPackageLabels bounds the executing_package_id label set.
// Any party can upload a package that declares the pinned module and entity,
// so the set of observed package ids is attacker-influenced.
const maxDistinctUnpinnedPackageLabels = 8

// unpinnedPackageOverflowLabel labels package ids seen after the cap. Package
// ids are hex hashes, so it cannot collide with one.
const unpinnedPackageOverflowLabel = "other"

var unpinnedPublishMessages = promauto.NewCounterVec(
	prometheus.CounterOpts{
		Name: "wormhole_canton_unpinned_publish_messages_total",
		Help: "PublishMessage exercises on the pinned template that the watcher dropped because a package other than --cantonPackageID or --cantonRetiringPackageID executed them",
	}, []string{"executing_package_id"})

// unpinnedPackageLabels assigns each executing package id its own label until
// maxDistinctUnpinnedPackageLabels ids are labeled, then the overflow label.
type unpinnedPackageLabels struct {
	mu                sync.Mutex
	labeledPackageIDs map[string]struct{}
}

func newUnpinnedPackageLabels() *unpinnedPackageLabels {
	return &unpinnedPackageLabels{labeledPackageIDs: make(map[string]struct{}, maxDistinctUnpinnedPackageLabels)}
}

func (labels *unpinnedPackageLabels) labelForPackageID(executingPackageID string) string {
	labels.mu.Lock()
	defer labels.mu.Unlock()
	if _, labeled := labels.labeledPackageIDs[executingPackageID]; labeled {
		return executingPackageID
	}
	if len(labels.labeledPackageIDs) >= maxDistinctUnpinnedPackageLabels {
		return unpinnedPackageOverflowLabel
	}
	labels.labeledPackageIDs[executingPackageID] = struct{}{}
	return executingPackageID
}

var unpinnedPublishPackageLabels = newUnpinnedPackageLabels()

// recordUnpinnedPublish logs and counts an exercise of the watched choice on
// the pinned module and entity under a package id outside the accepted set.
// Callers drop the exercise: the package pin is a security boundary.
func recordUnpinnedPublish(updateID string, executingPackageID string, acceptedPackageIDs AcceptedPackageIDs, logger *zap.Logger) {
	unpinnedPublishMessages.WithLabelValues(unpinnedPublishPackageLabels.labelForPackageID(executingPackageID)).Inc()
	if logger == nil {
		return
	}
	logger.Warn("canton: dropped PublishMessage executed by a package outside the accepted core package ids",
		zap.String("updateId", updateID),
		zap.String("executingPackageId", executingPackageID),
		zap.Stringer("acceptedPackageIds", acceptedPackageIDs))
}
