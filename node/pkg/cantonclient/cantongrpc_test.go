package cantonclient

import (
	"testing"

	"github.com/stretchr/testify/assert"
	"github.com/stretchr/testify/require"

	apiv2 "github.com/certusone/wormhole/node/pkg/cantonclient/proto/gen/com/daml/ledger/api/v2"
)

// value helpers for building a WormholeMessage record.
func partyVal(p string) *apiv2.Value { return &apiv2.Value{Sum: &apiv2.Value_Party{Party: p}} }
func int64Val(i int64) *apiv2.Value  { return &apiv2.Value{Sum: &apiv2.Value_Int64{Int64: i}} }
func textVal(s string) *apiv2.Value  { return &apiv2.Value{Sum: &apiv2.Value_Text{Text: s}} }

// wormholeRecord builds a WormholeMessage record Value from field overrides,
// starting from a valid baseline.
func wormholeRecord(overrides map[string]*apiv2.Value) *apiv2.Value {
	fields := map[string]*apiv2.Value{
		"registrar":        partyVal("Operator::1220abcd"),
		"owner":            partyVal("Alice::1220ef01"),
		"emitterId":        int64Val(3),
		"sequence":         int64Val(5),
		"nonce":            int64Val(42),
		"consistencyLevel": int64Val(0),
		"payload":          textVal("deadbeef"),
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
	return &apiv2.Value{Sum: &apiv2.Value_Record{Record: rec}}
}

func TestDecodeWormholeMessageHappyPath(t *testing.T) {
	msg, err := decodeWormholeMessage(wrappedResult("00deadbeef", wormholeRecord(nil)))
	require.NoError(t, err)
	assert.Equal(t, "Operator::1220abcd", msg.Registrar)
	assert.Equal(t, "Alice::1220ef01", msg.Owner)
	assert.Equal(t, uint64(3), msg.EmitterID)
	assert.Equal(t, uint64(5), msg.Sequence)
	assert.Equal(t, uint32(42), msg.Nonce)
	assert.Equal(t, uint8(0), msg.ConsistencyLevel)
	assert.Equal(t, []byte{0xde, 0xad, 0xbe, 0xef}, msg.Payload)
}

func TestDecodeWormholeMessageErrors(t *testing.T) {
	cases := map[string]map[string]*apiv2.Value{
		"missing registrar":   {"registrar": nil},
		"missing owner":       {"owner": nil},
		"registrar not party": {"registrar": textVal("Operator::1220abcd")},
		"owner not party":     {"owner": int64Val(1)},
		"negative emitterId":  {"emitterId": int64Val(-1)},
		"negative sequence":   {"sequence": int64Val(-1)},
		"nonce too large":     {"nonce": int64Val(1 << 33)},
	}
	for name, overrides := range cases {
		t.Run(name, func(t *testing.T) {
			_, err := decodeWormholeMessage(wrappedResult("00deadbeef", wormholeRecord(overrides)))
			assert.Error(t, err)
		})
	}
}

func TestDecodeWormholeMessageNotARecord(t *testing.T) {
	_, err := decodeWormholeMessage(partyVal("Operator::1220abcd"))
	assert.Error(t, err)
}

// contractIdVal builds a Value holding a Daml ContractId, as returned for the
// `emitterCid` field of the new PublishResult wrapper.
func contractIdVal(cid string) *apiv2.Value {
	return &apiv2.Value{Sum: &apiv2.Value_ContractId{ContractId: cid}}
}

// wrappedResult builds a PublishResult: an `emitterCid` field and a `message`
// field holding the WormholeMessage record.
func wrappedResult(emitterCid string, message *apiv2.Value) *apiv2.Value {
	rec := &apiv2.Record{
		Fields: []*apiv2.RecordField{
			{Label: "emitterCid", Value: contractIdVal(emitterCid)},
			{Label: "message", Value: message},
		},
	}
	return &apiv2.Value{Sum: &apiv2.Value_Record{Record: rec}}
}

// A result without the `message` record — e.g. a bare WormholeMessage from a
// pre-PublishResult core — must error rather than panic or decode to zero
// values.
func TestDecodeWormholeMessageRequiresMessageRecord(t *testing.T) {
	_, err := decodeWormholeMessage(wormholeRecord(nil))
	assert.Error(t, err)
}

func TestDecodeWormholeMessageNeitherShape(t *testing.T) {
	rec := &apiv2.Record{
		Fields: []*apiv2.RecordField{
			{Label: "somethingElse", Value: textVal("nope")},
		},
	}
	_, err := decodeWormholeMessage(&apiv2.Value{Sum: &apiv2.Value_Record{Record: rec}})
	assert.Error(t, err)
}
