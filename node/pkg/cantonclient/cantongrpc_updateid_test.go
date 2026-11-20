package cantonclient

import (
	"context"
	"io"
	"testing"
	"time"

	"github.com/stretchr/testify/assert"
	"github.com/stretchr/testify/require"
	"go.uber.org/zap"
	"google.golang.org/grpc"

	apiv2 "github.com/certusone/wormhole/node/pkg/cantonclient/proto/gen/com/daml/ledger/api/v2"
)

const (
	pinnedPackageID         = "1111111111111111111111111111111111111111111111111111111111111111"
	retiringPinnedPackageID = "2222222222222222222222222222222222222222222222222222222222222222"
	lookalikePackageID      = "9999999999999999999999999999999999999999999999999999999999999999"
	governanceParty         = testGovernancePartyID
	otherUpdateIDHex        = "122053276773593cd4d44463b8a459f24fa089e5abd4a4b0b0103153e5898d59f072"
)

var governanceReadScope = mustReadScope(governanceParty, false)

var anyPartyReadScope = mustReadScope("", true)

func mustReadScope(readAsParty string, unsafeDevMode bool) ReadScope {
	readScope, err := ParseReadScope(readAsParty, unsafeDevMode)
	if err != nil {
		panic(err)
	}
	return readScope
}

var pinnedTmpl = TemplateID{AcceptedPackageIDs: mustParseAcceptedPackageIDs(pinnedPackageID, ""), ModuleName: "Wormhole.Core.State", EntityName: "Emitter"}

// mustParseAcceptedPackageIDs gives a non-empty retiring id the deadline testRetiringDeadline.
func mustParseAcceptedPackageIDs(currentHex string, retiringHex string) AcceptedPackageIDs {
	retiring := RetiringPackageConfig{}
	if retiringHex != "" {
		retiring = RetiringPackageConfig{PackageIDHex: retiringHex, AcceptedUntilUTC: testRetiringDeadlineText}
	}
	accepted, err := ParseAcceptedPackageIDs(CurrentPackageConfig{PackageIDHex: currentHex}, retiring, testStartupWallClock)
	if err != nil {
		panic(err)
	}
	return accepted
}

// fakeUpdateByIDService records the GetUpdateById request and returns a scripted response.
type fakeUpdateByIDService struct {
	apiv2.UpdateServiceClient
	response    *apiv2.GetUpdateResponse
	lastRequest *apiv2.GetUpdateByIdRequest
}

func (f *fakeUpdateByIDService) GetUpdateById(_ context.Context, request *apiv2.GetUpdateByIdRequest, _ ...grpc.CallOption) (*apiv2.GetUpdateResponse, error) {
	f.lastRequest = request
	return f.response, nil
}

func publishMessageEvent(packageID string, sequence int64) *apiv2.Event {
	return &apiv2.Event{Event: &apiv2.Event_Exercised{Exercised: &apiv2.ExercisedEvent{
		TemplateId:     &apiv2.Identifier{PackageId: packageID, ModuleName: "Wormhole.Core.State", EntityName: "Emitter"},
		Choice:         "PublishMessage",
		ExerciseResult: tupleResult("00deadbeef", wormholeRecord(map[string]*apiv2.Value{"sequence": int64Val(sequence)})),
		Consuming:      true,
		AcsDelta:       true,
	}}}
}

func transactionResponse(updateIDHex string, offset int64, events ...*apiv2.Event) *apiv2.GetUpdateResponse {
	return &apiv2.GetUpdateResponse{Update: &apiv2.GetUpdateResponse_Transaction{Transaction: &apiv2.Transaction{
		UpdateId: updateIDHex,
		Offset:   offset,
		Events:   events,
	}}}
}

func TestGetTransactionByUpdateIDAcceptsBothWindowPackages(t *testing.T) {
	updateID, err := ParseUpdateID(sampleUpdateIDHex)
	require.NoError(t, err)
	response := transactionResponse(sampleUpdateIDHex, 590,
		publishMessageEvent(pinnedPackageID, 5),
		publishMessageEvent(retiringPinnedPackageID, 6),
		publishMessageEvent(lookalikePackageID, 7),
	)
	withLedgerTimes(response.GetTransaction(), sampleEffectiveAt, testRetiringDeadline.Add(-time.Second))
	service := &fakeUpdateByIDService{response: response}
	client := &grpcClient{update: service, readScope: governanceReadScope, logger: zap.NewNop()}
	countBefore := unpinnedPublishCount(lookalikePackageID)

	transaction, err := client.GetTransactionByUpdateID(context.Background(), updateID, upgradeWindowTmpl, "PublishMessage")
	require.NoError(t, err)

	require.Len(t, transaction.Messages, 2, "only the look-alike package event must be filtered")
	assert.Equal(t, uint64(5), transaction.Messages[0].Sequence)
	assert.Equal(t, uint64(6), transaction.Messages[1].Sequence)
	assert.Equal(t, float64(1), unpinnedPublishCount(lookalikePackageID)-countBefore)
}

func TestGetTransactionByUpdateIDAppliesPartyFilterAndPackagePin(t *testing.T) {
	updateID, err := ParseUpdateID(sampleUpdateIDHex)
	require.NoError(t, err)
	service := &fakeUpdateByIDService{response: transactionResponse(sampleUpdateIDHex, 590,
		publishMessageEvent(pinnedPackageID, 5),
		publishMessageEvent(lookalikePackageID, 6),
	)}
	client := &grpcClient{update: service, readScope: governanceReadScope, logger: zap.NewNop()}

	transaction, err := client.GetTransactionByUpdateID(context.Background(), updateID, pinnedTmpl, "PublishMessage")
	require.NoError(t, err)

	require.NotNil(t, service.lastRequest)
	assert.Equal(t, sampleUpdateIDHex, service.lastRequest.GetUpdateId())
	eventFormat := service.lastRequest.GetUpdateFormat().GetIncludeTransactions().GetEventFormat()
	assert.Nil(t, eventFormat.GetFiltersForAnyParty(), "reobservation must not widen to filters_for_any_party")
	require.Len(t, eventFormat.GetFiltersByParty(), 1)
	assert.Contains(t, eventFormat.GetFiltersByParty(), governanceParty)

	assert.Equal(t, updateID, transaction.UpdateID)
	assert.Equal(t, int64(590), transaction.ParticipantOffset)
	require.Len(t, transaction.Messages, 1, "the look-alike package event must be filtered")
	assert.Equal(t, uint64(5), transaction.Messages[0].Sequence)
}

// Reobservation decides by the transaction's record time, not the time of the request.
func TestGetTransactionByUpdateIDAppliesRetiringDeadline(t *testing.T) {
	updateID, err := ParseUpdateID(sampleUpdateIDHex)
	require.NoError(t, err)
	cases := []struct {
		name                   string
		recordTime             time.Time
		wantSequences          []uint64
		wantAfterDeadlineCount float64
		wantMissingRecordTime  float64
	}{
		{name: "pre-deadline update reobserved after the deadline", recordTime: testRetiringDeadline.Add(-time.Second), wantSequences: []uint64{5, 6}},
		{name: "update at the deadline", recordTime: testRetiringDeadline, wantSequences: []uint64{5}, wantAfterDeadlineCount: 1},
		{name: "post-deadline update", recordTime: testRetiringDeadline.Add(time.Hour), wantSequences: []uint64{5}, wantAfterDeadlineCount: 1},
		{name: "update without record time", wantSequences: []uint64{5}, wantMissingRecordTime: 1},
	}
	for _, tc := range cases {
		t.Run(tc.name, func(t *testing.T) {
			response := transactionResponse(sampleUpdateIDHex, 590,
				publishMessageEvent(pinnedPackageID, 5),
				publishMessageEvent(retiringPinnedPackageID, 6),
			)
			withLedgerTimes(response.GetTransaction(), sampleEffectiveAt, tc.recordTime)
			client := &grpcClient{update: &fakeUpdateByIDService{response: response}, readScope: governanceReadScope, logger: zap.NewNop()}
			afterDeadlineBefore := retiringPackagePublishAfterDeadlineCount()
			missingRecordTimeBefore := publishWithoutRecordTimeCount()

			transaction, err := client.GetTransactionByUpdateID(context.Background(), updateID, upgradeWindowTmpl, "PublishMessage")
			require.NoError(t, err)

			sequences := make([]uint64, 0, len(transaction.Messages))
			for _, message := range transaction.Messages {
				sequences = append(sequences, message.Sequence)
			}
			assert.Equal(t, tc.wantSequences, sequences)
			assert.Equal(t, tc.wantAfterDeadlineCount, retiringPackagePublishAfterDeadlineCount()-afterDeadlineBefore)
			assert.Equal(t, tc.wantMissingRecordTime, publishWithoutRecordTimeCount()-missingRecordTimeBefore)
		})
	}
}

func TestGetTransactionByUpdateIDRejectsBadResponse(t *testing.T) {
	updateID, err := ParseUpdateID(sampleUpdateIDHex)
	require.NoError(t, err)
	cases := []struct {
		name     string
		response *apiv2.GetUpdateResponse
	}{
		{name: "different update id", response: transactionResponse(otherUpdateIDHex, 590, publishMessageEvent(pinnedPackageID, 5))},
		{name: "malformed update id", response: transactionResponse("not-a-hash", 590, publishMessageEvent(pinnedPackageID, 5))},
		{name: "not a transaction", response: &apiv2.GetUpdateResponse{Update: &apiv2.GetUpdateResponse_Reassignment{Reassignment: &apiv2.Reassignment{}}}},
	}
	for _, tc := range cases {
		t.Run(tc.name, func(t *testing.T) {
			client := &grpcClient{update: &fakeUpdateByIDService{response: tc.response}, readScope: anyPartyReadScope, logger: zap.NewNop()}
			_, err := client.GetTransactionByUpdateID(context.Background(), updateID, pinnedTmpl, "PublishMessage")
			assert.Error(t, err)
		})
	}
}

// scriptedStream returns the scripted responses, then blocks until release closes, then io.EOF.
func scriptedStream(release <-chan struct{}, responses ...*apiv2.GetUpdatesResponse) *fakeStream {
	next := 0
	return &fakeStream{recv: func() (*apiv2.GetUpdatesResponse, error) {
		if next < len(responses) {
			next++
			return responses[next-1], nil
		}
		<-release
		return nil, io.EOF
	}}
}

func streamedTransaction(updateIDHex string, offset int64, events ...*apiv2.Event) *apiv2.GetUpdatesResponse {
	return &apiv2.GetUpdatesResponse{Update: &apiv2.GetUpdatesResponse_Transaction{Transaction: &apiv2.Transaction{
		UpdateId: updateIDHex,
		Offset:   offset,
		Events:   events,
	}}}
}

// Participants assign different offsets to one update; the event's update id must not depend on them.
func TestSubscribeUpdatesCarriesOffsetIndependentUpdateID(t *testing.T) {
	release := make(chan struct{})
	defer close(release)
	client := newTestGrpcClient(scriptedStream(release,
		streamedTransaction(sampleUpdateIDHex, 590, publishMessageEvent(pinnedPackageID, 5)),
		streamedTransaction(sampleUpdateIDHex, 762, publishMessageEvent(pinnedPackageID, 5)),
	))
	events := make(chan CantonMessageEvent, 2)
	subscription, err := client.SubscribeUpdates(context.Background(), 0, pinnedTmpl, "PublishMessage", events)
	require.NoError(t, err)
	defer subscription.Unsubscribe()

	first := receiveMessageEvent(t, events)
	second := receiveMessageEvent(t, events)
	assert.Equal(t, int64(590), first.ParticipantOffset)
	assert.Equal(t, int64(762), second.ParticipantOffset)
	assert.Equal(t, sampleUpdateIDHex, first.UpdateID.String())
	assert.Equal(t, first.UpdateID, second.UpdateID)
}

func TestSubscribeUpdatesFailsOnMalformedUpdateID(t *testing.T) {
	release := make(chan struct{})
	defer close(release)
	client := newTestGrpcClient(scriptedStream(release,
		streamedTransaction("590", 590, publishMessageEvent(pinnedPackageID, 5)),
	))
	events := make(chan CantonMessageEvent, 1)
	subscription, err := client.SubscribeUpdates(context.Background(), 0, pinnedTmpl, "PublishMessage", events)
	require.NoError(t, err)
	defer subscription.Unsubscribe()

	select {
	case streamErr := <-subscription.Err():
		assert.Contains(t, streamErr.Error(), "update id")
	case event := <-events:
		t.Fatalf("event emitted for a malformed update id: %+v", event)
	case <-time.After(2 * time.Second):
		t.Fatal("malformed update id did not fail the subscription")
	}
}

// A transaction without Wormhole messages is skipped whatever its update id.
func TestSubscribeUpdatesIgnoresUpdateIDOfUnrelatedTransaction(t *testing.T) {
	release := make(chan struct{})
	defer close(release)
	client := newTestGrpcClient(scriptedStream(release,
		streamedTransaction("unrelated-update", 1),
		streamedTransaction(sampleUpdateIDHex, 2, publishMessageEvent(pinnedPackageID, 5)),
	))
	events := make(chan CantonMessageEvent, 1)
	subscription, err := client.SubscribeUpdates(context.Background(), 0, pinnedTmpl, "PublishMessage", events)
	require.NoError(t, err)
	defer subscription.Unsubscribe()

	event := receiveMessageEvent(t, events)
	assert.Equal(t, sampleUpdateIDHex, event.UpdateID.String())
}

func receiveMessageEvent(t *testing.T, events <-chan CantonMessageEvent) CantonMessageEvent {
	t.Helper()
	select {
	case event := <-events:
		return event
	case <-time.After(2 * time.Second):
		t.Fatal("no message event")
		return CantonMessageEvent{}
	}
}
