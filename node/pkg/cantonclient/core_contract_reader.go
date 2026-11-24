package cantonclient

import (
	"context"
	"errors"
	"fmt"
	"io"
	"slices"
	"strings"

	"google.golang.org/grpc"
	"google.golang.org/grpc/codes"
	"google.golang.org/grpc/credentials"
	"google.golang.org/grpc/status"

	apiv2 "github.com/certusone/wormhole/node/pkg/cantonclient/proto/gen/com/daml/ledger/api/v2"
)

// CoreContractReader reads the disclosed core contracts as the guardianGovernance party.
type CoreContractReader interface {
	// GetLedgerEnd maps to StateService.GetLedgerEnd.
	GetLedgerEnd(ctx context.Context) (int64, error)

	// ReadPartyIsHosted is true when a connected synchronizer hosts the guardianGovernance party on this participant,
	// at any permission. Maps to StateService.GetConnectedSynchronizers.
	ReadPartyIsHosted(ctx context.Context) (bool, error)

	// ReadActiveCoreContracts returns the active disclosed core contracts at
	// activeAtOffset, with their created event blobs. Above maxReplayNodes
	// ReplayNodes, the result keeps the singletons only and reports ReplayNodesTruncated.
	// Maps to StateService.GetActiveContracts.
	ReadActiveCoreContracts(ctx context.Context, activeAtOffset int64, maxReplayNodes int) (ActiveCoreContracts, error)

	// SubscribeCoreContractUpdates delivers on out, in ledger order, one
	// CoreContractLedgerAdvance per transaction with core contract events and
	// per offset checkpoint after beginExclusive. Offsets strictly increase.
	// Maps to UpdateService.GetUpdates in the ACS_DELTA shape.
	SubscribeCoreContractUpdates(ctx context.Context, beginExclusive int64, out chan<- CoreContractLedgerAdvance) (*Subscription, error)

	Close() error
}

// MaxActiveCoreContractsPerSnapshot bounds the maxReplayNodes of one ReadActiveCoreContracts call.
const MaxActiveCoreContractsPerSnapshot = 1 << 23

// maxActiveSingletonContractsPerTemplate bounds the kept instances of each singleton template; two show a conflict.
const maxActiveSingletonContractsPerTemplate = 3

// maxActiveSingletonContractsPerSnapshot bounds the singleton entries of one read.
const maxActiveSingletonContractsPerSnapshot = maxActiveSingletonContractsPerTemplate * len(singletonCoreContractTemplates)

// ActiveCoreContracts is the active core contracts at one offset. With ReplayNodesTruncated, it holds the singletons only.
type ActiveCoreContracts struct {
	contracts            []CreatedCoreContract
	replayNodesTruncated bool
}

// Contracts is a copy of the kept contracts in snapshot order.
func (snapshot ActiveCoreContracts) Contracts() []CreatedCoreContract {
	return slices.Clone(snapshot.contracts)
}

// ReplayNodesTruncated is true when the snapshot had more than maxReplayNodes ReplayNodes.
func (snapshot ActiveCoreContracts) ReplayNodesTruncated() bool {
	return snapshot.replayNodesTruncated
}

// activeCoreContractCollector keeps at most maxReplayNodes ReplayNodes and
// maxActiveSingletonContractsPerTemplate instances of each singleton template.
// A singleton-only collector has maxReplayNodes 0 and refuses every ReplayNode.
type activeCoreContractCollector struct {
	maxReplayNodes            int
	contracts                 []CreatedCoreContract
	keptContractIDs           map[ContractID]bool
	replayNodeCount           int
	singletonCountsByTemplate map[CoreContractTemplate]int
	receivedEntryCount        int
}

func newActiveCoreContractCollector(maxReplayNodes int) (*activeCoreContractCollector, error) {
	if maxReplayNodes < 1 || maxReplayNodes > MaxActiveCoreContractsPerSnapshot {
		return nil, fmt.Errorf("maximum replay node count %d is outside [1, %d]", maxReplayNodes, MaxActiveCoreContractsPerSnapshot)
	}
	return newCollectorWithReplayNodeLimit(maxReplayNodes), nil
}

func newSingletonContractCollector() *activeCoreContractCollector {
	return newCollectorWithReplayNodeLimit(0)
}

func newCollectorWithReplayNodeLimit(maxReplayNodes int) *activeCoreContractCollector {
	return &activeCoreContractCollector{
		maxReplayNodes:            maxReplayNodes,
		contracts:                 []CreatedCoreContract{},
		keptContractIDs:           map[ContractID]bool{},
		singletonCountsByTemplate: map[CoreContractTemplate]int{},
	}
}

// maxReceivedEntryCount is the largest entry count of one read: every kept contract plus the
// first ReplayNode above the limit, at which the read stops.
func (collector *activeCoreContractCollector) maxReceivedEntryCount() int {
	return collector.maxReplayNodes + 1 + maxActiveSingletonContractsPerSnapshot
}

// countReceivedEntry fails when a read receives more than maxReceivedEntryCount entries.
func (collector *activeCoreContractCollector) countReceivedEntry() error {
	if collector.receivedEntryCount == collector.maxReceivedEntryCount() {
		return fmt.Errorf("more than %d entries", collector.maxReceivedEntryCount())
	}
	collector.receivedEntryCount++
	return nil
}

// keepActiveContract keeps contract. It returns replayNodeLimitExceeded at the first ReplayNode
// above maxReplayNodes; the caller stops the read there.
func (collector *activeCoreContractCollector) keepActiveContract(contract CreatedCoreContract) (replayNodeLimitExceeded bool, err error) {
	template := contract.TemplateID().Template()
	if template == CoreContractTemplateReplayNode {
		if collector.maxReplayNodes == 0 {
			return false, fmt.Errorf("singleton read returned replay node %s", contract.ContractID())
		}
		if collector.replayNodeCount == collector.maxReplayNodes {
			return true, nil
		}
		collector.replayNodeCount++
	} else {
		if collector.singletonCountsByTemplate[template] == maxActiveSingletonContractsPerTemplate {
			return false, fmt.Errorf("more than %d active %s", maxActiveSingletonContractsPerTemplate, template)
		}
		collector.singletonCountsByTemplate[template]++
	}
	if collector.keptContractIDs[contract.ContractID()] {
		return false, fmt.Errorf("repeats contract id %s on synchronizer %s", contract.ContractID(), contract.SynchronizerID())
	}
	collector.keptContractIDs[contract.ContractID()] = true
	collector.contracts = append(collector.contracts, contract)
	return false, nil
}

// collectedActiveCoreContracts is the kept contracts. A singleton-only collector reports truncated ReplayNodes.
func (collector *activeCoreContractCollector) collectedActiveCoreContracts() ActiveCoreContracts {
	if len(collector.contracts) != len(collector.keptContractIDs) {
		panic("cantonclient: kept contract count differs from the kept contract ids")
	}
	singletonCount := 0
	for _, templateCount := range collector.singletonCountsByTemplate {
		singletonCount += templateCount
	}
	if len(collector.contracts) != collector.replayNodeCount+singletonCount {
		panic("cantonclient: kept contract count differs from the replay node and singleton counts")
	}
	isSingletonOnly := collector.maxReplayNodes == 0
	if isSingletonOnly && collector.replayNodeCount != 0 {
		panic("cantonclient: singleton read kept replay nodes")
	}
	return ActiveCoreContracts{contracts: collector.contracts, replayNodesTruncated: isSingletonOnly}
}

// corePackageNameReference is the package-name reference format that template filters require since Canton 3.5.
const corePackageNameReference = "#" + CorePackageName

// CoreContractUpdateKind is the ledger effect of one CoreContractUpdate.
type CoreContractUpdateKind int

const (
	CoreContractUpdateKindCreated CoreContractUpdateKind = iota + 1
	CoreContractUpdateKindArchived
)

// CoreContractUpdate is one create or archive of a disclosed core contract at offset.
type CoreContractUpdate struct {
	offset   int64
	kind     CoreContractUpdateKind
	created  CreatedCoreContract
	archived ArchivedCoreContract
}

// Offset is the participant offset of the transaction of the event.
func (update CoreContractUpdate) Offset() int64 { return update.offset }

func (update CoreContractUpdate) Kind() CoreContractUpdateKind { return update.kind }

func (update CoreContractUpdate) Created() (CreatedCoreContract, bool) {
	if update.kind != CoreContractUpdateKindCreated {
		return CreatedCoreContract{}, false
	}
	return update.created, true
}

func (update CoreContractUpdate) Archived() (ArchivedCoreContract, bool) {
	if update.kind != CoreContractUpdateKindArchived {
		return ArchivedCoreContract{}, false
	}
	return update.archived, true
}

// maxCoreContractUpdatesPerTransaction bounds the core contract events of one delivered transaction.
const maxCoreContractUpdatesPerTransaction = 1 << 16

// CoreContractLedgerAdvance is the ledger progress of one stream element: the
// core contract updates of one transaction, or an offset checkpoint without updates.
type CoreContractLedgerAdvance struct {
	offset  int64
	updates []CoreContractUpdate
}

// Offset is the participant offset that the stream reached.
func (advance CoreContractLedgerAdvance) Offset() int64 { return advance.offset }

// Updates is a copy of the transaction updates, in event order. Empty for an offset checkpoint.
func (advance CoreContractLedgerAdvance) Updates() []CoreContractUpdate {
	return slices.Clone(advance.updates)
}

// IsOffsetCheckpoint is true when the advance carries no core contract update.
func (advance CoreContractLedgerAdvance) IsOffsetCheckpoint() bool { return len(advance.updates) == 0 }

type grpcCoreContractReader struct {
	conn    *grpc.ClientConn
	state   apiv2.StateServiceClient
	update  apiv2.UpdateServiceClient
	decoder coreContractDecoder
}

// NewCoreContractReader dials the Ledger API at rpc. readScope must be a
// guardianGovernance scope. transportCreds follows NewCantonGrpcClient.
func NewCoreContractReader(rpc string, readScope ReadScope, approved ApprovedCorePackages, transportCreds credentials.TransportCredentials, opts ...grpc.DialOption) (CoreContractReader, error) {
	decoder, err := newCoreContractDecoder(readScope, approved)
	if err != nil {
		return nil, err
	}
	conn, err := dialLedgerAPI(rpc, transportCreds, opts...)
	if err != nil {
		return nil, err
	}
	return &grpcCoreContractReader{
		conn:    conn,
		state:   apiv2.NewStateServiceClient(conn),
		update:  apiv2.NewUpdateServiceClient(conn),
		decoder: decoder,
	}, nil
}

func newCoreContractDecoder(readScope ReadScope, approved ApprovedCorePackages) (coreContractDecoder, error) {
	if err := readScope.CheckWasConstructed(); err != nil {
		return coreContractDecoder{}, err
	}
	guardianGovernance, isGuardianGovernance := readScope.GuardianGovernanceParty()
	if !isGuardianGovernance {
		return coreContractDecoder{}, errors.New("cantonclient: core contract reads require a guardianGovernance read scope")
	}
	if len(approved.entries) == 0 {
		return coreContractDecoder{}, errors.New("cantonclient: core contract reads require approved core packages")
	}
	return coreContractDecoder{guardianGovernance: guardianGovernance, approved: approved}, nil
}

func (reader *grpcCoreContractReader) Close() error {
	return reader.conn.Close()
}

func (reader *grpcCoreContractReader) GetLedgerEnd(ctx context.Context) (int64, error) {
	ctx, cancel := context.WithTimeout(ctx, unaryRPCTimeout)
	defer cancel()
	response, err := reader.state.GetLedgerEnd(ctx, &apiv2.GetLedgerEndRequest{})
	if err != nil {
		return 0, fmt.Errorf("GetLedgerEnd: %w", err)
	}
	return response.GetOffset(), nil
}

func (reader *grpcCoreContractReader) ReadPartyIsHosted(ctx context.Context) (bool, error) {
	ctx, cancel := context.WithTimeout(ctx, unaryRPCTimeout)
	defer cancel()
	response, err := reader.state.GetConnectedSynchronizers(ctx, &apiv2.GetConnectedSynchronizersRequest{Party: reader.decoder.guardianGovernance.String()})
	if err != nil {
		return false, fmt.Errorf("GetConnectedSynchronizers: %w", err)
	}
	for _, synchronizer := range response.GetConnectedSynchronizers() {
		if synchronizer.GetPermission() != apiv2.ParticipantPermission_PARTICIPANT_PERMISSION_UNSPECIFIED {
			return true, nil
		}
	}
	return false, nil
}

// eventFormat filters the disclosed core templates of every core package
// version for the guardianGovernance party, with created event blobs. Template
// filters take a package name, so the decoder rejects each unapproved package id.
func (decoder coreContractDecoder) eventFormat() *apiv2.EventFormat {
	return decoder.eventFormatForTemplates(disclosedCoreContractTemplates[:])
}

// singletonEventFormat is eventFormat without ReplayNode.
func (decoder coreContractDecoder) singletonEventFormat() *apiv2.EventFormat {
	return decoder.eventFormatForTemplates(singletonCoreContractTemplates[:])
}

func (decoder coreContractDecoder) eventFormatForTemplates(templates []CoreContractTemplate) *apiv2.EventFormat {
	cumulativeFilters := make([]*apiv2.CumulativeFilter, 0, len(templates))
	for _, template := range templates {
		cumulativeFilters = append(cumulativeFilters, &apiv2.CumulativeFilter{
			IdentifierFilter: &apiv2.CumulativeFilter_TemplateFilter{
				TemplateFilter: &apiv2.TemplateFilter{
					TemplateId: &apiv2.Identifier{
						PackageId:  corePackageNameReference,
						ModuleName: template.ModuleName(),
						EntityName: template.EntityName(),
					},
					IncludeCreatedEventBlob: true,
				},
			},
		})
	}
	return &apiv2.EventFormat{
		FiltersByParty: map[string]*apiv2.Filters{
			decoder.guardianGovernance.String(): {Cumulative: cumulativeFilters},
		},
		Verbose: true,
	}
}

// ReadActiveCoreContracts reads every disclosed core contract. At the first ReplayNode above
// maxReplayNodes it stops that read and reads the singletons alone at the same offset.
func (reader *grpcCoreContractReader) ReadActiveCoreContracts(ctx context.Context, activeAtOffset int64, maxReplayNodes int) (ActiveCoreContracts, error) {
	if activeAtOffset < 0 {
		return ActiveCoreContracts{}, fmt.Errorf("GetActiveContracts: negative offset %d", activeAtOffset)
	}
	collector, err := newActiveCoreContractCollector(maxReplayNodes)
	if err != nil {
		return ActiveCoreContracts{}, fmt.Errorf("GetActiveContracts: %w", err)
	}
	replayNodeLimitExceeded, err := reader.receiveActiveCoreContracts(ctx, activeAtOffset, reader.decoder.eventFormat(), collector)
	if err != nil {
		return ActiveCoreContracts{}, err
	}
	if !replayNodeLimitExceeded {
		return collector.collectedActiveCoreContracts(), nil
	}
	singletonCollector := newSingletonContractCollector()
	if _, err := reader.receiveActiveCoreContracts(ctx, activeAtOffset, reader.decoder.singletonEventFormat(), singletonCollector); err != nil {
		return ActiveCoreContracts{}, fmt.Errorf("singleton read: %w", err)
	}
	snapshot := singletonCollector.collectedActiveCoreContracts()
	if !snapshot.ReplayNodesTruncated() {
		panic("cantonclient: singleton read did not report truncated replay nodes")
	}
	return snapshot, nil
}

// receiveActiveCoreContracts feeds one GetActiveContracts stream into collector until io.EOF or
// the first ReplayNode above the collector limit.
func (reader *grpcCoreContractReader) receiveActiveCoreContracts(ctx context.Context, activeAtOffset int64, eventFormat *apiv2.EventFormat, collector *activeCoreContractCollector) (replayNodeLimitExceeded bool, err error) {
	streamCtx, cancel := context.WithCancel(ctx)
	defer cancel()
	stream, err := reader.state.GetActiveContracts(streamCtx, &apiv2.GetActiveContractsRequest{
		ActiveAtOffset:                 activeAtOffset,
		EventFormat:                    eventFormat,
		IncludeIncompleteReassignments: false,
	})
	if err != nil {
		return false, fmt.Errorf("GetActiveContracts(%d): %w", activeAtOffset, err)
	}
	// The snapshot has one entry per synchronizer that a contract is active on; the collector refuses a repeated kept contract id.
	// collector.maxReceivedEntryCount bounds the loop.
	for entryIndex := 0; ; entryIndex++ {
		response, err := stream.Recv()
		if errors.Is(err, io.EOF) {
			return false, nil
		}
		if err != nil {
			return false, fmt.Errorf("GetActiveContracts(%d): %w", activeAtOffset, err)
		}
		if err := collector.countReceivedEntry(); err != nil {
			return false, fmt.Errorf("GetActiveContracts(%d): %w", activeAtOffset, err)
		}
		activeContract := response.GetActiveContract()
		if activeContract == nil {
			return false, fmt.Errorf("GetActiveContracts(%d): entry %d is not an active contract", activeAtOffset, entryIndex)
		}
		createdEvent := activeContract.GetCreatedEvent()
		if createdEvent.GetOffset() <= 0 || createdEvent.GetOffset() > activeAtOffset {
			return false, fmt.Errorf("GetActiveContracts(%d): created event offset %d is outside [1, %d]", activeAtOffset, createdEvent.GetOffset(), activeAtOffset)
		}
		contract, err := reader.decoder.decodeCreatedCoreContract(createdEvent, activeContract.GetSynchronizerId())
		if err != nil {
			return false, fmt.Errorf("GetActiveContracts(%d): %w", activeAtOffset, err)
		}
		replayNodeLimitExceeded, err := collector.keepActiveContract(contract)
		if err != nil {
			return false, fmt.Errorf("GetActiveContracts(%d): %w", activeAtOffset, err)
		}
		if replayNodeLimitExceeded {
			return true, nil
		}
	}
}

func (reader *grpcCoreContractReader) SubscribeCoreContractUpdates(ctx context.Context, beginExclusive int64, out chan<- CoreContractLedgerAdvance) (*Subscription, error) {
	if beginExclusive < 0 {
		return nil, fmt.Errorf("GetUpdates: negative begin offset %d", beginExclusive)
	}
	streamCtx, cancel := context.WithCancel(ctx)
	stream, err := reader.update.GetUpdates(streamCtx, &apiv2.GetUpdatesRequest{
		BeginExclusive: beginExclusive,
		UpdateFormat: &apiv2.UpdateFormat{
			IncludeTransactions: &apiv2.TransactionFormat{
				TransactionShape: apiv2.TransactionShape_TRANSACTION_SHAPE_ACS_DELTA,
				EventFormat:      reader.decoder.eventFormat(),
			},
			// Requested so that a core contract moving between synchronizers fails the stream.
			IncludeReassignments: reader.decoder.eventFormat(),
		},
	})
	if err != nil {
		cancel()
		return nil, fmt.Errorf("GetUpdates: %w", err)
	}
	subscription := NewSubscription(cancel)
	go func() {
		defer subscription.Close()
		previousOffset := beginExclusive
		// Runs until cancellation or a stream error.
		for {
			response, err := stream.Recv()
			if err != nil {
				if streamCtx.Err() != nil {
					return
				}
				subscription.Fail(fmt.Errorf("GetUpdates stream ended: %w", err))
				return
			}
			advance, movesTheOffset, err := reader.decoder.decodeUpdatesResponse(response, previousOffset)
			if err != nil {
				subscription.Fail(fmt.Errorf("GetUpdates: %w: %w", ErrCoreContractStreamDecode, err))
				return
			}
			if !movesTheOffset {
				continue
			}
			previousOffset = advance.offset
			select {
			case <-streamCtx.Done():
				return
			case out <- advance:
			}
		}
	}()
	return subscription, nil
}

// decodeUpdatesResponse returns movesTheOffset false for a checkpoint at previousOffset.
func (decoder coreContractDecoder) decodeUpdatesResponse(response *apiv2.GetUpdatesResponse, previousOffset int64) (advance CoreContractLedgerAdvance, movesTheOffset bool, err error) {
	switch update := response.GetUpdate().(type) {
	case *apiv2.GetUpdatesResponse_Transaction:
		updates, err := decoder.decodeTransactionCoreContractUpdates(update.Transaction, previousOffset)
		if err != nil {
			return CoreContractLedgerAdvance{}, false, err
		}
		if len(updates) == 0 || len(updates) > maxCoreContractUpdatesPerTransaction {
			return CoreContractLedgerAdvance{}, false, fmt.Errorf("transaction at offset %d has %d core contract updates, expected [1, %d]", update.Transaction.GetOffset(), len(updates), maxCoreContractUpdatesPerTransaction)
		}
		return CoreContractLedgerAdvance{offset: update.Transaction.GetOffset(), updates: updates}, true, nil
	case *apiv2.GetUpdatesResponse_OffsetCheckpoint:
		checkpointOffset := update.OffsetCheckpoint.GetOffset()
		if checkpointOffset < previousOffset {
			return CoreContractLedgerAdvance{}, false, fmt.Errorf("offset checkpoint %d is below offset %d", checkpointOffset, previousOffset)
		}
		if checkpointOffset == previousOffset {
			return CoreContractLedgerAdvance{}, false, nil
		}
		return CoreContractLedgerAdvance{offset: checkpointOffset}, true, nil
	default:
		return CoreContractLedgerAdvance{}, false, fmt.Errorf("unexpected update %T after offset %d", update, previousOffset)
	}
}

// ErrCoreContractStreamDecode wraps a SubscribeCoreContractUpdates failure on an update that the decoder refuses.
// A reconnect from the same offset receives the same update.
var ErrCoreContractStreamDecode = errors.New("cantonclient: core contract update refused by the decoder")

// prunedDataAccessedErrorID is the Canton error id of a read below the pruning offset (state_service.proto).
const prunedDataAccessedErrorID = "PARTICIPANT_PRUNED_DATA_ACCESSED"

// IsPrunedOffsetError is true for FAILED_PRECONDITION/PARTICIPANT_PRUNED_DATA_ACCESSED anywhere in the error chain.
func IsPrunedOffsetError(err error) bool {
	grpcStatus, hasStatus := status.FromError(err)
	if !hasStatus || err == nil {
		return false
	}
	return grpcStatus.Code() == codes.FailedPrecondition && strings.Contains(grpcStatus.Message(), prunedDataAccessedErrorID)
}

// decodeTransactionCoreContractUpdates decodes every event of an ACS_DELTA
// transaction whose offset must follow previousOffset.
func (decoder coreContractDecoder) decodeTransactionCoreContractUpdates(transaction *apiv2.Transaction, previousOffset int64) ([]CoreContractUpdate, error) {
	if transaction == nil {
		return nil, errors.New("missing transaction")
	}
	offset := transaction.GetOffset()
	if offset <= previousOffset {
		return nil, fmt.Errorf("transaction offset %d does not follow offset %d", offset, previousOffset)
	}
	if len(transaction.GetEvents()) == 0 {
		return nil, fmt.Errorf("transaction at offset %d has no events", offset)
	}
	updates := make([]CoreContractUpdate, 0, len(transaction.GetEvents()))
	for eventIndex, event := range transaction.GetEvents() {
		switch ledgerEvent := event.GetEvent().(type) {
		case *apiv2.Event_Created:
			if ledgerEvent.Created.GetOffset() != offset {
				return nil, fmt.Errorf("transaction at offset %d: event %d has offset %d", offset, eventIndex, ledgerEvent.Created.GetOffset())
			}
			created, err := decoder.decodeCreatedCoreContract(ledgerEvent.Created, transaction.GetSynchronizerId())
			if err != nil {
				return nil, fmt.Errorf("transaction at offset %d: event %d: %w", offset, eventIndex, err)
			}
			updates = append(updates, CoreContractUpdate{offset: offset, kind: CoreContractUpdateKindCreated, created: created})
		case *apiv2.Event_Archived:
			if ledgerEvent.Archived.GetOffset() != offset {
				return nil, fmt.Errorf("transaction at offset %d: event %d has offset %d", offset, eventIndex, ledgerEvent.Archived.GetOffset())
			}
			archived, err := decoder.decodeArchivedCoreContract(ledgerEvent.Archived)
			if err != nil {
				return nil, fmt.Errorf("transaction at offset %d: event %d: %w", offset, eventIndex, err)
			}
			updates = append(updates, CoreContractUpdate{offset: offset, kind: CoreContractUpdateKindArchived, archived: archived})
		default:
			return nil, fmt.Errorf("transaction at offset %d: event %d is %T, expected a created or archived event", offset, eventIndex, ledgerEvent)
		}
	}
	return updates, nil
}
