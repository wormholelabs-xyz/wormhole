package cantonclient

import (
	"testing"

	"github.com/stretchr/testify/assert"
	"github.com/stretchr/testify/require"

	apiv2 "github.com/certusone/wormhole/node/pkg/cantonclient/proto/gen/com/daml/ledger/api/v2"
)

// emitterRequestEvent builds a CreatedEvent for a Wormhole.Core.State:EmitterRequest
// contract from field overrides, starting from a valid baseline. A nil override
// value deletes the field (to exercise the malformed-argument path).
func emitterRequestEvent(cid string, id *apiv2.Identifier, overrides map[string]*apiv2.Value) *apiv2.CreatedEvent {
	fields := map[string]*apiv2.Value{
		"requester": partyVal("Alice::1220ef01"),
		"operator":  partyVal("Operator::1220abcd"),
	}
	for k, v := range overrides {
		if v == nil {
			delete(fields, k)
		} else {
			fields[k] = v
		}
	}
	rec := &apiv2.Record{}
	for label, v := range fields {
		rec.Fields = append(rec.Fields, &apiv2.RecordField{Label: label, Value: v})
	}
	return &apiv2.CreatedEvent{
		ContractId:      cid,
		TemplateId:      id,
		CreateArguments: rec,
	}
}

func emitterRequestID() *apiv2.Identifier {
	return &apiv2.Identifier{
		PackageId:  "cafebabe",
		ModuleName: emitterRequestModule,
		EntityName: emitterRequestEntity,
	}
}

func TestDecodeEmitterRequestCreatedEvent(t *testing.T) {
	// Happy path: a well-formed EmitterRequest decodes to the right domain type,
	// carrying the template id echoed verbatim from the event.
	req, err := decodeEmitterRequest(emitterRequestEvent("req#0", emitterRequestID(), nil))
	require.NoError(t, err)
	assert.Equal(t, "req#0", req.ContractID)
	assert.Equal(t, "Alice::1220ef01", req.Requester)
	assert.Equal(t, "Operator::1220abcd", req.Operator)
	assert.Equal(t, TemplateID{PackageID: "cafebabe", ModuleName: emitterRequestModule, EntityName: emitterRequestEntity}, req.TemplateID)

	// templateMatches gates which ACS entries are decoded at all. An event for a
	// different template must not match the EmitterRequest filter (the caller
	// skips it), regardless of package id.
	tmpl := TemplateID{ModuleName: emitterRequestModule, EntityName: emitterRequestEntity}
	otherID := &apiv2.Identifier{PackageId: "cafebabe", ModuleName: emitterRequestModule, EntityName: "Emitter"}
	assert.False(t, templateMatches(otherID, tmpl), "Emitter must not match the EmitterRequest filter")
	assert.True(t, templateMatches(emitterRequestID(), tmpl), "EmitterRequest of any package id must match")

	// Malformed arguments must error, not panic.
	t.Run("missing requester", func(t *testing.T) {
		_, err := decodeEmitterRequest(emitterRequestEvent("req#1", emitterRequestID(), map[string]*apiv2.Value{"requester": nil}))
		assert.Error(t, err)
	})
	t.Run("requester not a party", func(t *testing.T) {
		_, err := decodeEmitterRequest(emitterRequestEvent("req#2", emitterRequestID(), map[string]*apiv2.Value{"requester": textVal("not-a-party")}))
		assert.Error(t, err)
	})
	t.Run("nil create arguments", func(t *testing.T) {
		_, err := decodeEmitterRequest(&apiv2.CreatedEvent{ContractId: "req#3", TemplateId: emitterRequestID()})
		assert.Error(t, err)
	})
}
