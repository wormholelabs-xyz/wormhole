package cantonclient

import (
	"testing"

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
		readAsParty   string
		wantAccepted  bool
	}{
		{name: "consuming exercise with acs_delta is accepted", consuming: true, acsDelta: true, contractID: "emitter-active",
			tmpl: pinnedTmpl, readAsParty: governanceParty, wantAccepted: true},
		{name: "non-consuming exercise with acs_delta is rejected", consuming: false, acsDelta: true, contractID: "emitter-active",
			tmpl: pinnedTmpl, readAsParty: governanceParty},
		{name: "transient real emitter is accepted", consuming: true, contractID: "emitter-transient",
			createdEvents: map[string]*apiv2.CreatedEvent{"emitter-transient": realTransientEmitter},
			tmpl:          pinnedTmpl, readAsParty: governanceParty, wantAccepted: true},
		{name: "non-consuming exercise on a transient real emitter is rejected", consuming: false, contractID: "emitter-transient",
			createdEvents: map[string]*apiv2.CreatedEvent{"emitter-transient": realTransientEmitter},
			tmpl:          pinnedTmpl, readAsParty: governanceParty},
		{name: "transient real emitter without a read-as party is rejected", consuming: true, contractID: "emitter-transient",
			createdEvents: map[string]*apiv2.CreatedEvent{"emitter-transient": realTransientEmitter},
			tmpl:          pinnedTmpl, readAsParty: ""},
		{name: "transient real emitter without a package pin is rejected", consuming: true, contractID: "emitter-transient",
			createdEvents: map[string]*apiv2.CreatedEvent{"emitter-transient": realTransientEmitter},
			tmpl:          unpinnedTmpl, readAsParty: governanceParty},
		{name: "witness-only exercise without a create is rejected", consuming: true, contractID: "emitter-witnessed",
			createdEvents: map[string]*apiv2.CreatedEvent{"emitter-transient": realTransientEmitter},
			tmpl:          pinnedTmpl, readAsParty: governanceParty},
		{name: "ambiguous contract id is rejected", consuming: true, contractID: "emitter-transient",
			createdEvents: map[string]*apiv2.CreatedEvent{"emitter-transient": nil},
			tmpl:          pinnedTmpl, readAsParty: governanceParty},
		{name: "transient rogue emitter is rejected", consuming: true, contractID: "emitter-rogue",
			createdEvents: map[string]*apiv2.CreatedEvent{"emitter-rogue": emitterCreatedEvent("emitter-rogue", pinnedPackageID, "Emitter", rogueEmitterSignatories, nil).GetCreated()},
			tmpl:          pinnedTmpl, readAsParty: governanceParty},
		{name: "transient rogue emitter observed by the read-as party is rejected", consuming: true, contractID: "emitter-rogue",
			createdEvents: map[string]*apiv2.CreatedEvent{"emitter-rogue": emitterCreatedEvent("emitter-rogue", pinnedPackageID, "Emitter", rogueEmitterSignatories, []string{governanceParty}).GetCreated()},
			tmpl:          pinnedTmpl, readAsParty: governanceParty},
		{name: "transient emitter of another package is rejected", consuming: true, contractID: "emitter-lookalike",
			createdEvents: map[string]*apiv2.CreatedEvent{"emitter-lookalike": emitterCreatedEvent("emitter-lookalike", lookalikePackageID, "Emitter", realEmitterSignatories, nil).GetCreated()},
			tmpl:          pinnedTmpl, readAsParty: governanceParty},
		{name: "transient contract of another template is rejected", consuming: true, contractID: "registry-0",
			createdEvents: map[string]*apiv2.CreatedEvent{"registry-0": emitterCreatedEvent("registry-0", pinnedPackageID, "EmitterRegistry", realEmitterSignatories, nil).GetCreated()},
			tmpl:          pinnedTmpl, readAsParty: governanceParty},
		{name: "transient emitter with no template id is rejected", consuming: true, contractID: "emitter-untyped",
			createdEvents: map[string]*apiv2.CreatedEvent{"emitter-untyped": {ContractId: "emitter-untyped", Signatories: realEmitterSignatories}},
			tmpl:          pinnedTmpl, readAsParty: governanceParty},
	}
	for _, tc := range cases {
		t.Run(tc.name, func(t *testing.T) {
			exercised := &apiv2.ExercisedEvent{ContractId: tc.contractID, Consuming: tc.consuming, AcsDelta: tc.acsDelta}
			assert.Equal(t, tc.wantAccepted, readPartySignsExercisedEmitter(exercised, tc.createdEvents, tc.tmpl, tc.readAsParty))
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
	for _, readAsParty := range []string{governanceParty, ""} {
		client := &grpcClient{readAsParty: readAsParty}
		assert.Equal(t, apiv2.TransactionShape_TRANSACTION_SHAPE_LEDGER_EFFECTS,
			client.updateFormat().GetIncludeTransactions().GetTransactionShape(),
			"transient Emitter creates are only in the ledger effects shape")
	}
}
