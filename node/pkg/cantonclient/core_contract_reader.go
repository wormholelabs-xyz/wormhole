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

	// ReadActiveCoreContracts returns every active disclosed core contract at
	// activeAtOffset, with its created event blob, or an error when there are
	// more than maxContracts.
	// Maps to StateService.GetActiveContracts.
	ReadActiveCoreContracts(ctx context.Context, activeAtOffset int64, maxContracts int) ([]CreatedCoreContract, error)

	// SubscribeCoreContractUpdates delivers on out, in ledger order, one
	// CoreContractLedgerAdvance per transaction with core contract events and
	// per offset checkpoint after beginExclusive. Offsets strictly increase.
	// Maps to UpdateService.GetUpdates in the ACS_DELTA shape.
	SubscribeCoreContractUpdates(ctx context.Context, beginExclusive int64, out chan<- CoreContractLedgerAdvance) (*Subscription, error)

	Close() error
}

// MaxActiveCoreContractsPerSnapshot bounds the maxContracts of one ReadActiveCoreContracts call.
const MaxActiveCoreContractsPerSnapshot = 1 << 23

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

// eventFormat filters the disclosed core templates of every core package
// version for the guardianGovernance party, with created event blobs. Template
// filters take a package name, so the decoder rejects each unapproved package id.
func (decoder coreContractDecoder) eventFormat() *apiv2.EventFormat {
	cumulativeFilters := make([]*apiv2.CumulativeFilter, 0, len(disclosedCoreContractTemplates))
	for _, template := range disclosedCoreContractTemplates {
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

func (reader *grpcCoreContractReader) ReadActiveCoreContracts(ctx context.Context, activeAtOffset int64, maxContracts int) ([]CreatedCoreContract, error) {
	if activeAtOffset < 0 {
		return nil, fmt.Errorf("GetActiveContracts: negative offset %d", activeAtOffset)
	}
	if maxContracts < 1 || maxContracts > MaxActiveCoreContractsPerSnapshot {
		return nil, fmt.Errorf("GetActiveContracts: maximum contract count %d is outside [1, %d]", maxContracts, MaxActiveCoreContractsPerSnapshot)
	}
	streamCtx, cancel := context.WithCancel(ctx)
	defer cancel()
	stream, err := reader.state.GetActiveContracts(streamCtx, &apiv2.GetActiveContractsRequest{
		ActiveAtOffset:                 activeAtOffset,
		EventFormat:                    reader.decoder.eventFormat(),
		IncludeIncompleteReassignments: false,
	})
	if err != nil {
		return nil, fmt.Errorf("GetActiveContracts(%d): %w", activeAtOffset, err)
	}
	contracts := []CreatedCoreContract{}
	// The snapshot has one entry per synchronizer that a contract is active on.
	seenContractIDs := map[ContractID]bool{}
	// Terminates: each response appends one contract or returns, and the append count stops at maxContracts.
	for {
		response, err := stream.Recv()
		if errors.Is(err, io.EOF) {
			return contracts, nil
		}
		if err != nil {
			return nil, fmt.Errorf("GetActiveContracts(%d): %w", activeAtOffset, err)
		}
		activeContract := response.GetActiveContract()
		if activeContract == nil {
			return nil, fmt.Errorf("GetActiveContracts(%d): entry %d is not an active contract", activeAtOffset, len(contracts))
		}
		if len(contracts) == maxContracts {
			return nil, fmt.Errorf("GetActiveContracts(%d): more than %d active core contracts", activeAtOffset, maxContracts)
		}
		createdEvent := activeContract.GetCreatedEvent()
		if createdEvent.GetOffset() <= 0 || createdEvent.GetOffset() > activeAtOffset {
			return nil, fmt.Errorf("GetActiveContracts(%d): created event offset %d is outside [1, %d]", activeAtOffset, createdEvent.GetOffset(), activeAtOffset)
		}
		contract, err := reader.decoder.decodeCreatedCoreContract(createdEvent, activeContract.GetSynchronizerId())
		if err != nil {
			return nil, fmt.Errorf("GetActiveContracts(%d): %w", activeAtOffset, err)
		}
		if seenContractIDs[contract.ContractID()] {
			return nil, fmt.Errorf("GetActiveContracts(%d): repeats contract id %s on synchronizer %s", activeAtOffset, contract.ContractID(), contract.SynchronizerID())
		}
		seenContractIDs[contract.ContractID()] = true
		contracts = append(contracts, contract)
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
			advance, advances, err := reader.decoder.decodeUpdatesResponse(response, previousOffset)
			if err != nil {
				subscription.Fail(fmt.Errorf("GetUpdates: %w", err))
				return
			}
			if !advances {
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

// decodeUpdatesResponse returns advances false for a checkpoint at previousOffset, which adds no progress.
func (decoder coreContractDecoder) decodeUpdatesResponse(response *apiv2.GetUpdatesResponse, previousOffset int64) (advance CoreContractLedgerAdvance, advances bool, err error) {
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
