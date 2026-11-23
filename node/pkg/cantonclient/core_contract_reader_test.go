package cantonclient

import (
	"context"
	"errors"
	"io"
	"testing"
	"time"

	"github.com/stretchr/testify/assert"
	"github.com/stretchr/testify/require"
	"google.golang.org/grpc"

	apiv2 "github.com/certusone/wormhole/node/pkg/cantonclient/proto/gen/com/daml/ledger/api/v2"
)

// scriptedCoreContractStream embeds grpc.ClientStream (nil) and returns responses in order, then io.EOF.
type scriptedCoreContractStream[Response any] struct {
	grpc.ClientStream
	responses []*Response
	finalErr  error
}

func (stream *scriptedCoreContractStream[Response]) Recv() (*Response, error) {
	if len(stream.responses) == 0 {
		if stream.finalErr != nil {
			return nil, stream.finalErr
		}
		return nil, io.EOF
	}
	response := stream.responses[0]
	stream.responses = stream.responses[1:]
	return response, nil
}

type fakeActiveContractsService struct {
	apiv2.StateServiceClient
	stream  *scriptedCoreContractStream[apiv2.GetActiveContractsResponse]
	request *apiv2.GetActiveContractsRequest
}

func (service *fakeActiveContractsService) GetActiveContracts(_ context.Context, request *apiv2.GetActiveContractsRequest, _ ...grpc.CallOption) (apiv2.StateService_GetActiveContractsClient, error) {
	service.request = request
	return service.stream, nil
}

type fakeCoreUpdatesService struct {
	apiv2.UpdateServiceClient
	stream  apiv2.UpdateService_GetUpdatesClient
	request *apiv2.GetUpdatesRequest
}

func (service *fakeCoreUpdatesService) GetUpdates(_ context.Context, request *apiv2.GetUpdatesRequest, _ ...grpc.CallOption) (apiv2.UpdateService_GetUpdatesClient, error) {
	service.request = request
	return service.stream, nil
}

func activeContractResponse(event *apiv2.CreatedEvent) *apiv2.GetActiveContractsResponse {
	return &apiv2.GetActiveContractsResponse{ContractEntry: &apiv2.GetActiveContractsResponse_ActiveContract{
		ActiveContract: &apiv2.ActiveContract{CreatedEvent: event, SynchronizerId: testSynchronizerID},
	}}
}

func testCoreContractReaderWithSnapshot(t *testing.T, responses ...*apiv2.GetActiveContractsResponse) (*grpcCoreContractReader, *fakeActiveContractsService) {
	t.Helper()
	service := &fakeActiveContractsService{stream: &scriptedCoreContractStream[apiv2.GetActiveContractsResponse]{responses: responses}}
	return &grpcCoreContractReader{state: service, decoder: testCoreContractDecoder(t)}, service
}

// checkFiltersTheDisclosedCoreTemplates requires the guardianGovernance party filter with blobs for the four core templates.
func checkFiltersTheDisclosedCoreTemplates(t *testing.T, eventFormat *apiv2.EventFormat) {
	t.Helper()
	require.NotNil(t, eventFormat)
	assert.True(t, eventFormat.GetVerbose())
	assert.Nil(t, eventFormat.GetFiltersForAnyParty())
	require.Len(t, eventFormat.GetFiltersByParty(), 1)
	filters, found := eventFormat.GetFiltersByParty()[governanceParty]
	require.True(t, found)
	wantTemplates := []string{
		"#wormhole-core:Wormhole.Core.State:CoreState",
		"#wormhole-core:Wormhole.Core.State:EmitterRegistry",
		"#wormhole-core:Wormhole.Core.State:ReplayRootRegistry",
		"#wormhole-core:Wormhole.Core.Replay:ReplayNode",
	}
	gotTemplates := []string{}
	for _, cumulativeFilter := range filters.GetCumulative() {
		templateFilter := cumulativeFilter.GetTemplateFilter()
		require.NotNil(t, templateFilter)
		assert.True(t, templateFilter.GetIncludeCreatedEventBlob())
		identifier := templateFilter.GetTemplateId()
		gotTemplates = append(gotTemplates, identifier.GetPackageId()+":"+identifier.GetModuleName()+":"+identifier.GetEntityName())
	}
	assert.Equal(t, wantTemplates, gotTemplates)
}

func TestReadActiveCoreContractsReturnsEveryEntry(t *testing.T) {
	reader, service := testCoreContractReaderWithSnapshot(t,
		activeContractResponse(createdCoreEvent(CoreContractTemplateCoreState, 3, testCoreContractIDHex, nil)),
		activeContractResponse(createdCoreEvent(CoreContractTemplateReplayNode, 10, testOtherContractIDHex, nil)),
	)
	contracts, err := reader.ReadActiveCoreContracts(context.Background(), 10, 2)
	require.NoError(t, err)
	require.Len(t, contracts, 2)
	assert.Equal(t, CoreContractTemplateCoreState, contracts[0].TemplateID().Template())
	assert.Equal(t, testCoreContractIDHex, contracts[0].ContractID().String())
	assert.Equal(t, CoreContractTemplateReplayNode, contracts[1].TemplateID().Template())
	assert.Equal(t, testOtherContractIDHex, contracts[1].ContractID().String())
	assert.Equal(t, len(testCreatedEventBlob), contracts[1].CreatedEventBlobByteCount())

	assert.Equal(t, int64(10), service.request.GetActiveAtOffset())
	assert.False(t, service.request.GetIncludeIncompleteReassignments())
	checkFiltersTheDisclosedCoreTemplates(t, service.request.GetEventFormat())
}

func TestReadActiveCoreContractsEmptySnapshot(t *testing.T) {
	reader, _ := testCoreContractReaderWithSnapshot(t)
	contracts, err := reader.ReadActiveCoreContracts(context.Background(), 10, 1)
	require.NoError(t, err)
	assert.Empty(t, contracts)
}

func TestReadActiveCoreContractsCap(t *testing.T) {
	threeContracts := []*apiv2.GetActiveContractsResponse{
		activeContractResponse(createdCoreEvent(CoreContractTemplateReplayNode, 1, testCoreContractIDHex, nil)),
		activeContractResponse(createdCoreEvent(CoreContractTemplateReplayNode, 2, testOtherContractIDHex, nil)),
		activeContractResponse(createdCoreEvent(CoreContractTemplateCoreState, 3, testThirdContractIDHex, nil)),
	}
	cases := []struct {
		name          string
		maxContracts  int
		wantContracts int
		wantErr       string
	}{
		{name: "cap above the count", maxContracts: 4, wantContracts: 3},
		{name: "cap equal to the count", maxContracts: 3, wantContracts: 3},
		{name: "cap one below the count", maxContracts: 2, wantErr: "more than 2 active core contracts"},
		{name: "cap of one", maxContracts: 1, wantErr: "more than 1 active core contracts"},
		{name: "zero cap", maxContracts: 0, wantErr: "outside [1, 8388608]"},
		{name: "negative cap", maxContracts: -1, wantErr: "outside [1, 8388608]"},
		{name: "cap above the upper bound", maxContracts: MaxActiveCoreContractsPerSnapshot + 1, wantErr: "outside [1, 8388608]"},
	}
	for _, tc := range cases {
		t.Run(tc.name, func(t *testing.T) {
			reader, _ := testCoreContractReaderWithSnapshot(t, threeContracts...)
			contracts, err := reader.ReadActiveCoreContracts(context.Background(), 3, tc.maxContracts)
			if tc.wantErr != "" {
				require.ErrorContains(t, err, tc.wantErr)
				assert.Nil(t, contracts)
				return
			}
			require.NoError(t, err)
			assert.Len(t, contracts, tc.wantContracts)
		})
	}
}

func TestReadActiveCoreContractsRejects(t *testing.T) {
	incompleteUnassigned := &apiv2.GetActiveContractsResponse{ContractEntry: &apiv2.GetActiveContractsResponse_IncompleteUnassigned{
		IncompleteUnassigned: &apiv2.IncompleteUnassigned{CreatedEvent: createdCoreEvent(CoreContractTemplateCoreState, 3, testCoreContractIDHex, nil)},
	}}
	unapproved := createdCoreEvent(CoreContractTemplateCoreState, 3, testCoreContractIDHex, nil)
	unapproved.TemplateId = coreTemplateIdentifier(testUnapprovedPackageID, CoreContractTemplateCoreState)
	cases := []struct {
		name           string
		activeAtOffset int64
		responses      []*apiv2.GetActiveContractsResponse
		finalErr       error
		wantErr        string
	}{
		{name: "negative offset", activeAtOffset: -1, wantErr: "negative offset"},
		{name: "empty entry", activeAtOffset: 5, responses: []*apiv2.GetActiveContractsResponse{{}}, wantErr: "is not an active contract"},
		{name: "incomplete reassignment", activeAtOffset: 5, responses: []*apiv2.GetActiveContractsResponse{incompleteUnassigned}, wantErr: "is not an active contract"},
		{name: "created after the snapshot offset", activeAtOffset: 2, responses: []*apiv2.GetActiveContractsResponse{activeContractResponse(createdCoreEvent(CoreContractTemplateCoreState, 3, testCoreContractIDHex, nil))}, wantErr: "outside [1, 2]"},
		{name: "created at offset zero", activeAtOffset: 5, responses: []*apiv2.GetActiveContractsResponse{activeContractResponse(createdCoreEvent(CoreContractTemplateCoreState, 0, testCoreContractIDHex, nil))}, wantErr: "outside [1, 5]"},
		{name: "unapproved package id", activeAtOffset: 5, responses: []*apiv2.GetActiveContractsResponse{activeContractResponse(unapproved)}, wantErr: "not in the approved list"},
		{name: "contract active on two synchronizers", activeAtOffset: 5, responses: []*apiv2.GetActiveContractsResponse{
			activeContractResponse(createdCoreEvent(CoreContractTemplateCoreState, 3, testCoreContractIDHex, nil)),
			activeContractResponse(createdCoreEvent(CoreContractTemplateCoreState, 3, testCoreContractIDHex, nil)),
		}, wantErr: "repeats contract id"},
		{name: "stream error after an entry", activeAtOffset: 5, responses: []*apiv2.GetActiveContractsResponse{activeContractResponse(createdCoreEvent(CoreContractTemplateCoreState, 3, testCoreContractIDHex, nil))}, finalErr: errors.New("connection reset"), wantErr: "connection reset"},
	}
	for _, tc := range cases {
		t.Run(tc.name, func(t *testing.T) {
			reader, service := testCoreContractReaderWithSnapshot(t, tc.responses...)
			service.stream.finalErr = tc.finalErr
			contracts, err := reader.ReadActiveCoreContracts(context.Background(), tc.activeAtOffset, 10)
			require.ErrorContains(t, err, tc.wantErr)
			assert.Nil(t, contracts)
		})
	}
}

func coreTransaction(offset int64, events ...*apiv2.Event) *apiv2.GetUpdatesResponse {
	return &apiv2.GetUpdatesResponse{Update: &apiv2.GetUpdatesResponse_Transaction{Transaction: &apiv2.Transaction{
		Offset:         offset,
		SynchronizerId: testSynchronizerID,
		Events:         events,
	}}}
}

func coreReassignment(offset int64, events ...*apiv2.ReassignmentEvent) *apiv2.GetUpdatesResponse {
	return &apiv2.GetUpdatesResponse{Update: &apiv2.GetUpdatesResponse_Reassignment{Reassignment: &apiv2.Reassignment{
		Offset:         offset,
		SynchronizerId: testSynchronizerID,
		Events:         events,
	}}}
}

func createdLedgerEvent(event *apiv2.CreatedEvent) *apiv2.Event {
	return &apiv2.Event{Event: &apiv2.Event_Created{Created: event}}
}

func archivedLedgerEvent(event *apiv2.ArchivedEvent) *apiv2.Event {
	return &apiv2.Event{Event: &apiv2.Event_Archived{Archived: event}}
}

func offsetCheckpointResponse(offset int64) *apiv2.GetUpdatesResponse {
	return &apiv2.GetUpdatesResponse{Update: &apiv2.GetUpdatesResponse_OffsetCheckpoint{OffsetCheckpoint: &apiv2.OffsetCheckpoint{Offset: offset}}}
}

// subscribeAndCollectCoreUpdates runs the stream to its end and returns the delivered updates and the terminal error.
func subscribeAndCollectCoreUpdates(t *testing.T, beginExclusive int64, responses ...*apiv2.GetUpdatesResponse) ([]CoreContractUpdate, error, *apiv2.GetUpdatesRequest) {
	t.Helper()
	service := &fakeCoreUpdatesService{stream: &scriptedCoreContractStream[apiv2.GetUpdatesResponse]{responses: responses}}
	reader := &grpcCoreContractReader{update: service, decoder: testCoreContractDecoder(t)}
	out := make(chan CoreContractUpdate, 4)
	subscription, err := reader.SubscribeCoreContractUpdates(context.Background(), beginExclusive, out)
	require.NoError(t, err)
	defer subscription.Unsubscribe()
	updates := []CoreContractUpdate{}
	deadline := time.After(2 * time.Second)
	for {
		select {
		case update := <-out:
			updates = append(updates, update)
		case streamErr := <-subscription.Err():
			for drained := false; !drained; {
				select {
				case update := <-out:
					updates = append(updates, update)
				default:
					drained = true
				}
			}
			return updates, streamErr, service.request
		case <-deadline:
			t.Fatal("stream did not end")
		}
	}
}

func TestSubscribeCoreContractUpdatesDeliversInOrder(t *testing.T) {
	updates, streamErr, request := subscribeAndCollectCoreUpdates(t, 4,
		coreTransaction(5,
			createdLedgerEvent(createdCoreEvent(CoreContractTemplateReplayNode, 5, testCoreContractIDHex, nil)),
		),
		offsetCheckpointResponse(6),
		coreTransaction(8,
			archivedLedgerEvent(archivedCoreEvent(CoreContractTemplateReplayNode, 8, testCoreContractIDHex)),
			createdLedgerEvent(createdCoreEvent(CoreContractTemplateReplayNode, 8, testOtherContractIDHex, map[string]*apiv2.Value{"prefix": digestNibblePrefixVal(pinnedPackageID, "a70")})),
		),
	)
	require.ErrorContains(t, streamErr, "GetUpdates stream ended")
	require.ErrorIs(t, streamErr, io.EOF)

	assert.Equal(t, int64(4), request.GetBeginExclusive())
	assert.Nil(t, request.EndInclusive)
	transactionFormat := request.GetUpdateFormat().GetIncludeTransactions()
	assert.Equal(t, apiv2.TransactionShape_TRANSACTION_SHAPE_ACS_DELTA, transactionFormat.GetTransactionShape())
	checkFiltersTheDisclosedCoreTemplates(t, transactionFormat.GetEventFormat())
	checkFiltersTheDisclosedCoreTemplates(t, request.GetUpdateFormat().GetIncludeReassignments())

	require.Len(t, updates, 3)
	assert.Equal(t, []int64{5, 8, 8}, []int64{updates[0].Offset(), updates[1].Offset(), updates[2].Offset()})

	created, isCreated := updates[0].Created()
	require.True(t, isCreated)
	assert.Equal(t, CoreContractUpdateKindCreated, updates[0].Kind())
	assert.Equal(t, testCoreContractIDHex, created.ContractID().String())
	_, isArchived := updates[0].Archived()
	assert.False(t, isArchived)

	archived, isArchived := updates[1].Archived()
	require.True(t, isArchived)
	assert.Equal(t, CoreContractUpdateKindArchived, updates[1].Kind())
	assert.Equal(t, testCoreContractIDHex, archived.ContractID().String())
	assert.Equal(t, CoreContractTemplateReplayNode, archived.TemplateID().Template())
	_, isCreated = updates[1].Created()
	assert.False(t, isCreated)

	created, isCreated = updates[2].Created()
	require.True(t, isCreated)
	replayNodeKey, isReplayNode := created.ReplayNodeKey()
	require.True(t, isReplayNode)
	assert.Equal(t, "a70", replayNodeKey.Prefix().String())
	assert.Equal(t, testSynchronizerID, created.SynchronizerID().String())
}

func TestSubscribeCoreContractUpdatesRejects(t *testing.T) {
	unapprovedArchive := archivedCoreEvent(CoreContractTemplateReplayNode, 6, testCoreContractIDHex)
	unapprovedArchive.TemplateId = coreTemplateIdentifier(testUnapprovedPackageID, CoreContractTemplateReplayNode)
	unknownCreate := createdCoreEvent(CoreContractTemplateCoreState, 6, testCoreContractIDHex, nil)
	unknownCreate.TemplateId = &apiv2.Identifier{PackageId: pinnedPackageID, ModuleName: coreStateModuleName, EntityName: "Emitter"}
	validFirst := coreTransaction(5, createdLedgerEvent(createdCoreEvent(CoreContractTemplateCoreState, 5, testCoreContractIDHex, nil)))
	cases := []struct {
		name    string
		second  *apiv2.GetUpdatesResponse
		wantErr string
	}{
		{name: "offset equal to the previous one", second: coreTransaction(5, archivedLedgerEvent(archivedCoreEvent(CoreContractTemplateCoreState, 5, testCoreContractIDHex))), wantErr: "does not follow offset 5"},
		{name: "offset below the previous one", second: coreTransaction(4, archivedLedgerEvent(archivedCoreEvent(CoreContractTemplateCoreState, 4, testCoreContractIDHex))), wantErr: "does not follow offset 5"},
		{name: "transaction without events", second: coreTransaction(6), wantErr: "has no events"},
		{name: "event offset other than the transaction", second: coreTransaction(6, archivedLedgerEvent(archivedCoreEvent(CoreContractTemplateCoreState, 7, testCoreContractIDHex))), wantErr: "event 0 has offset 7"},
		{name: "exercised event", second: coreTransaction(6, &apiv2.Event{Event: &apiv2.Event_Exercised{Exercised: &apiv2.ExercisedEvent{Offset: 6}}}), wantErr: "expected a created or archived event"},
		{name: "empty event", second: coreTransaction(6, &apiv2.Event{}), wantErr: "expected a created or archived event"},
		{name: "archive of an unapproved package id", second: coreTransaction(6, archivedLedgerEvent(unapprovedArchive)), wantErr: "not in the approved list"},
		{name: "create of an unknown template", second: coreTransaction(6, createdLedgerEvent(unknownCreate)), wantErr: "not a disclosed core template"},
		{name: "unassignment of a core contract", second: coreReassignment(6, &apiv2.ReassignmentEvent{Event: &apiv2.ReassignmentEvent_Unassigned{Unassigned: &apiv2.UnassignedEvent{
			Offset: 6, ContractId: testCoreContractIDHex, TemplateId: coreTemplateIdentifier(pinnedPackageID, CoreContractTemplateReplayNode), PackageName: CorePackageName,
			Source: testSynchronizerID, Target: "other-synchronizer::" + testLocalNetFingerprint,
		}}}), wantErr: "unexpected update"},
		{name: "assignment of a core contract", second: coreReassignment(6, &apiv2.ReassignmentEvent{Event: &apiv2.ReassignmentEvent_Assigned{Assigned: &apiv2.AssignedEvent{
			Source: "other-synchronizer::" + testLocalNetFingerprint, Target: testSynchronizerID,
			CreatedEvent: createdCoreEvent(CoreContractTemplateReplayNode, 6, testCoreContractIDHex, nil),
		}}}), wantErr: "unexpected update"},
		{name: "empty update", second: &apiv2.GetUpdatesResponse{}, wantErr: "unexpected update"},
	}
	for _, tc := range cases {
		t.Run(tc.name, func(t *testing.T) {
			updates, streamErr, _ := subscribeAndCollectCoreUpdates(t, 4, validFirst, tc.second)
			require.ErrorContains(t, streamErr, tc.wantErr)
			require.Len(t, updates, 1)
			assert.Equal(t, int64(5), updates[0].Offset())
		})
	}
}

func TestSubscribeCoreContractUpdatesRejectsFirstTransactionAtBegin(t *testing.T) {
	updates, streamErr, _ := subscribeAndCollectCoreUpdates(t, 5,
		coreTransaction(5, createdLedgerEvent(createdCoreEvent(CoreContractTemplateCoreState, 5, testCoreContractIDHex, nil))),
	)
	require.ErrorContains(t, streamErr, "does not follow offset 5")
	assert.Empty(t, updates)
}

func TestSubscribeCoreContractUpdatesRejectsNegativeBegin(t *testing.T) {
	reader := &grpcCoreContractReader{update: &fakeCoreUpdatesService{}, decoder: testCoreContractDecoder(t)}
	_, err := reader.SubscribeCoreContractUpdates(context.Background(), -1, make(chan CoreContractUpdate, 1))
	require.ErrorContains(t, err, "negative begin offset")
}

func TestSubscribeCoreContractUpdatesCancellationIsSilent(t *testing.T) {
	release := make(chan struct{})
	stream := &fakeStream{recv: func() (*apiv2.GetUpdatesResponse, error) {
		<-release
		return nil, context.Canceled
	}}
	reader := &grpcCoreContractReader{update: &fakeCoreUpdatesService{stream: stream}, decoder: testCoreContractDecoder(t)}
	ctx, cancel := context.WithCancel(context.Background())
	subscription, err := reader.SubscribeCoreContractUpdates(ctx, 0, make(chan CoreContractUpdate, 1))
	require.NoError(t, err)
	cancel()
	close(release)
	select {
	case <-subscription.Done():
		select {
		case streamErr := <-subscription.Err():
			t.Fatalf("cancellation must not fail the subscription: %v", streamErr)
		default:
		}
	case <-time.After(2 * time.Second):
		t.Fatal("subscription did not finish on cancellation")
	}
}

// A full channel blocks delivery until cancellation ends the subscription.
func TestSubscribeCoreContractUpdatesStopsOnCancellationWhileBlocked(t *testing.T) {
	service := &fakeCoreUpdatesService{stream: &scriptedCoreContractStream[apiv2.GetUpdatesResponse]{responses: []*apiv2.GetUpdatesResponse{
		coreTransaction(5,
			createdLedgerEvent(createdCoreEvent(CoreContractTemplateCoreState, 5, testCoreContractIDHex, nil)),
			archivedLedgerEvent(archivedCoreEvent(CoreContractTemplateEmitterRegistry, 5, testOtherContractIDHex)),
		),
	}}}
	reader := &grpcCoreContractReader{update: service, decoder: testCoreContractDecoder(t)}
	out := make(chan CoreContractUpdate, 1)
	subscription, err := reader.SubscribeCoreContractUpdates(context.Background(), 0, out)
	require.NoError(t, err)
	require.Eventually(t, func() bool { return len(out) == 1 }, 2*time.Second, time.Millisecond)
	subscription.Unsubscribe()
	select {
	case <-subscription.Done():
	case <-time.After(2 * time.Second):
		t.Fatal("subscription did not finish on cancellation")
	}
	select {
	case streamErr := <-subscription.Err():
		t.Fatalf("cancellation must not fail the subscription: %v", streamErr)
	default:
	}
	first := <-out
	assert.Equal(t, CoreContractUpdateKindCreated, first.Kind())
	assert.Empty(t, out)
}
