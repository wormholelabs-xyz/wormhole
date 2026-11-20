package cantonclient

import (
	"testing"
	"time"

	"github.com/stretchr/testify/assert"
	"github.com/stretchr/testify/require"

	apiv2 "github.com/certusone/wormhole/node/pkg/cantonclient/proto/gen/com/daml/ledger/api/v2"
)

func TestReadPartySignsExercisedEmitter(t *testing.T) {
	realTransientEmitter := emitterCreatedEvent("emitter-transient", pinnedPackageID, "Emitter", realEmitterSignatories, nil).GetCreated()
	unpinnedTmpl := pinnedTmpl
	unpinnedTmpl.AcceptedPackageIDs = AcceptAnyPackageID()
	cases := []struct {
		name          string
		consuming     bool
		acsDelta      bool
		contractID    string
		createdEvents map[string]*apiv2.CreatedEvent
		tmpl          TemplateID
		readScope     ReadScope
		wantAccepted  bool
	}{
		{name: "consuming exercise with acs_delta is accepted", consuming: true, acsDelta: true, contractID: "emitter-active",
			tmpl: pinnedTmpl, readScope: governanceReadScope, wantAccepted: true},
		{name: "non-consuming exercise with acs_delta is rejected", consuming: false, acsDelta: true, contractID: "emitter-active",
			tmpl: pinnedTmpl, readScope: governanceReadScope},
		{name: "transient real emitter is accepted", consuming: true, contractID: "emitter-transient",
			createdEvents: map[string]*apiv2.CreatedEvent{"emitter-transient": realTransientEmitter},
			tmpl:          pinnedTmpl, readScope: governanceReadScope, wantAccepted: true},
		{name: "non-consuming exercise on a transient real emitter is rejected", consuming: false, contractID: "emitter-transient",
			createdEvents: map[string]*apiv2.CreatedEvent{"emitter-transient": realTransientEmitter},
			tmpl:          pinnedTmpl, readScope: governanceReadScope},
		{name: "transient real emitter without a read-as party is rejected", consuming: true, contractID: "emitter-transient",
			createdEvents: map[string]*apiv2.CreatedEvent{"emitter-transient": realTransientEmitter},
			tmpl:          pinnedTmpl, readScope: anyPartyReadScope},
		{name: "transient real emitter without a package pin is rejected", consuming: true, contractID: "emitter-transient",
			createdEvents: map[string]*apiv2.CreatedEvent{"emitter-transient": realTransientEmitter},
			tmpl:          unpinnedTmpl, readScope: governanceReadScope},
		{name: "witness-only exercise without a create is rejected", consuming: true, contractID: "emitter-witnessed",
			createdEvents: map[string]*apiv2.CreatedEvent{"emitter-transient": realTransientEmitter},
			tmpl:          pinnedTmpl, readScope: governanceReadScope},
		{name: "ambiguous contract id is rejected", consuming: true, contractID: "emitter-transient",
			createdEvents: map[string]*apiv2.CreatedEvent{"emitter-transient": nil},
			tmpl:          pinnedTmpl, readScope: governanceReadScope},
		{name: "transient rogue emitter is rejected", consuming: true, contractID: "emitter-rogue",
			createdEvents: map[string]*apiv2.CreatedEvent{"emitter-rogue": emitterCreatedEvent("emitter-rogue", pinnedPackageID, "Emitter", rogueEmitterSignatories, nil).GetCreated()},
			tmpl:          pinnedTmpl, readScope: governanceReadScope},
		{name: "transient rogue emitter observed by the read-as party is rejected", consuming: true, contractID: "emitter-rogue",
			createdEvents: map[string]*apiv2.CreatedEvent{"emitter-rogue": emitterCreatedEvent("emitter-rogue", pinnedPackageID, "Emitter", rogueEmitterSignatories, []string{governanceParty}).GetCreated()},
			tmpl:          pinnedTmpl, readScope: governanceReadScope},
		{name: "transient emitter of another package is rejected", consuming: true, contractID: "emitter-lookalike",
			createdEvents: map[string]*apiv2.CreatedEvent{"emitter-lookalike": emitterCreatedEvent("emitter-lookalike", lookalikePackageID, "Emitter", realEmitterSignatories, nil).GetCreated()},
			tmpl:          pinnedTmpl, readScope: governanceReadScope},
		{name: "transient contract of another template is rejected", consuming: true, contractID: "registry-0",
			createdEvents: map[string]*apiv2.CreatedEvent{"registry-0": emitterCreatedEvent("registry-0", pinnedPackageID, "EmitterRegistry", realEmitterSignatories, nil).GetCreated()},
			tmpl:          pinnedTmpl, readScope: governanceReadScope},
		{name: "transient emitter with no template id is rejected", consuming: true, contractID: "emitter-untyped",
			createdEvents: map[string]*apiv2.CreatedEvent{"emitter-untyped": {ContractId: "emitter-untyped", Signatories: realEmitterSignatories}},
			tmpl:          pinnedTmpl, readScope: governanceReadScope},
	}
	for _, tc := range cases {
		t.Run(tc.name, func(t *testing.T) {
			exercised := &apiv2.ExercisedEvent{ContractId: tc.contractID, TemplateId: emitterTemplateIDUnder(pinnedPackageID), Consuming: tc.consuming, AcsDelta: tc.acsDelta}
			assert.Equal(t, tc.wantAccepted, readPartySignsExercisedEmitter(exercised, tc.createdEvents, tc.tmpl, sampleRecordTime, tc.readScope))
		})
	}
}

func emitterTemplateIDUnder(packageID string) *apiv2.Identifier {
	return &apiv2.Identifier{PackageId: packageID, ModuleName: "Wormhole.Core.State", EntityName: "Emitter"}
}

// A transient Emitter is accepted only when its CreatedEvent names the same
// package id as the exercise, and that id is accepted at the record time.
func TestReadPartySignsExercisedEmitterInUpgradeWindow(t *testing.T) {
	beforeDeadline := testRetiringDeadline.Add(-time.Second)
	afterDeadline := testRetiringDeadline.Add(time.Second)
	cases := []struct {
		name               string
		createdPackageID   string
		exercisedPackageID string
		recordTime         time.Time
		wantAccepted       bool
	}{
		{name: "current create, current exercise", createdPackageID: pinnedPackageID, exercisedPackageID: pinnedPackageID,
			recordTime: beforeDeadline, wantAccepted: true},
		{name: "retiring create, retiring exercise, before the deadline", createdPackageID: retiringPinnedPackageID, exercisedPackageID: retiringPinnedPackageID,
			recordTime: beforeDeadline, wantAccepted: true},
		{name: "current create, current exercise, after the deadline", createdPackageID: pinnedPackageID, exercisedPackageID: pinnedPackageID,
			recordTime: afterDeadline, wantAccepted: true},
		{name: "current create, retiring exercise, before the deadline", createdPackageID: pinnedPackageID, exercisedPackageID: retiringPinnedPackageID,
			recordTime: beforeDeadline},
		{name: "retiring create, current exercise, before the deadline", createdPackageID: retiringPinnedPackageID, exercisedPackageID: pinnedPackageID,
			recordTime: beforeDeadline},
		{name: "retiring create, current exercise, after the deadline", createdPackageID: retiringPinnedPackageID, exercisedPackageID: pinnedPackageID,
			recordTime: afterDeadline},
		{name: "retiring create, retiring exercise, at the deadline", createdPackageID: retiringPinnedPackageID, exercisedPackageID: retiringPinnedPackageID,
			recordTime: testRetiringDeadline},
		{name: "retiring create, retiring exercise, without record time", createdPackageID: retiringPinnedPackageID, exercisedPackageID: retiringPinnedPackageID},
		{name: "look-alike create, look-alike exercise", createdPackageID: lookalikePackageID, exercisedPackageID: lookalikePackageID,
			recordTime: beforeDeadline},
		{name: "look-alike create, current exercise", createdPackageID: lookalikePackageID, exercisedPackageID: pinnedPackageID,
			recordTime: beforeDeadline},
	}
	for _, tc := range cases {
		t.Run(tc.name, func(t *testing.T) {
			created := emitterCreatedEvent("emitter-transient", tc.createdPackageID, "Emitter", realEmitterSignatories, nil).GetCreated()
			exercised := &apiv2.ExercisedEvent{ContractId: "emitter-transient", TemplateId: emitterTemplateIDUnder(tc.exercisedPackageID), Consuming: true}
			createdEvents := map[string]*apiv2.CreatedEvent{"emitter-transient": created}
			assert.Equal(t, tc.wantAccepted, readPartySignsExercisedEmitter(exercised, createdEvents, upgradeWindowTmpl, tc.recordTime, governanceReadScope))
		})
	}
}

func TestCreatedEventsByContractIDMarksDuplicateContractIDs(t *testing.T) {
	first := emitterCreatedEvent("emitter-0", pinnedPackageID, "Emitter", realEmitterSignatories, nil)
	second := emitterCreatedEvent("emitter-0", pinnedPackageID, "Emitter", rogueEmitterSignatories, nil)
	unique := emitterCreatedEvent("emitter-1", pinnedPackageID, "Emitter", realEmitterSignatories, nil)
	tx := &apiv2.Transaction{Events: []*apiv2.Event{first, publishOnContractEvent("emitter-0", true, false, 0), second, unique}}

	createdEvents := createdEventsByContractID(tx)

	require.Len(t, createdEvents, 2)
	assert.Nil(t, createdEvents["emitter-0"])
	assert.Same(t, unique.GetCreated(), createdEvents["emitter-1"])
}

func TestUpdateFormatRequestsLedgerEffectsShape(t *testing.T) {
	for _, readScope := range []ReadScope{governanceReadScope, anyPartyReadScope} {
		client := &grpcClient{readScope: readScope}
		assert.Equal(t, apiv2.TransactionShape_TRANSACTION_SHAPE_LEDGER_EFFECTS,
			client.updateFormat().GetIncludeTransactions().GetTransactionShape(),
			"transient Emitter creates are only in the ledger effects shape")
	}
}
