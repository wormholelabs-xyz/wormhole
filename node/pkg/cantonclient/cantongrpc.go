// This file is the Canton Ledger API v2 gRPC client. It depends on the Go stubs
// generated from the vendored protos under ./proto (regenerate with
// `make generate-canton-proto`; see README.md). The stubs are committed and
// CI-verified, so this builds by default — no build tag.
package cantonclient

import (
	"context"
	"encoding/hex"
	"errors"
	"fmt"
	"io"
	"math"
	"time"

	"go.uber.org/zap"
	"google.golang.org/grpc"
	"google.golang.org/grpc/credentials"
	"google.golang.org/protobuf/types/known/timestamppb"

	apiv2 "github.com/certusone/wormhole/node/pkg/cantonclient/proto/gen/com/daml/ledger/api/v2"
)

type grpcClient struct {
	conn      *grpc.ClientConn
	state     apiv2.StateServiceClient
	update    apiv2.UpdateServiceClient
	packages  apiv2.PackageServiceClient
	version   apiv2.VersionServiceClient
	readScope ReadScope
	logger    *zap.Logger
}

// maxLedgerUpdateBytes bounds one received Ledger API message. A Canton
// update above it stops the watcher at that offset.
const maxLedgerUpdateBytes = 64 << 20

// NewCantonGrpcClient dials the Ledger API at rpc. readScope selects the
// party filter of the update stream and GetUpdateById.
//
// transportCreds selects the transport: pass nil for TLS (the production
// default), or insecure credentials for a plaintext dev-mode participant. It is
// an explicit parameter rather than something inferred from opts because gRPC
// dial options cannot be introspected — opts carries non-transport options such
// as per-RPC OAuth credentials, and guessing from their presence would silently
// dial without transport security.
func NewCantonGrpcClient(rpc string, readScope ReadScope, logger *zap.Logger, transportCreds credentials.TransportCredentials, opts ...grpc.DialOption) (CantonClient, error) {
	if err := readScope.CheckWasConstructed(); err != nil {
		return nil, err
	}
	if transportCreds == nil {
		transportCreds = credentials.NewTLS(nil)
	}
	opts = append(opts,
		grpc.WithTransportCredentials(transportCreds),
		grpc.WithDefaultCallOptions(grpc.MaxCallRecvMsgSize(maxLedgerUpdateBytes)),
	)
	conn, err := grpc.NewClient(rpc, opts...)
	if err != nil {
		return nil, fmt.Errorf("cantonclient: failed to dial %s: %w", rpc, err)
	}
	return &grpcClient{
		conn:      conn,
		state:     apiv2.NewStateServiceClient(conn),
		update:    apiv2.NewUpdateServiceClient(conn),
		packages:  apiv2.NewPackageServiceClient(conn),
		version:   apiv2.NewVersionServiceClient(conn),
		readScope: readScope,
		logger:    logger,
	}, nil
}

func (c *grpcClient) Close() error {
	return c.conn.Close()
}

// unaryRPCTimeout bounds the non-streaming calls so a hung participant fails
// fast instead of blocking Run. The streaming GetUpdates is deliberately left
// unbounded (cancelable via context).
const unaryRPCTimeout = 30 * time.Second

func (c *grpcClient) GetLedgerEnd(ctx context.Context) (int64, error) {
	ctx, cancel := context.WithTimeout(ctx, unaryRPCTimeout)
	defer cancel()
	resp, err := c.state.GetLedgerEnd(ctx, &apiv2.GetLedgerEndRequest{})
	if err != nil {
		return 0, fmt.Errorf("GetLedgerEnd: %w", err)
	}
	return resp.GetOffset(), nil
}

// maxScannedVettedPackagesEntries bounds the (participant, synchronizer) entries that one
// ListVettedPackageMetadata call pages through. The server applies the package filter after it
// selects a page, so a page can be empty and still carry a next-page token. A participant filter
// needs GetParticipantId, which requires ParticipantAdmin.
const maxScannedVettedPackagesEntries = 1 << 16

// maxVettedPackagesPageSizeUpperBound rejects a server maximum page size that is not plausible.
const maxVettedPackagesPageSizeUpperBound = 1 << 20

// maxVettedPackagePagesForPageSize is the page cap of one call: every scanned entry, plus one final page.
func maxVettedPackagePagesForPageSize(pageSize int) int {
	if pageSize <= 0 {
		panic(fmt.Sprintf("maxVettedPackagePagesForPageSize: page size %d", pageSize))
	}
	return (maxScannedVettedPackagesEntries+pageSize-1)/pageSize + 1
}

// serverMaxVettedPackagesPageSize reads PackageFeature.max_vetted_packages_page_size from the VersionService.
func (c *grpcClient) serverMaxVettedPackagesPageSize(ctx context.Context) (int, error) {
	ctx, cancel := context.WithTimeout(ctx, unaryRPCTimeout)
	defer cancel()
	response, err := c.version.GetLedgerApiVersion(ctx, &apiv2.GetLedgerApiVersionRequest{})
	if err != nil {
		return 0, fmt.Errorf("GetLedgerApiVersion: %w", err)
	}
	pageSize := response.GetFeatures().GetPackageFeature().GetMaxVettedPackagesPageSize()
	if pageSize <= 0 {
		return 0, fmt.Errorf("GetLedgerApiVersion: ListVettedPackages maximum page size %d, expected a positive value", pageSize)
	}
	if pageSize > maxVettedPackagesPageSizeUpperBound {
		return 0, fmt.Errorf("GetLedgerApiVersion: ListVettedPackages maximum page size %d, expected at most %d", pageSize, maxVettedPackagesPageSizeUpperBound)
	}
	return int(pageSize), nil
}

func (c *grpcClient) ListVettedPackageMetadata(ctx context.Context, packageIDs []PackageID) ([]VettedPackageMetadata, error) {
	// An empty filter matches every vetted package.
	if len(packageIDs) == 0 {
		return nil, fmt.Errorf("ListVettedPackages: at least one package id is required")
	}
	packageIDHexes := make([]string, 0, len(packageIDs))
	for _, packageID := range packageIDs {
		if packageID.isZero() {
			return nil, fmt.Errorf("ListVettedPackages: empty package id")
		}
		packageIDHexes = append(packageIDHexes, packageID.hexID)
	}
	pageSize, err := c.serverMaxVettedPackagesPageSize(ctx)
	if err != nil {
		return nil, err
	}
	maxPages := maxVettedPackagePagesForPageSize(pageSize)
	var entries []VettedPackageMetadata
	pageToken := ""
	for pageIndex := 0; pageIndex < maxPages; pageIndex++ {
		response, err := c.listVettedPackagesPage(ctx, packageIDHexes, pageToken, pageSize)
		if err != nil {
			return nil, err
		}
		for _, vettedPackages := range response.GetVettedPackages() {
			for _, vettedPackage := range vettedPackages.GetPackages() {
				entries = append(entries, VettedPackageMetadata{
					PackageID:      vettedPackage.GetPackageId(),
					PackageName:    vettedPackage.GetPackageName(),
					PackageVersion: vettedPackage.GetPackageVersion(),
				})
			}
		}
		pageToken = response.GetNextPageToken()
		if pageToken == "" {
			return entries, nil
		}
	}
	return nil, fmt.Errorf("ListVettedPackages: more than %d pages of %d entries (%d packages read); the reply lists every participant and synchronizer that vets the requested ids", maxPages, pageSize, len(entries))
}

func (c *grpcClient) listVettedPackagesPage(ctx context.Context, packageIDHexes []string, pageToken string, pageSize int) (*apiv2.ListVettedPackagesResponse, error) {
	ctx, cancel := context.WithTimeout(ctx, unaryRPCTimeout)
	defer cancel()
	response, err := c.packages.ListVettedPackages(ctx, &apiv2.ListVettedPackagesRequest{
		PackageMetadataFilter: &apiv2.PackageMetadataFilter{PackageIds: packageIDHexes},
		PageToken:             pageToken,
		PageSize:              uint32(pageSize), //nolint:gosec // serverMaxVettedPackagesPageSize bounds it to (0, 1<<20]
	})
	if err != nil {
		return nil, fmt.Errorf("ListVettedPackages: %w", err)
	}
	return response, nil
}

// updateFormat builds the v2 UpdateFormat that selects transactions in
// LEDGER_EFFECTS shape (so ExercisedEvents — which carry choice results — are
// included).
// SECURITY: LEDGER_EFFECTS also carries the CreatedEvent of a contract that the
// same transaction archives; readPartySignsExercisedEmitter relies on it.
// A wildcard template filter matches every template; the watcher narrows to
// the core-bridge template/choice in Go. The any-party scope streams the events
// of every party hosted on the participant (filters_for_any_party).
// GetUpdates and GetUpdateById share it, so reobservation sees what the stream sees.
func (c *grpcClient) updateFormat() *apiv2.UpdateFormat {
	wildcard := &apiv2.Filters{
		Cumulative: []*apiv2.CumulativeFilter{{
			IdentifierFilter: &apiv2.CumulativeFilter_WildcardFilter{
				WildcardFilter: &apiv2.WildcardFilter{},
			},
		}},
	}
	eventFormat := &apiv2.EventFormat{Verbose: true}
	if party, isGuardianGovernance := c.readScope.GuardianGovernanceParty(); isGuardianGovernance {
		eventFormat.FiltersByParty = map[string]*apiv2.Filters{party.String(): wildcard}
	} else if c.readScope.IsAnyPartyUnsafeDevMode() {
		eventFormat.FiltersForAnyParty = wildcard
	} else {
		panic("cantonclient: update format with a read scope that was not constructed")
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
			if err != nil {
				if streamCtx.Err() != nil {
					return // cancelled via Unsubscribe / parent ctx
				}
				// Any non-cancellation error, including a clean io.EOF stream end,
				// is terminal: fail the subscription so the data pump returns and
				// the supervisor restarts it rather than silently observing nothing.
				sub.Fail(fmt.Errorf("GetUpdates stream ended: %w", err))
				return
			}
			tx := resp.GetTransaction()
			if tx == nil {
				continue // reassignment / checkpoint / topology — ignore
			}
			messages := messagesFromTx(tx, tmpl, choiceName, c.readScope, c.logger)
			if len(messages) == 0 {
				continue
			}
			updateID, err := ParseUpdateID(tx.GetUpdateId())
			if err != nil {
				sub.Fail(fmt.Errorf("GetUpdates: transaction at offset %d: %w", tx.GetOffset(), err))
				return
			}
			for _, msg := range messages {
				select {
				case <-streamCtx.Done():
					return
				case out <- CantonMessageEvent{
					ParticipantOffset: tx.GetOffset(),
					UpdateID:          updateID,
					EffectiveAt:       protoTime(tx.GetEffectiveAt()),
					RecordTime:        protoTime(tx.GetRecordTime()),
					Message:           msg,
				}:
				}
			}
		}
	}()
	return sub, nil
}

// maxPublishesPerRange bounds one ReadPublishesInRange call.
const maxPublishesPerRange = 100_000

func (c *grpcClient) ReadPublishesInRange(ctx context.Context, beginExclusive int64, endInclusive int64, tmpl TemplateID, choiceName string) ([]CantonMessageEvent, error) {
	if beginExclusive < 0 {
		return nil, fmt.Errorf("GetUpdates: negative begin offset %d", beginExclusive)
	}
	if beginExclusive > endInclusive {
		return nil, fmt.Errorf("GetUpdates: begin offset %d is after end offset %d", beginExclusive, endInclusive)
	}
	events := []CantonMessageEvent{}
	if beginExclusive == endInclusive {
		return events, nil
	}
	streamCtx, cancel := context.WithCancel(ctx)
	defer cancel()
	stream, err := c.update.GetUpdates(streamCtx, &apiv2.GetUpdatesRequest{
		BeginExclusive: beginExclusive,
		EndInclusive:   &endInclusive,
		UpdateFormat:   c.updateFormat(),
	})
	if err != nil {
		return nil, fmt.Errorf("GetUpdates: %w", err)
	}
	// Terminates: the participant ends the stream after endInclusive.
	for {
		response, err := stream.Recv()
		if errors.Is(err, io.EOF) {
			return events, nil
		}
		if err != nil {
			return nil, fmt.Errorf("GetUpdates(%d, %d]: %w", beginExclusive, endInclusive, err)
		}
		tx := response.GetTransaction()
		if tx == nil {
			continue
		}
		if tx.GetOffset() <= beginExclusive || tx.GetOffset() > endInclusive {
			return nil, fmt.Errorf("GetUpdates(%d, %d]: transaction at offset %d is beyond the requested range", beginExclusive, endInclusive, tx.GetOffset())
		}
		messages := messagesFromTx(tx, tmpl, choiceName, c.readScope, c.logger)
		if len(messages) == 0 {
			continue
		}
		updateID, err := ParseUpdateID(tx.GetUpdateId())
		if err != nil {
			return nil, fmt.Errorf("GetUpdates: transaction at offset %d: %w", tx.GetOffset(), err)
		}
		if len(events)+len(messages) > maxPublishesPerRange {
			return nil, fmt.Errorf("GetUpdates(%d, %d]: more than %d publishes", beginExclusive, endInclusive, maxPublishesPerRange)
		}
		for _, message := range messages {
			events = append(events, CantonMessageEvent{
				ParticipantOffset: tx.GetOffset(),
				UpdateID:          updateID,
				EffectiveAt:       protoTime(tx.GetEffectiveAt()),
				RecordTime:        protoTime(tx.GetRecordTime()),
				Message:           message,
			})
		}
	}
}

func (c *grpcClient) GetTransactionByUpdateID(ctx context.Context, updateID UpdateID, tmpl TemplateID, choiceName string) (CantonTransaction, error) {
	ctx, cancel := context.WithTimeout(ctx, unaryRPCTimeout)
	defer cancel()
	resp, err := c.update.GetUpdateById(ctx, &apiv2.GetUpdateByIdRequest{
		UpdateId:     updateID.String(),
		UpdateFormat: c.updateFormat(),
	})
	if err != nil {
		return CantonTransaction{}, fmt.Errorf("GetUpdateById(%s): %w", updateID, err)
	}
	tx := resp.GetTransaction()
	if tx == nil {
		return CantonTransaction{}, fmt.Errorf("GetUpdateById(%s): not a transaction", updateID)
	}
	returnedUpdateID, err := ParseUpdateID(tx.GetUpdateId())
	if err != nil {
		return CantonTransaction{}, fmt.Errorf("GetUpdateById(%s): %w", updateID, err)
	}
	if returnedUpdateID != updateID {
		return CantonTransaction{}, fmt.Errorf("GetUpdateById(%s): participant returned update %s", updateID, returnedUpdateID)
	}
	return CantonTransaction{
		ParticipantOffset: tx.GetOffset(),
		UpdateID:          returnedUpdateID,
		EffectiveAt:       protoTime(tx.GetEffectiveAt()),
		RecordTime:        protoTime(tx.GetRecordTime()),
		Messages:          messagesFromTx(tx, tmpl, choiceName, c.readScope, c.logger),
	}, nil
}

// messagesFromTx extracts every Wormhole message from a transaction: each
// consuming ExercisedEvent of `choiceName` on `tmpl` that the accepted package
// ids admit at the transaction's record time and that
// readPartySignsExercisedEmitter accepts, whose exercise_result decodes to a
// WormholeMessage record. The event's package id is the package that executed
// the choice, which a Daml upgrade changes.
func messagesFromTx(tx *apiv2.Transaction, tmpl TemplateID, choiceName string, readScope ReadScope, logger *zap.Logger) []CantonMessage {
	createdEvents := createdEventsByContractID(tx)
	var out []CantonMessage
	for _, ev := range tx.GetEvents() {
		ex := ev.GetExercised()
		if ex == nil {
			continue
		}
		if ex.GetChoice() != choiceName {
			continue
		}
		if !templateNameMatches(ex.GetTemplateId(), tmpl) {
			continue
		}
		executingPackageID := ex.GetTemplateId().GetPackageId()
		verdict := tmpl.AcceptedPackageIDs.VerdictForPublish(executingPackageID, protoTime(tx.GetRecordTime()))
		switch verdict {
		case PublishPackageVerdictAcceptedCurrent, PublishPackageVerdictAcceptedRetiring:
		case PublishPackageVerdictRejectedRetiringAfterDeadline:
			recordRetiringPackagePublishAfterDeadline(tx.GetUpdateId(), executingPackageID, protoTime(tx.GetRecordTime()), logger)
			continue
		case PublishPackageVerdictRejectedMissingRecordTime:
			recordPublishWithoutRecordTime(tx.GetUpdateId(), executingPackageID, logger)
			continue
		default:
			recordUnpinnedPublish(tx.GetUpdateId(), executingPackageID, tmpl.AcceptedPackageIDs, logger)
			continue
		}
		// SECURITY: guardianGovernance signs every real Emitter, so a publish
		// on an Emitter it does not sign is on an Emitter of another registry.
		if !readPartySignsExercisedEmitter(ex, createdEvents, tmpl, protoTime(tx.GetRecordTime()), readScope) {
			recordPublishWithoutReadPartyStakeholder(tx.GetUpdateId(), logger)
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
		acceptedPackageRole, isAccepted := verdict.AcceptedRole()
		if !isAccepted {
			panic("canton: rejected publish verdict reached decoding")
		}
		msg.AcceptedPackageRole = acceptedPackageRole
		out = append(out, msg)
	}
	return out
}

// templateNameMatches compares module and entity only, ignoring the package id.
func templateNameMatches(id *apiv2.Identifier, tmpl TemplateID) bool {
	if id == nil {
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
// consistencyLevel are Daml Int -> Value.int64; payload is the Daml
// MessagePayload single-constructor type -> Value.variant (messagePayloadField).
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
	payload, err := messagePayloadField(fields, "payload")
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
	if emitterID > MaxEmitterID {
		return CantonMessage{}, fmt.Errorf("emitterId exceeds MaxEmitterID (emitterId=%d)", emitterID)
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

// messagePayloadConstructor is the constructor of Wormhole.Core.Bytes.MessagePayload.
const messagePayloadConstructor = "MessagePayload"

// messagePayloadField reads a Daml MessagePayload: a Value.variant with
// constructor messagePayloadConstructor whose value is lowercase even-length
// hex without a 0x prefix. This is the ledger's own constructor rule.
func messagePayloadField(fields map[string]*apiv2.Value, label string) ([]byte, error) {
	v, ok := fields[label]
	if !ok {
		return nil, fmt.Errorf("missing field %q", label)
	}
	variant := v.GetVariant()
	if variant == nil {
		return nil, fmt.Errorf("field %q is not a variant", label)
	}
	if variant.GetConstructor() != messagePayloadConstructor {
		return nil, fmt.Errorf("field %q has constructor %q, expected %q", label, variant.GetConstructor(), messagePayloadConstructor)
	}
	textValue, ok := variant.GetValue().GetSum().(*apiv2.Value_Text)
	if !ok {
		return nil, fmt.Errorf("field %q does not hold text", label)
	}
	payloadHex := textValue.Text
	if !isLowercaseHex(payloadHex) {
		return nil, fmt.Errorf("field %q is not lowercase hex", label)
	}
	if len(payloadHex)%2 != 0 {
		return nil, fmt.Errorf("field %q has an odd hex length %d", label, len(payloadHex))
	}
	payload, err := hex.DecodeString(payloadHex)
	if err != nil {
		return nil, fmt.Errorf("field %q is not valid hex: %w", label, err)
	}
	return payload, nil
}

func isLowercaseHex(text string) bool {
	for _, character := range text {
		if !((character >= '0' && character <= '9') || (character >= 'a' && character <= 'f')) {
			return false
		}
	}
	return true
}

func intField(fields map[string]*apiv2.Value, label string) (int64, error) {
	v, ok := fields[label]
	if !ok {
		return 0, fmt.Errorf("missing field %q", label)
	}
	return v.GetInt64(), nil
}

// protoTime is the zero time.Time for a missing, epoch-zero, or out-of-range
// timestamp, so the callers' missing-time checks reject it.
func protoTime(ts *timestamppb.Timestamp) time.Time {
	if ts == nil || !ts.IsValid() {
		return time.Time{}
	}
	if ts.GetSeconds() == 0 && ts.GetNanos() == 0 {
		return time.Time{}
	}
	return ts.AsTime()
}
