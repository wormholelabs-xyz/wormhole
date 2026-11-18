package cantonclient

import (
	"context"
	"errors"
	"io"
	"testing"
	"time"

	"github.com/stretchr/testify/assert"
	"github.com/stretchr/testify/require"
	"go.uber.org/zap"
	"google.golang.org/grpc"

	apiv2 "github.com/certusone/wormhole/node/pkg/cantonclient/proto/gen/com/daml/ledger/api/v2"
)

// fakeRangeUpdateService records the GetUpdates request and serves a finite stream.
type fakeRangeUpdateService struct {
	apiv2.UpdateServiceClient
	responses   []*apiv2.GetUpdatesResponse
	streamErr   error
	lastRequest *apiv2.GetUpdatesRequest
}

func (f *fakeRangeUpdateService) GetUpdates(_ context.Context, request *apiv2.GetUpdatesRequest, _ ...grpc.CallOption) (apiv2.UpdateService_GetUpdatesClient, error) {
	f.lastRequest = request
	next := 0
	return &fakeStream{recv: func() (*apiv2.GetUpdatesResponse, error) {
		if next < len(f.responses) {
			next++
			return f.responses[next-1], nil
		}
		if f.streamErr != nil {
			return nil, f.streamErr
		}
		return nil, io.EOF
	}}, nil
}

func TestReadPublishesInRange(t *testing.T) {
	recordTime := testRetiringDeadline.Add(-time.Second)
	twoPublishes := []*apiv2.GetUpdatesResponse{
		streamedTransaction(sampleUpdateIDHex, 11, publishMessageEvent(pinnedPackageID, 5), publishMessageEvent(lookalikePackageID, 6)),
		streamedTransaction(otherUpdateIDHex, 12, publishMessageEvent(pinnedPackageID, 7)),
	}
	for _, response := range twoPublishes {
		withLedgerTimes(response.GetTransaction(), sampleEffectiveAt, recordTime)
	}
	tooManyPublishes := make([]*apiv2.GetUpdatesResponse, 0, maxPublishesPerRange+1)
	for offset := int64(1); offset <= maxPublishesPerRange+1; offset++ {
		tooManyPublishes = append(tooManyPublishes, streamedTransaction(sampleUpdateIDHex, offset, publishMessageEvent(pinnedPackageID, offset)))
	}
	cases := []struct {
		name           string
		beginExclusive int64
		endInclusive   int64
		service        *fakeRangeUpdateService
		wantSequences  []uint64
		wantRequest    bool
		wantErr        string
	}{
		{name: "two transactions", beginExclusive: 10, endInclusive: 12, service: &fakeRangeUpdateService{responses: twoPublishes}, wantSequences: []uint64{5, 7}, wantRequest: true},
		{name: "empty range", beginExclusive: 12, endInclusive: 12, service: &fakeRangeUpdateService{}, wantSequences: []uint64{}},
		{name: "begin after end", beginExclusive: 13, endInclusive: 12, service: &fakeRangeUpdateService{}, wantErr: "after"},
		{name: "negative begin", beginExclusive: -1, endInclusive: 12, service: &fakeRangeUpdateService{}, wantErr: "negative"},
		{name: "stream error", beginExclusive: 10, endInclusive: 12, service: &fakeRangeUpdateService{responses: twoPublishes, streamErr: errors.New("reset")}, wantRequest: true, wantErr: "reset"},
		{name: "publish cap exceeded", beginExclusive: 0, endInclusive: maxPublishesPerRange + 1, service: &fakeRangeUpdateService{responses: tooManyPublishes}, wantRequest: true, wantErr: "more than"},
		{name: "offset beyond the requested end", beginExclusive: 10, endInclusive: 11, service: &fakeRangeUpdateService{responses: twoPublishes}, wantRequest: true, wantErr: "beyond"},
	}
	for _, tc := range cases {
		t.Run(tc.name, func(t *testing.T) {
			client := &grpcClient{update: tc.service, readAsParty: governanceParty, logger: zap.NewNop()}

			events, err := client.ReadPublishesInRange(context.Background(), tc.beginExclusive, tc.endInclusive, pinnedTmpl, "PublishMessage")

			if tc.wantRequest {
				require.NotNil(t, tc.service.lastRequest)
				assert.Equal(t, tc.beginExclusive, tc.service.lastRequest.GetBeginExclusive())
				assert.Equal(t, tc.endInclusive, tc.service.lastRequest.GetEndInclusive())
				assert.Contains(t, tc.service.lastRequest.GetUpdateFormat().GetIncludeTransactions().GetEventFormat().GetFiltersByParty(), governanceParty)
			} else {
				assert.Nil(t, tc.service.lastRequest)
			}
			if tc.wantErr != "" {
				require.ErrorContains(t, err, tc.wantErr)
				assert.Nil(t, events)
				return
			}
			require.NoError(t, err)
			sequences := make([]uint64, 0, len(events))
			for _, event := range events {
				sequences = append(sequences, event.Message.Sequence)
				assert.True(t, recordTime.Equal(event.RecordTime))
			}
			assert.Equal(t, tc.wantSequences, sequences)
		})
	}
}
