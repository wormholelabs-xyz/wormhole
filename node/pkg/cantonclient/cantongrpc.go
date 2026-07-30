// This file is the Canton Ledger API v2 gRPC client. It depends on the Go stubs
// generated from the vendored protos under ./proto (regenerate with
// `make generate-canton-proto`; see README.md). The stubs are committed and
// CI-verified, so this builds by default — no build tag.
package cantonclient

import (
	"context"
	"encoding/hex"
	"fmt"
	"io"
	"math"
	"strings"
	"time"

	"go.uber.org/zap"
	"google.golang.org/grpc"
	"google.golang.org/grpc/credentials"
	"google.golang.org/protobuf/types/known/timestamppb"

	apiv2 "github.com/certusone/wormhole/node/pkg/cantonclient/proto/gen/com/daml/ledger/api/v2"
)

type grpcClient struct {
	conn        *grpc.ClientConn
	state       apiv2.StateServiceClient
	update      apiv2.UpdateServiceClient
	readAsParty string
	logger      *zap.Logger
}

// NewCantonGrpcClient dials the Ledger API at rpc. readAsParty narrows the
// update stream to a single party; leave it empty to observe every party hosted
// on the participant (a wildcard "any party" filter), which is the default for
// the core-bridge watcher — it should see PublishMessage from every emitter, and
// it avoids depending on the operator party id (which carries a namespace
// fingerprint not known until allocation). If no transport-credentials dial
// option is supplied, TLS is used.
func NewCantonGrpcClient(rpc string, readAsParty string, logger *zap.Logger, opts ...grpc.DialOption) (CantonClient, error) {
	if !hasTransportCreds(opts) {
		opts = append(opts, grpc.WithTransportCredentials(credentials.NewTLS(nil)))
	}
	conn, err := grpc.NewClient(rpc, opts...)
	if err != nil {
		return nil, fmt.Errorf("cantonclient: failed to dial %s: %w", rpc, err)
	}
	return &grpcClient{
		conn:        conn,
		state:       apiv2.NewStateServiceClient(conn),
		update:      apiv2.NewUpdateServiceClient(conn),
		readAsParty: readAsParty,
		logger:      logger,
	}, nil
}

// hasTransportCreds reports whether the caller already supplied dial options.
// grpc exposes no way to introspect option contents, so we rely on the
// convention that dev mode passes insecure.NewCredentials() explicitly; in that
// case we must not also add TLS.
func hasTransportCreds(opts []grpc.DialOption) bool {
	return len(opts) > 0
}

func (c *grpcClient) Close() error {
	return c.conn.Close()
}

func (c *grpcClient) GetLedgerEnd(ctx context.Context) (int64, error) {
	resp, err := c.state.GetLedgerEnd(ctx, &apiv2.GetLedgerEndRequest{})
	if err != nil {
		return 0, fmt.Errorf("GetLedgerEnd: %w", err)
	}
	return resp.GetOffset(), nil
}

// updateFormat builds the v2 UpdateFormat that selects transactions in
// LEDGER_EFFECTS shape (so ExercisedEvents — which carry choice results — are
// included). A wildcard template filter matches every template; the watcher
// narrows to the core-bridge template/choice in Go. When readAsParty is empty
// the events of every party hosted on the participant are streamed
// (filters_for_any_party); otherwise the stream is narrowed to that one party.
func (c *grpcClient) updateFormat() *apiv2.UpdateFormat {
	wildcard := &apiv2.Filters{
		Cumulative: []*apiv2.CumulativeFilter{{
			IdentifierFilter: &apiv2.CumulativeFilter_WildcardFilter{
				WildcardFilter: &apiv2.WildcardFilter{},
			},
		}},
	}
	eventFormat := &apiv2.EventFormat{Verbose: true}
	if c.readAsParty == "" {
		eventFormat.FiltersForAnyParty = wildcard
	} else {
		eventFormat.FiltersByParty = map[string]*apiv2.Filters{c.readAsParty: wildcard}
	}
	return &apiv2.UpdateFormat{
		IncludeTransactions: &apiv2.TransactionFormat{
			TransactionShape: apiv2.TransactionShape_TRANSACTION_SHAPE_LEDGER_EFFECTS,
			EventFormat:      eventFormat,
		},
	}
}

func (c *grpcClient) SubscribeUpdates(ctx context.Context, beginExclusive int64, tmpl TemplateID, choiceName string, out chan<- CantonMessageEvent) (*Subscription, error) {
	streamCtx, cancel := context.WithCancel(ctx)
	stream, err := c.update.GetUpdates(streamCtx, &apiv2.GetUpdatesRequest{
		BeginExclusive: beginExclusive,
		UpdateFormat:   c.updateFormat(),
	})
	if err != nil {
		cancel()
		return nil, fmt.Errorf("GetUpdates: %w", err)
	}

	sub := NewSubscription(cancel)
	go func() {
		defer sub.Close()
		for {
			resp, err := stream.Recv()
			if err == io.EOF {
				return
			}
			if err != nil {
				if streamCtx.Err() != nil {
					return // cancelled via Unsubscribe / parent ctx
				}
				sub.Fail(fmt.Errorf("GetUpdates stream: %w", err))
				return
			}
			tx := resp.GetTransaction()
			if tx == nil {
				continue // reassignment / checkpoint / topology — ignore
			}
			for _, msg := range messagesFromTx(tx, tmpl, choiceName, c.logger) {
				select {
				case <-streamCtx.Done():
					return
				case out <- CantonMessageEvent{
					Offset:      tx.GetOffset(),
					UpdateID:    tx.GetUpdateId(),
					EffectiveAt: protoTime(tx.GetEffectiveAt()),
					Message:     msg,
				}:
				}
			}
		}
	}()
	return sub, nil
}

func (c *grpcClient) GetUpdateByOffset(ctx context.Context, offset int64, tmpl TemplateID, choiceName string) (CantonTransaction, error) {
	resp, err := c.update.GetUpdateByOffset(ctx, &apiv2.GetUpdateByOffsetRequest{
		Offset:       offset,
		UpdateFormat: c.updateFormat(),
	})
	if err != nil {
		return CantonTransaction{}, fmt.Errorf("GetUpdateByOffset(%d): %w", offset, err)
	}
	tx := resp.GetTransaction()
	if tx == nil {
		return CantonTransaction{}, fmt.Errorf("GetUpdateByOffset(%d): not a transaction", offset)
	}
	return CantonTransaction{
		Offset:      tx.GetOffset(),
		UpdateID:    tx.GetUpdateId(),
		EffectiveAt: protoTime(tx.GetEffectiveAt()),
		Messages:    messagesFromTx(tx, tmpl, choiceName, c.logger),
	}, nil
}

// messagesFromTx extracts every Wormhole message from a transaction: each
// ExercisedEvent of `choiceName` on `tmpl` whose exercise_result decodes to a
// WormholeMessage record.
func messagesFromTx(tx *apiv2.Transaction, tmpl TemplateID, choiceName string, logger *zap.Logger) []CantonMessage {
	var out []CantonMessage
	for _, ev := range tx.GetEvents() {
		ex := ev.GetExercised()
		if ex == nil {
			continue
		}
		if ex.GetChoice() != choiceName || !templateMatches(ex.GetTemplateId(), tmpl) {
			continue
		}
		msg, err := decodeWormholeMessage(ex.GetExerciseResult())
		if err != nil {
			if logger != nil {
				logger.Error("canton: failed to decode WormholeMessage",
					zap.String("updateId", tx.GetUpdateId()), zap.Error(err))
			}
			continue
		}
		out = append(out, msg)
	}
	return out
}

// templateMatches compares an event's template id against the configured filter.
// PackageID may be empty in the filter to match any package version (Daml
// package upgrades change the package id while preserving module/entity).
func templateMatches(id *apiv2.Identifier, tmpl TemplateID) bool {
	if id == nil {
		return false
	}
	if tmpl.PackageID != "" && id.GetPackageId() != tmpl.PackageID {
		return false
	}
	return id.GetModuleName() == tmpl.ModuleName && id.GetEntityName() == tmpl.EntityName
}

// tupleMessageField is the second element of the (ContractId Emitter,
// WormholeMessage) tuple PublishMessage returns. Daml tuples serialize as
// DA.Types.Tuple2 records with positional labels _1/_2, not field names.
const tupleMessageField = "_2"

// decodeWormholeMessage maps PublishMessage's exercise result — the
// (ContractId Emitter, WormholeMessage) tuple — to a CantonMessage. The
// message is the tuple's second element (tupleMessageField); the first
// element, the sequence-bumped Emitter successor cid, is for on-ledger
// callers and is not needed here.
//
// See Wormhole.Core.State.WormholeMessage for the field set: registrar and
// owner are Daml Parties -> Value.party (the emitter's key components, from
// which the watcher derives the emitter address); emitterId, sequence, nonce,
// consistencyLevel are Daml Int -> Value.int64; payload is Daml Text (hex) ->
// Value.text.
func decodeWormholeMessage(v *apiv2.Value) (CantonMessage, error) {
	rec := v.GetRecord()
	if rec == nil {
		return CantonMessage{}, fmt.Errorf("exercise result is not a record")
	}
	outer := map[string]*apiv2.Value{}
	for _, f := range rec.GetFields() {
		outer[f.GetLabel()] = f.GetValue()
	}
	inner := outer[tupleMessageField].GetRecord()
	if inner == nil {
		return CantonMessage{}, fmt.Errorf("exercise result has no %s (WormholeMessage) record", tupleMessageField)
	}
	fields := map[string]*apiv2.Value{}
	for _, f := range inner.GetFields() {
		fields[f.GetLabel()] = f.GetValue()
	}
	registrar, err := partyField(fields, "registrar")
	if err != nil {
		return CantonMessage{}, err
	}
	owner, err := partyField(fields, "owner")
	if err != nil {
		return CantonMessage{}, err
	}
	emitterID, err := intField(fields, "emitterId")
	if err != nil {
		return CantonMessage{}, err
	}
	payload, err := hexField(fields, "payload")
	if err != nil {
		return CantonMessage{}, err
	}
	seq, err := intField(fields, "sequence")
	if err != nil {
		return CantonMessage{}, err
	}
	nonce, err := intField(fields, "nonce")
	if err != nil {
		return CantonMessage{}, err
	}
	cl, err := intField(fields, "consistencyLevel")
	if err != nil {
		return CantonMessage{}, err
	}
	if emitterID < 0 || seq < 0 || nonce < 0 || nonce > math.MaxUint32 || cl < 0 || cl > math.MaxUint8 {
		return CantonMessage{}, fmt.Errorf("field out of range (emitterId=%d seq=%d nonce=%d cl=%d)", emitterID, seq, nonce, cl)
	}
	return CantonMessage{
		Registrar:        registrar,
		Owner:            owner,
		EmitterID:        uint64(emitterID),
		Sequence:         uint64(seq),
		Nonce:            uint32(nonce),
		ConsistencyLevel: uint8(cl),
		Payload:          payload,
	}, nil
}

// partyField reads a Daml Party field, returned by the Ledger API as a non-empty
// party-id string (Value.party).
func partyField(fields map[string]*apiv2.Value, label string) (string, error) {
	v, ok := fields[label]
	if !ok {
		return "", fmt.Errorf("missing field %q", label)
	}
	p := v.GetParty()
	if p == "" {
		return "", fmt.Errorf("field %q is not a party", label)
	}
	return p, nil
}

func hexField(fields map[string]*apiv2.Value, label string) ([]byte, error) {
	v, ok := fields[label]
	if !ok {
		return nil, fmt.Errorf("missing field %q", label)
	}
	b, err := hex.DecodeString(strings.TrimPrefix(v.GetText(), "0x"))
	if err != nil {
		return nil, fmt.Errorf("field %q is not valid hex: %w", label, err)
	}
	return b, nil
}

func intField(fields map[string]*apiv2.Value, label string) (int64, error) {
	v, ok := fields[label]
	if !ok {
		return 0, fmt.Errorf("missing field %q", label)
	}
	return v.GetInt64(), nil
}

func protoTime(ts *timestamppb.Timestamp) time.Time {
	if ts == nil {
		return time.Time{}
	}
	return ts.AsTime()
}
