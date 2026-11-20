package cantonclient

import (
	"slices"
	"time"

	apiv2 "github.com/certusone/wormhole/node/pkg/cantonclient/proto/gen/com/daml/ledger/api/v2"
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

// createdEventsByContractID indexes the CreatedEvents of a transaction. A
// contract id that occurs twice maps to nil, which no exercise accepts.
func createdEventsByContractID(tx *apiv2.Transaction) map[string]*apiv2.CreatedEvent {
	createdEvents := make(map[string]*apiv2.CreatedEvent)
	for _, event := range tx.GetEvents() {
		created := event.GetCreated()
		if created == nil {
			continue
		}
		if _, seen := createdEvents[created.GetContractId()]; seen {
			createdEvents[created.GetContractId()] = nil
			continue
		}
		createdEvents[created.GetContractId()] = created
	}
	return createdEvents
}

// readPartySignsExercisedEmitter reports whether the read-as party is a stakeholder
// of the contract that a consuming exercise on the pinned template consumes.
//
// SECURITY: acs_delta on a consuming exercise is true only when the contract
// existed before the transaction and a read-as party is its stakeholder. For a
// contract that the same transaction creates, acs_delta is false, and the
// CreatedEvent in createdEvents must show the read-as party as a signatory of the
// pinned template. Emitter has no observers, so a stakeholder is a signatory.
// SECURITY: the CreatedEvent must name the exercise's package id, accepted at recordTime.
func readPartySignsExercisedEmitter(exercised *apiv2.ExercisedEvent, createdEvents map[string]*apiv2.CreatedEvent, tmpl TemplateID, recordTime time.Time, readScope ReadScope) bool {
	if !exercised.GetConsuming() {
		return false
	}
	if exercised.GetAcsDelta() {
		return true
	}
	readAsParty, isGuardianGovernance := readScope.GuardianGovernanceParty()
	if !isGuardianGovernance || tmpl.AcceptedPackageIDs.MatchesAnyPackage() {
		return false
	}
	created := createdEvents[exercised.GetContractId()]
	if created == nil {
		return false
	}
	if !templateNameMatches(created.GetTemplateId(), tmpl) {
		return false
	}
	createdPackageID := created.GetTemplateId().GetPackageId()
	if createdPackageID == "" {
		return false
	}
	if createdPackageID != exercised.GetTemplateId().GetPackageId() {
		return false
	}
	if _, isAccepted := tmpl.AcceptedPackageIDs.VerdictForPublish(createdPackageID, recordTime).AcceptedRole(); !isAccepted {
		return false
	}
	return slices.Contains(created.GetSignatories(), readAsParty.String())
}
