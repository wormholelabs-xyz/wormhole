package cantonclient

import (
	"context"
	"testing"
	"time"

	"github.com/stretchr/testify/assert"
	"github.com/stretchr/testify/require"
	"go.uber.org/zap"
	"google.golang.org/protobuf/types/known/timestamppb"

	apiv2 "github.com/certusone/wormhole/node/pkg/cantonclient/proto/gen/com/daml/ledger/api/v2"
)

var (
	sampleEffectiveAt = time.Date(2026, 11, 17, 10, 0, 0, 0, time.UTC)
	sampleRecordTime  = time.Date(2026, 11, 17, 10, 0, 1, 500, time.UTC)
)

func withLedgerTimes(transaction *apiv2.Transaction, effectiveAt time.Time, recordTime time.Time) *apiv2.Transaction {
	if !effectiveAt.IsZero() {
		transaction.EffectiveAt = timestamppb.New(effectiveAt)
	}
	if !recordTime.IsZero() {
		transaction.RecordTime = timestamppb.New(recordTime)
	}
	return transaction
}

func TestMessageEventsCarryRecordTime(t *testing.T) {
	cases := []struct {
		name            string
		recordTime      time.Time
		wantRecordTime  time.Time
		wantEffectiveAt time.Time
	}{
		{name: "record time set", recordTime: sampleRecordTime, wantRecordTime: sampleRecordTime, wantEffectiveAt: sampleEffectiveAt},
		{name: "record time missing", wantRecordTime: time.Time{}, wantEffectiveAt: sampleEffectiveAt},
	}
	for _, tc := range cases {
		t.Run(tc.name+" on the stream", func(t *testing.T) {
			release := make(chan struct{})
			defer close(release)
			streamed := streamedTransaction(sampleUpdateIDHex, 590, publishMessageEvent(pinnedPackageID, 5))
			withLedgerTimes(streamed.GetTransaction(), sampleEffectiveAt, tc.recordTime)
			client := newTestGrpcClient(scriptedStream(release, streamed))
			events := make(chan CantonMessageEvent, 1)
			subscription, err := client.SubscribeUpdates(context.Background(), 0, testTmpl, "PublishMessage", events)
			require.NoError(t, err)
			defer subscription.Unsubscribe()

			event := receiveMessageEvent(t, events)
			assert.True(t, tc.wantRecordTime.Equal(event.RecordTime), "record time %s, want %s", event.RecordTime, tc.wantRecordTime)
			assert.True(t, tc.wantEffectiveAt.Equal(event.EffectiveAt))
		})
		t.Run(tc.name+" on reobservation", func(t *testing.T) {
			updateID, err := ParseUpdateID(sampleUpdateIDHex)
			require.NoError(t, err)
			response := transactionResponse(sampleUpdateIDHex, 590, publishMessageEvent(pinnedPackageID, 5))
			withLedgerTimes(response.GetTransaction(), sampleEffectiveAt, tc.recordTime)
			client := &grpcClient{update: &fakeUpdateByIDService{response: response}, readScope: governanceReadScope, logger: zap.NewNop()}

			transaction, err := client.GetTransactionByUpdateID(context.Background(), updateID, testTmpl, "PublishMessage")
			require.NoError(t, err)

			require.Len(t, transaction.Messages, 1)
			assert.True(t, tc.wantRecordTime.Equal(transaction.RecordTime), "record time %s, want %s", transaction.RecordTime, tc.wantRecordTime)
			assert.True(t, tc.wantEffectiveAt.Equal(transaction.EffectiveAt))
		})
	}
}

// An unset, epoch-zero, or out-of-range timestamp is a missing ledger time.
func TestProtoTimeTreatsInvalidTimestampsAsMissing(t *testing.T) {
	cases := []struct {
		name      string
		timestamp *timestamppb.Timestamp
		want      time.Time
	}{
		{name: "nil", timestamp: nil},
		{name: "epoch zero", timestamp: &timestamppb.Timestamp{}},
		{name: "negative nanos", timestamp: &timestamppb.Timestamp{Seconds: sampleRecordTime.Unix(), Nanos: -1}},
		{name: "nanos of a full second", timestamp: &timestamppb.Timestamp{Seconds: sampleRecordTime.Unix(), Nanos: 1_000_000_000}},
		{name: "seconds after year 9999", timestamp: &timestamppb.Timestamp{Seconds: 253402300800}},
		{name: "valid", timestamp: timestamppb.New(sampleRecordTime), want: sampleRecordTime},
		{name: "one nanosecond after epoch", timestamp: &timestamppb.Timestamp{Nanos: 1}, want: time.Unix(0, 1).UTC()},
	}
	for _, tc := range cases {
		t.Run(tc.name, func(t *testing.T) {
			got := protoTime(tc.timestamp)
			assert.True(t, tc.want.Equal(got), "want %v, got %v", tc.want, got)
			assert.Equal(t, tc.want.IsZero(), got.IsZero())
		})
	}
}
