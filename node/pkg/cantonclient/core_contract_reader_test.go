package cantonclient

import (
	"context"
	"errors"
	"fmt"
	"io"
	"testing"
	"time"

	"github.com/stretchr/testify/assert"
	"github.com/stretchr/testify/require"
	"google.golang.org/grpc"
	"google.golang.org/grpc/codes"
	"google.golang.org/grpc/status"

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

// fakeActiveContractsService returns stream for the first request and singletonStream for later requests.
type fakeActiveContractsService struct {
	apiv2.StateServiceClient
	stream          *scriptedCoreContractStream[apiv2.GetActiveContractsResponse]
	singletonStream *scriptedCoreContractStream[apiv2.GetActiveContractsResponse]
	request         *apiv2.GetActiveContractsRequest
	requests        []*apiv2.GetActiveContractsRequest
}

func (service *fakeActiveContractsService) GetActiveContracts(_ context.Context, request *apiv2.GetActiveContractsRequest, _ ...grpc.CallOption) (apiv2.StateService_GetActiveContractsClient, error) {
	service.requests = append(service.requests, request)
	if len(service.requests) > 1 {
		if service.singletonStream == nil {
			return nil, errors.New("fake state service: unexpected second GetActiveContracts")
		}
		return service.singletonStream, nil
	}
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
	snapshot, err := reader.ReadActiveCoreContracts(context.Background(), 10, 2)
	require.NoError(t, err)
	assert.False(t, snapshot.ReplayNodesTruncated())
	contracts := snapshot.Contracts()
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
	snapshot, err := reader.ReadActiveCoreContracts(context.Background(), 10, 1)
	require.NoError(t, err)
	assert.Empty(t, snapshot.Contracts())
	assert.False(t, snapshot.ReplayNodesTruncated())
}

func TestReadActiveCoreContractsReplayNodeLimit(t *testing.T) {
	snapshotResponses := []*apiv2.GetActiveContractsResponse{
		activeContractResponse(createdCoreEvent(CoreContractTemplateReplayNode, 1, testCoreContractIDHex, nil)),
		activeContractResponse(createdCoreEvent(CoreContractTemplateReplayNode, 2, testOtherContractIDHex, nil)),
		activeContractResponse(createdCoreEvent(CoreContractTemplateCoreState, 3, testThirdContractIDHex, nil)),
	}
	cases := []struct {
		name               string
		maxReplayNodes     int
		wantContractIDs    []string
		wantReplayTruncate bool
		wantErr            string
	}{
		{name: "limit above the replay node count", maxReplayNodes: 3, wantContractIDs: []string{testCoreContractIDHex, testOtherContractIDHex, testThirdContractIDHex}},
		{name: "limit equal to the replay node count", maxReplayNodes: 2, wantContractIDs: []string{testCoreContractIDHex, testOtherContractIDHex, testThirdContractIDHex}},
		{name: "limit one below the replay node count keeps the singletons", maxReplayNodes: 1, wantContractIDs: []string{testThirdContractIDHex}, wantReplayTruncate: true},
		{name: "zero limit", maxReplayNodes: 0, wantErr: "outside [1, 8388608]"},
		{name: "negative limit", maxReplayNodes: -1, wantErr: "outside [1, 8388608]"},
		{name: "limit above the upper bound", maxReplayNodes: MaxActiveCoreContractsPerSnapshot + 1, wantErr: "outside [1, 8388608]"},
	}
	for _, tc := range cases {
		t.Run(tc.name, func(t *testing.T) {
			reader, service := testCoreContractReaderWithSnapshot(t, snapshotResponses...)
			service.singletonStream = &scriptedCoreContractStream[apiv2.GetActiveContractsResponse]{responses: snapshotResponses[2:]}
			snapshot, err := reader.ReadActiveCoreContracts(context.Background(), 3, tc.maxReplayNodes)
			if tc.wantErr != "" {
				require.ErrorContains(t, err, tc.wantErr)
				assert.Equal(t, ActiveCoreContracts{}, snapshot)
				return
			}
			require.NoError(t, err)
			assert.Equal(t, tc.wantReplayTruncate, snapshot.ReplayNodesTruncated())
			contractIDs := []string{}
			for _, contract := range snapshot.Contracts() {
				contractIDs = append(contractIDs, contract.ContractID().String())
			}
			assert.Equal(t, tc.wantContractIDs, contractIDs)
		})
	}
}

func snapshotContractIDHex(serial int) string {
	return fmt.Sprintf("00%064x", serial)
}

func decodedSnapshotContract(t *testing.T, template CoreContractTemplate, serial int) CreatedCoreContract {
	t.Helper()
	contract, err := testCoreContractDecoder(t).decodeCreatedCoreContract(createdCoreEvent(template, 1, snapshotContractIDHex(serial), nil), testSynchronizerID)
	require.NoError(t, err)
	return contract
}

// checkFiltersTemplates requires the guardianGovernance party filter with blobs for exactly wantTemplates.
func checkFiltersTemplates(t *testing.T, eventFormat *apiv2.EventFormat, wantTemplates []string) {
	t.Helper()
	filters, found := eventFormat.GetFiltersByParty()[governanceParty]
	require.True(t, found)
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

func TestReadActiveCoreContractsStopsAtTheReplayNodeLimitAndReadsTheSingletonsAlone(t *testing.T) {
	const maxReplayNodes = 2
	fullResponses := []*apiv2.GetActiveContractsResponse{
		activeContractResponse(createdCoreEvent(CoreContractTemplateCoreState, 1, snapshotContractIDHex(100), nil)),
	}
	for serial := 1; serial <= 50; serial++ {
		fullResponses = append(fullResponses, activeContractResponse(createdCoreEvent(CoreContractTemplateReplayNode, 1, snapshotContractIDHex(serial), nil)))
	}
	singletonResponses := []*apiv2.GetActiveContractsResponse{
		activeContractResponse(createdCoreEvent(CoreContractTemplateCoreState, 1, snapshotContractIDHex(100), nil)),
		activeContractResponse(createdCoreEvent(CoreContractTemplateEmitterRegistry, 1, snapshotContractIDHex(101), nil)),
		activeContractResponse(createdCoreEvent(CoreContractTemplateReplayRootRegistry, 1, snapshotContractIDHex(102), nil)),
	}
	reader, service := testCoreContractReaderWithSnapshot(t, fullResponses...)
	service.singletonStream = &scriptedCoreContractStream[apiv2.GetActiveContractsResponse]{responses: singletonResponses}

	snapshot, err := reader.ReadActiveCoreContracts(context.Background(), 7, maxReplayNodes)
	require.NoError(t, err)
	require.True(t, snapshot.ReplayNodesTruncated())
	gotTemplates := []CoreContractTemplate{}
	for _, contract := range snapshot.Contracts() {
		gotTemplates = append(gotTemplates, contract.TemplateID().Template())
	}
	assert.Equal(t, []CoreContractTemplate{CoreContractTemplateCoreState, CoreContractTemplateEmitterRegistry, CoreContractTemplateReplayRootRegistry}, gotTemplates)

	receivedFullEntryCount := len(fullResponses) - len(service.stream.responses)
	assert.Equal(t, 1+maxReplayNodes+1, receivedFullEntryCount, "the full read stops at the first replay node above the limit")
	assert.Empty(t, service.singletonStream.responses)
	require.Len(t, service.requests, 2)
	for _, request := range service.requests {
		assert.Equal(t, int64(7), request.GetActiveAtOffset())
	}
	checkFiltersTheDisclosedCoreTemplates(t, service.requests[0].GetEventFormat())
	checkFiltersTemplates(t, service.requests[1].GetEventFormat(), []string{
		"#wormhole-core:Wormhole.Core.State:CoreState",
		"#wormhole-core:Wormhole.Core.State:EmitterRegistry",
		"#wormhole-core:Wormhole.Core.State:ReplayRootRegistry",
	})
}

func TestReadActiveCoreContractsSingletonReadRejects(t *testing.T) {
	overLimit := []*apiv2.GetActiveContractsResponse{
		activeContractResponse(createdCoreEvent(CoreContractTemplateReplayNode, 1, snapshotContractIDHex(1), nil)),
		activeContractResponse(createdCoreEvent(CoreContractTemplateReplayNode, 1, snapshotContractIDHex(2), nil)),
	}
	fourCoreStates := []*apiv2.GetActiveContractsResponse{}
	for serial := 10; serial < 14; serial++ {
		fourCoreStates = append(fourCoreStates, activeContractResponse(createdCoreEvent(CoreContractTemplateCoreState, 1, snapshotContractIDHex(serial), nil)))
	}
	cases := []struct {
		name               string
		singletonResponses []*apiv2.GetActiveContractsResponse
		singletonFinalErr  error
		wantErr            string
	}{
		{name: "replay node in the singleton read", singletonResponses: overLimit[:1], wantErr: "singleton read returned replay node"},
		{name: "four instances of one singleton", singletonResponses: fourCoreStates, wantErr: "more than 3 active"},
		{name: "stream error", singletonFinalErr: errors.New("connection reset"), wantErr: "singleton read: GetActiveContracts(5): connection reset"},
	}
	for _, tc := range cases {
		t.Run(tc.name, func(t *testing.T) {
			reader, service := testCoreContractReaderWithSnapshot(t, overLimit...)
			service.singletonStream = &scriptedCoreContractStream[apiv2.GetActiveContractsResponse]{responses: tc.singletonResponses, finalErr: tc.singletonFinalErr}
			snapshot, err := reader.ReadActiveCoreContracts(context.Background(), 5, 1)
			require.ErrorContains(t, err, tc.wantErr)
			assert.Equal(t, ActiveCoreContracts{}, snapshot)
		})
	}
}

func TestActiveCoreContractCollectorEntryBound(t *testing.T) {
	collector, err := newActiveCoreContractCollector(4)
	require.NoError(t, err)
	require.Equal(t, 4+1+9, collector.maxReceivedEntryCount())
	for entryIndex := 0; entryIndex < collector.maxReceivedEntryCount(); entryIndex++ {
		require.NoError(t, collector.countReceivedEntry())
	}
	require.ErrorContains(t, collector.countReceivedEntry(), "more than 14 entries")
	require.Equal(t, 0+1+9, newSingletonContractCollector().maxReceivedEntryCount())
	largest, err := newActiveCoreContractCollector(MaxActiveCoreContractsPerSnapshot)
	require.NoError(t, err)
	require.Equal(t, MaxActiveCoreContractsPerSnapshot+10, largest.maxReceivedEntryCount())
}

func TestCollectActiveCoreContracts(t *testing.T) {
	cases := []struct {
		name               string
		templates          []CoreContractTemplate
		maxReplayNodes     int
		wantTemplates      []CoreContractTemplate
		wantReplayTruncate bool
		wantErr            string
	}{
		{
			name:           "singletons after the truncation are kept",
			templates:      []CoreContractTemplate{CoreContractTemplateReplayNode, CoreContractTemplateReplayNode, CoreContractTemplateReplayNode, CoreContractTemplateEmitterRegistry, CoreContractTemplateReplayNode, CoreContractTemplateCoreState},
			maxReplayNodes: 2, wantTemplates: []CoreContractTemplate{CoreContractTemplateEmitterRegistry, CoreContractTemplateCoreState}, wantReplayTruncate: true,
		},
		{
			name:           "three instances of one singleton",
			templates:      []CoreContractTemplate{CoreContractTemplateCoreState, CoreContractTemplateCoreState, CoreContractTemplateCoreState},
			maxReplayNodes: 1, wantTemplates: []CoreContractTemplate{CoreContractTemplateCoreState, CoreContractTemplateCoreState, CoreContractTemplateCoreState},
		},
		{
			name:           "four instances of one singleton",
			templates:      []CoreContractTemplate{CoreContractTemplateCoreState, CoreContractTemplateCoreState, CoreContractTemplateCoreState, CoreContractTemplateCoreState},
			maxReplayNodes: 1, wantErr: "more than 3 active Wormhole.Core.State:CoreState",
		},
		{
			name:           "four singleton instances after the truncation",
			templates:      []CoreContractTemplate{CoreContractTemplateReplayNode, CoreContractTemplateReplayNode, CoreContractTemplateReplayRootRegistry, CoreContractTemplateReplayRootRegistry, CoreContractTemplateReplayRootRegistry, CoreContractTemplateReplayRootRegistry},
			maxReplayNodes: 1, wantErr: "more than 3 active Wormhole.Core.State:ReplayRootRegistry",
		},
	}
	for _, tc := range cases {
		t.Run(tc.name, func(t *testing.T) {
			contracts := make([]CreatedCoreContract, 0, len(tc.templates))
			for serial, template := range tc.templates {
				contracts = append(contracts, decodedSnapshotContract(t, template, serial+1))
			}
			snapshot, err := CollectActiveCoreContractsForTest(contracts, tc.maxReplayNodes)
			if tc.wantErr != "" {
				require.ErrorContains(t, err, tc.wantErr)
				assert.Equal(t, ActiveCoreContracts{}, snapshot)
				return
			}
			require.NoError(t, err)
			assert.Equal(t, tc.wantReplayTruncate, snapshot.ReplayNodesTruncated())
			gotTemplates := []CoreContractTemplate{}
			for _, contract := range snapshot.Contracts() {
				gotTemplates = append(gotTemplates, contract.TemplateID().Template())
			}
			assert.Equal(t, tc.wantTemplates, gotTemplates)
		})
	}
}

func TestCollectActiveCoreContractsRejectsARepeatedKeptContractID(t *testing.T) {
	coreState := decodedSnapshotContract(t, CoreContractTemplateCoreState, 1)
	_, err := CollectActiveCoreContractsForTest([]CreatedCoreContract{coreState, coreState}, 1)
	require.ErrorContains(t, err, "repeats contract id")
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
			snapshot, err := reader.ReadActiveCoreContracts(context.Background(), tc.activeAtOffset, 10)
			require.ErrorContains(t, err, tc.wantErr)
			assert.Equal(t, ActiveCoreContracts{}, snapshot)
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

// subscribeAndCollectCoreUpdates runs the stream to its end and returns the delivered advances and the terminal error.
func subscribeAndCollectCoreUpdates(t *testing.T, beginExclusive int64, responses ...*apiv2.GetUpdatesResponse) ([]CoreContractLedgerAdvance, error, *apiv2.GetUpdatesRequest) {
	t.Helper()
	service := &fakeCoreUpdatesService{stream: &scriptedCoreContractStream[apiv2.GetUpdatesResponse]{responses: responses}}
	reader := &grpcCoreContractReader{update: service, decoder: testCoreContractDecoder(t)}
	out := make(chan CoreContractLedgerAdvance, 4)
	subscription, err := reader.SubscribeCoreContractUpdates(context.Background(), beginExclusive, out)
	require.NoError(t, err)
	defer subscription.Unsubscribe()
	updates := []CoreContractLedgerAdvance{}
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

func TestSubscribeCoreContractUpdatesDeliversOneAdvancePerTransaction(t *testing.T) {
	advances, streamErr, request := subscribeAndCollectCoreUpdates(t, 4,
		coreTransaction(5,
			createdLedgerEvent(createdCoreEvent(CoreContractTemplateReplayNode, 5, testCoreContractIDHex, nil)),
		),
		offsetCheckpointResponse(6),
		coreTransaction(8,
			archivedLedgerEvent(archivedCoreEvent(CoreContractTemplateReplayNode, 8, testCoreContractIDHex)),
			createdLedgerEvent(createdCoreEvent(CoreContractTemplateReplayNode, 8, testOtherContractIDHex, map[string]*apiv2.Value{"prefix": digestNibblePrefixVal(pinnedPackageID, "a70")})),
		),
		offsetCheckpointResponse(8),
		offsetCheckpointResponse(11),
	)
	require.ErrorContains(t, streamErr, "GetUpdates stream ended")
	require.ErrorIs(t, streamErr, io.EOF)
	require.NotErrorIs(t, streamErr, ErrCoreContractStreamDecode)

	assert.Equal(t, int64(4), request.GetBeginExclusive())
	assert.Nil(t, request.EndInclusive)
	transactionFormat := request.GetUpdateFormat().GetIncludeTransactions()
	assert.Equal(t, apiv2.TransactionShape_TRANSACTION_SHAPE_ACS_DELTA, transactionFormat.GetTransactionShape())
	checkFiltersTheDisclosedCoreTemplates(t, transactionFormat.GetEventFormat())
	checkFiltersTheDisclosedCoreTemplates(t, request.GetUpdateFormat().GetIncludeReassignments())

	require.Len(t, advances, 4)
	assert.Equal(t, []int64{5, 6, 8, 11}, []int64{advances[0].Offset(), advances[1].Offset(), advances[2].Offset(), advances[3].Offset()})
	assert.Equal(t, []bool{false, true, false, true}, []bool{advances[0].IsOffsetCheckpoint(), advances[1].IsOffsetCheckpoint(), advances[2].IsOffsetCheckpoint(), advances[3].IsOffsetCheckpoint()})
	assert.Empty(t, advances[1].Updates())
	assert.Empty(t, advances[3].Updates())

	firstUpdates := advances[0].Updates()
	require.Len(t, firstUpdates, 1)
	created, isCreated := firstUpdates[0].Created()
	require.True(t, isCreated)
	assert.Equal(t, int64(5), firstUpdates[0].Offset())
	assert.Equal(t, CoreContractUpdateKindCreated, firstUpdates[0].Kind())
	assert.Equal(t, testCoreContractIDHex, created.ContractID().String())
	_, isArchived := firstUpdates[0].Archived()
	assert.False(t, isArchived)

	splitUpdates := advances[2].Updates()
	require.Len(t, splitUpdates, 2)
	assert.Equal(t, []int64{8, 8}, []int64{splitUpdates[0].Offset(), splitUpdates[1].Offset()})
	archived, isArchived := splitUpdates[0].Archived()
	require.True(t, isArchived)
	assert.Equal(t, CoreContractUpdateKindArchived, splitUpdates[0].Kind())
	assert.Equal(t, testCoreContractIDHex, archived.ContractID().String())
	assert.Equal(t, CoreContractTemplateReplayNode, archived.TemplateID().Template())
	_, isCreated = splitUpdates[0].Created()
	assert.False(t, isCreated)

	created, isCreated = splitUpdates[1].Created()
	require.True(t, isCreated)
	replayNodeKey, isReplayNode := created.ReplayNodeKey()
	require.True(t, isReplayNode)
	assert.Equal(t, "a70", replayNodeKey.Prefix().String())
	assert.Equal(t, testSynchronizerID, created.SynchronizerID().String())
}

func TestCoreContractLedgerAdvanceUpdatesIsACopy(t *testing.T) {
	advances, _, _ := subscribeAndCollectCoreUpdates(t, 4,
		coreTransaction(5, createdLedgerEvent(createdCoreEvent(CoreContractTemplateCoreState, 5, testCoreContractIDHex, nil))),
	)
	require.Len(t, advances, 1)
	updates := advances[0].Updates()
	updates[0] = CoreContractUpdate{}
	require.Equal(t, CoreContractUpdateKindCreated, advances[0].Updates()[0].Kind())
}

func TestSubscribeCoreContractUpdatesOffsetCheckpointOrder(t *testing.T) {
	cases := []struct {
		name             string
		beginExclusive   int64
		responses        []*apiv2.GetUpdatesResponse
		expectedOffsets  []int64
		expectedErrorMsg string
	}{
		{name: "checkpoint after the begin offset", beginExclusive: 4, responses: []*apiv2.GetUpdatesResponse{offsetCheckpointResponse(5)}, expectedOffsets: []int64{5}},
		{name: "checkpoint at the begin offset is skipped", beginExclusive: 4, responses: []*apiv2.GetUpdatesResponse{offsetCheckpointResponse(4), offsetCheckpointResponse(7)}, expectedOffsets: []int64{7}},
		{name: "repeated checkpoint is skipped", beginExclusive: 4, responses: []*apiv2.GetUpdatesResponse{offsetCheckpointResponse(7), offsetCheckpointResponse(7)}, expectedOffsets: []int64{7}},
		{name: "checkpoint below the begin offset", beginExclusive: 4, responses: []*apiv2.GetUpdatesResponse{offsetCheckpointResponse(3)}, expectedErrorMsg: "offset checkpoint 3 is below offset 4"},
		{name: "checkpoint below the previous transaction", beginExclusive: 4, responses: []*apiv2.GetUpdatesResponse{
			coreTransaction(9, createdLedgerEvent(createdCoreEvent(CoreContractTemplateCoreState, 9, testCoreContractIDHex, nil))),
			offsetCheckpointResponse(8),
		}, expectedOffsets: []int64{9}, expectedErrorMsg: "offset checkpoint 8 is below offset 9"},
		{name: "transaction at the previous checkpoint", beginExclusive: 4, responses: []*apiv2.GetUpdatesResponse{
			offsetCheckpointResponse(9),
			coreTransaction(9, createdLedgerEvent(createdCoreEvent(CoreContractTemplateCoreState, 9, testCoreContractIDHex, nil))),
		}, expectedOffsets: []int64{9}, expectedErrorMsg: "does not follow offset 9"},
	}
	for _, tc := range cases {
		t.Run(tc.name, func(t *testing.T) {
			advances, streamErr, _ := subscribeAndCollectCoreUpdates(t, tc.beginExclusive, tc.responses...)
			offsets := []int64{}
			for _, advance := range advances {
				offsets = append(offsets, advance.Offset())
			}
			if len(tc.expectedOffsets) == 0 {
				assert.Empty(t, offsets)
			} else {
				assert.Equal(t, tc.expectedOffsets, offsets)
			}
			if tc.expectedErrorMsg == "" {
				require.ErrorIs(t, streamErr, io.EOF)
				return
			}
			require.ErrorContains(t, streamErr, tc.expectedErrorMsg)
		})
	}
}

func TestIsPrunedOffsetError(t *testing.T) {
	cases := []struct {
		name     string
		err      error
		isPruned bool
	}{
		{name: "pruned data accessed", err: status.Error(codes.FailedPrecondition, "PARTICIPANT_PRUNED_DATA_ACCESSED(9,0): Command completions request from 3 is beyond pruned offset 10"), isPruned: true},
		{name: "wrapped pruned data accessed", err: fmt.Errorf("GetUpdates stream ended: %w", status.Error(codes.FailedPrecondition, "PARTICIPANT_PRUNED_DATA_ACCESSED(9,0): pruned")), isPruned: true},
		{name: "other failed precondition", err: status.Error(codes.FailedPrecondition, "OFFSET_OUT_OF_RANGE"), isPruned: false},
		{name: "pruned id under another code", err: status.Error(codes.Unavailable, "PARTICIPANT_PRUNED_DATA_ACCESSED"), isPruned: false},
		{name: "unavailable", err: status.Error(codes.Unavailable, "connection refused"), isPruned: false},
		{name: "plain error", err: errors.New("PARTICIPANT_PRUNED_DATA_ACCESSED"), isPruned: false},
		{name: "nil", err: nil, isPruned: false},
	}
	for _, tc := range cases {
		t.Run(tc.name, func(t *testing.T) {
			assert.Equal(t, tc.isPruned, IsPrunedOffsetError(tc.err))
		})
	}
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
			advances, streamErr, _ := subscribeAndCollectCoreUpdates(t, 4, validFirst, tc.second)
			require.ErrorContains(t, streamErr, tc.wantErr)
			require.ErrorIs(t, streamErr, ErrCoreContractStreamDecode)
			require.Len(t, advances, 1)
			assert.Equal(t, int64(5), advances[0].Offset())
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
	_, err := reader.SubscribeCoreContractUpdates(context.Background(), -1, make(chan CoreContractLedgerAdvance, 1))
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
	subscription, err := reader.SubscribeCoreContractUpdates(ctx, 0, make(chan CoreContractLedgerAdvance, 1))
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
		coreTransaction(6, createdLedgerEvent(createdCoreEvent(CoreContractTemplateCoreState, 6, testThirdContractIDHex, nil))),
	}}}
	reader := &grpcCoreContractReader{update: service, decoder: testCoreContractDecoder(t)}
	out := make(chan CoreContractLedgerAdvance, 1)
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
	assert.Equal(t, int64(5), first.Offset())
	assert.Len(t, first.Updates(), 2)
	assert.Empty(t, out)
}

func TestSubscribeCoreContractUpdatesBoundsTheUpdatesOfOneTransaction(t *testing.T) {
	cases := []struct {
		name          string
		eventCount    int
		expectRefused bool
	}{
		{name: "maximum", eventCount: maxCoreContractUpdatesPerTransaction},
		{name: "maximum plus one", eventCount: maxCoreContractUpdatesPerTransaction + 1, expectRefused: true},
	}
	require.Equal(t, 65536, maxCoreContractUpdatesPerTransaction)
	for _, tc := range cases {
		t.Run(tc.name, func(t *testing.T) {
			events := make([]*apiv2.Event, 0, tc.eventCount)
			for eventIndex := 0; eventIndex < tc.eventCount; eventIndex++ {
				events = append(events, archivedLedgerEvent(archivedCoreEvent(CoreContractTemplateReplayNode, 5, testCoreContractIDHex)))
			}
			advance, advances, err := testCoreContractDecoder(t).decodeUpdatesResponse(coreTransaction(5, events...), 4)
			if tc.expectRefused {
				require.ErrorContains(t, err, "65537 core contract updates")
				require.False(t, advances)
				return
			}
			require.NoError(t, err)
			require.True(t, advances)
			require.Len(t, advance.Updates(), tc.eventCount)
		})
	}
}
