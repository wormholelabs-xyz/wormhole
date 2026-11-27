package cantonclient

import (
	"context"
	"math"
	"net"
	"testing"
	"time"

	"github.com/stretchr/testify/assert"
	"github.com/stretchr/testify/require"
	"go.uber.org/zap"
	"google.golang.org/grpc"

	apiv2 "github.com/certusone/wormhole/node/pkg/cantonclient/proto/gen/com/daml/ledger/api/v2"
)

// value helpers for building a WormholeMessage record.
func partyVal(p string) *apiv2.Value { return &apiv2.Value{Sum: &apiv2.Value_Party{Party: p}} }
func int64Val(i int64) *apiv2.Value  { return &apiv2.Value{Sum: &apiv2.Value_Int64{Int64: i}} }
func textVal(s string) *apiv2.Value  { return &apiv2.Value{Sum: &apiv2.Value_Text{Text: s}} }

// variantVal builds a single-constructor Daml value as the verbose Ledger API returns it.
func variantVal(variantID *apiv2.Identifier, constructor string, value *apiv2.Value) *apiv2.Value {
	return &apiv2.Value{Sum: &apiv2.Value_Variant{Variant: &apiv2.Variant{VariantId: variantID, Constructor: constructor, Value: value}}}
}

func messagePayloadVariantID(packageID string) *apiv2.Identifier {
	return &apiv2.Identifier{PackageId: packageID, ModuleName: messagePayloadModule, EntityName: messagePayloadEntity}
}

// messagePayloadVal builds a Wormhole.Core.Bytes.MessagePayload value of the pinned package.
func messagePayloadVal(payloadHex string) *apiv2.Value {
	return variantVal(messagePayloadVariantID(pinnedPackageID), messagePayloadConstructor, textVal(payloadHex))
}

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
		"payload":          messagePayloadVal("deadbeef"),
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
	msg, err := decodeWormholeMessage(tupleResult("00deadbeef", wormholeRecord(nil)), pinnedPackageID)
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
			_, err := decodeWormholeMessage(tupleResult("00deadbeef", wormholeRecord(overrides)), pinnedPackageID)
			assert.Error(t, err)
		})
	}
}

// lookalikePackageIDHex is a package other than the executing one that declares a MessagePayload lookalike.
const lookalikePackageIDHex = "2222222222222222222222222222222222222222222222222222222222222222"

// The payload decoder checks the variant identity and applies the MessagePayload rule of Wormhole.Core.Bytes.
func TestDecodeWormholeMessagePayloadShape(t *testing.T) {
	cases := []struct {
		name        string
		payload     *apiv2.Value
		wantPayload []byte
		wantErr     string
	}{
		{name: "valid", payload: messagePayloadVal("deadbeef"), wantPayload: []byte{0xde, 0xad, 0xbe, 0xef}},
		{name: "empty", payload: messagePayloadVal(""), wantPayload: []byte{}},
		{name: "plain text of the 0.9.0 shape", payload: textVal("deadbeef"), wantErr: "is not a variant"},
		{name: "wrong constructor", payload: variantVal(messagePayloadVariantID(pinnedPackageID), "HexBytes", textVal("deadbeef")), wantErr: "has constructor"},
		{name: "variant holding an int", payload: variantVal(messagePayloadVariantID(pinnedPackageID), messagePayloadConstructor, int64Val(1)), wantErr: "does not hold text"},
		{name: "no variant id", payload: variantVal(nil, messagePayloadConstructor, textVal("deadbeef")), wantErr: "has no variant id"},
		{name: "lookalike package variant id", payload: variantVal(messagePayloadVariantID(lookalikePackageIDHex), messagePayloadConstructor, textVal("deadbeef")), wantErr: "expected the executing package"},
		{name: "lookalike module", payload: variantVal(&apiv2.Identifier{PackageId: pinnedPackageID, ModuleName: "Lookalike.Bytes", EntityName: messagePayloadEntity}, messagePayloadConstructor, textVal("deadbeef")), wantErr: "expected Wormhole.Core.Bytes:MessagePayload"},
		{name: "other entity", payload: variantVal(&apiv2.Identifier{PackageId: pinnedPackageID, ModuleName: messagePayloadModule, EntityName: "HexBytes"}, messagePayloadConstructor, textVal("deadbeef")), wantErr: "expected Wormhole.Core.Bytes:MessagePayload"},
		{name: "uppercase", payload: messagePayloadVal("DEADBEEF"), wantErr: "not lowercase hex"},
		{name: "0x prefix", payload: messagePayloadVal("0xdeadbeef"), wantErr: "not lowercase hex"},
		{name: "odd length", payload: messagePayloadVal("deadbee"), wantErr: "odd hex length"},
		{name: "non-hex", payload: messagePayloadVal("zz"), wantErr: "not lowercase hex"},
		{name: "missing", payload: nil, wantErr: "missing field"},
	}
	for _, tc := range cases {
		t.Run(tc.name, func(t *testing.T) {
			msg, err := decodeWormholeMessage(tupleResult("00deadbeef", wormholeRecord(map[string]*apiv2.Value{"payload": tc.payload})), pinnedPackageID)
			if tc.wantErr != "" {
				require.ErrorContains(t, err, tc.wantErr)
				return
			}
			require.NoError(t, err)
			assert.Equal(t, tc.wantPayload, msg.Payload)
		})
	}
}

func TestDecodeWormholeMessageBounds(t *testing.T) {
	cases := []struct {
		name          string
		emitterID     int64
		wantEmitterID uint64
		wantErr       bool
	}{
		{name: "emitterId 0", emitterID: 0, wantEmitterID: 0},
		{name: "emitterId MaxEmitterID", emitterID: MaxEmitterID, wantEmitterID: MaxEmitterID},
		{name: "emitterId MaxEmitterID + 1", emitterID: MaxEmitterID + 1, wantErr: true},
		{name: "emitterId int64 max", emitterID: math.MaxInt64, wantErr: true},
	}
	for _, tc := range cases {
		t.Run(tc.name, func(t *testing.T) {
			msg, err := decodeWormholeMessage(tupleResult("00deadbeef",
				wormholeRecord(map[string]*apiv2.Value{"emitterId": int64Val(tc.emitterID)})), pinnedPackageID)
			if tc.wantErr {
				require.ErrorContains(t, err, "emitterId exceeds MaxEmitterID")
				return
			}
			require.NoError(t, err)
			assert.Equal(t, tc.wantEmitterID, msg.EmitterID)
		})
	}
}

func TestDecodeWormholeMessageNotARecord(t *testing.T) {
	_, err := decodeWormholeMessage(partyVal("Operator::1220abcd"), pinnedPackageID)
	assert.Error(t, err)
}

// contractIdVal builds a Value holding a Daml ContractId, as returned for the
// tuple's first element (the sequence-bumped Emitter successor cid).
func contractIdVal(cid string) *apiv2.Value {
	return &apiv2.Value{Sum: &apiv2.Value_ContractId{ContractId: cid}}
}

// tupleResult builds the (ContractId Emitter, WormholeMessage) tuple
// PublishMessage returns: a Tuple2 record with positional labels _1 (the
// emitter cid) and _2 (the WormholeMessage record).
func tupleResult(emitterCid string, message *apiv2.Value) *apiv2.Value {
	rec := &apiv2.Record{
		Fields: []*apiv2.RecordField{
			{Label: "_1", Value: contractIdVal(emitterCid)},
			{Label: "_2", Value: message},
		},
	}
	return &apiv2.Value{Sum: &apiv2.Value_Record{Record: rec}}
}

// A result without the `_2` element — e.g. a bare WormholeMessage from a
// pre-tuple core — must error rather than panic or decode to zero values.
func TestDecodeWormholeMessageRequiresTupleSecondElement(t *testing.T) {
	_, err := decodeWormholeMessage(wormholeRecord(nil), pinnedPackageID)
	assert.Error(t, err)
}

func TestDecodeWormholeMessageNeitherShape(t *testing.T) {
	rec := &apiv2.Record{
		Fields: []*apiv2.RecordField{
			{Label: "somethingElse", Value: textVal("nope")},
		},
	}
	_, err := decodeWormholeMessage(&apiv2.Value{Sum: &apiv2.Value_Record{Record: rec}}, pinnedPackageID)
	assert.Error(t, err)
}

// TestNonTransportOptionsDoNotDisableTLS is a regression guard: the client used
// to infer "caller supplied transport credentials" from opts being non-empty,
// so passing a non-transport option (per-RPC OAuth credentials) silently
// dropped TLS and the dial failed with "no transport security set". Transport
// is now an explicit parameter; a plaintext listener must be rejected by the
// TLS handshake rather than accepted.
func TestNonTransportOptionsDoNotDisableTLS(t *testing.T) {
	lis, err := net.Listen("tcp", "127.0.0.1:0")
	require.NoError(t, err)
	defer lis.Close()       //nolint:errcheck
	srv := grpc.NewServer() // plaintext
	go func() { _ = srv.Serve(lis) }()
	defer srv.Stop()

	// nil transport creds => TLS, plus a non-transport dial option alongside.
	c, err := NewCantonGrpcClient(lis.Addr().String(), anyPartyReadScope, zap.NewNop(), nil,
		grpc.WithUserAgent("regression-test"))
	require.NoError(t, err)
	defer c.Close() //nolint:errcheck

	ctx, cancel := context.WithTimeout(context.Background(), 10*time.Second)
	defer cancel()
	_, err = c.GetLedgerEnd(ctx)
	require.Error(t, err, "TLS client must not succeed against a plaintext server")
	assert.NotContains(t, err.Error(), "no transport security set")
}
