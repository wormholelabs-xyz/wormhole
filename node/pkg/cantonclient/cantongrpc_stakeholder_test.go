package cantonclient

import (
	"context"
	"testing"

	"github.com/prometheus/client_golang/prometheus"
	"github.com/stretchr/testify/assert"
	"github.com/stretchr/testify/require"
	"go.uber.org/zap"
	"go.uber.org/zap/zapcore"
	"go.uber.org/zap/zaptest/observer"

	apiv2 "github.com/certusone/wormhole/node/pkg/cantonclient/proto/gen/com/daml/ledger/api/v2"
)

const publishWithoutReadPartyStakeholderMetric = "wormhole_canton_publish_without_read_party_stakeholder_total"

func publishMessageEventWithStakeholderFlags(consuming bool, acsDelta bool, sequence int64) *apiv2.Event {
	event := publishMessageEvent(pinnedPackageID, sequence)
	event.GetExercised().Consuming = consuming
	event.GetExercised().AcsDelta = acsDelta
	return event
}

// publishWithoutReadPartyStakeholderCount reads the counter from the default registry; 0 before its registration.
func publishWithoutReadPartyStakeholderCount(t *testing.T) float64 {
	t.Helper()
	metricFamilies, err := prometheus.DefaultGatherer.Gather()
	require.NoError(t, err)
	for _, metricFamily := range metricFamilies {
		if metricFamily.GetName() != publishWithoutReadPartyStakeholderMetric {
			continue
		}
		require.Len(t, metricFamily.GetMetric(), 1)
		return metricFamily.GetMetric()[0].GetCounter().GetValue()
	}
	return 0
}

type readPartyStakeholderCase struct {
	name        string
	consuming   bool
	acsDelta    bool
	wantEmitted bool
}

var readPartyStakeholderCases = []readPartyStakeholderCase{
	{name: "consuming publish with the read party a stakeholder is emitted", consuming: true, acsDelta: true, wantEmitted: true},
	{name: "consuming publish seen only as a witness is dropped", consuming: true, acsDelta: false},
	{name: "non-consuming exercise is dropped", consuming: false, acsDelta: false},
	{name: "non-consuming exercise marked as an ACS delta is dropped", consuming: false, acsDelta: true},
}

func checkReadPartyStakeholderOutcome(t *testing.T, tc readPartyStakeholderCase, messages []CantonMessage, countIncrement float64, logs *observer.ObservedLogs) {
	t.Helper()
	if tc.wantEmitted {
		assert.Len(t, messages, 1)
		assert.Zero(t, countIncrement)
		assert.Zero(t, logs.Len())
		return
	}
	assert.Empty(t, messages)
	assert.Equal(t, float64(1), countIncrement)
	require.Equal(t, 1, logs.Len())
	assert.Equal(t, sampleUpdateIDHex, logs.All()[0].ContextMap()["updateId"])
}

func TestMessagesFromTxRequiresReadPartyStakeholder(t *testing.T) {
	for _, tc := range readPartyStakeholderCases {
		t.Run(tc.name, func(t *testing.T) {
			core, logs := observer.New(zapcore.WarnLevel)
			countBefore := publishWithoutReadPartyStakeholderCount(t)
			tx := &apiv2.Transaction{UpdateId: sampleUpdateIDHex, Events: []*apiv2.Event{
				publishMessageEventWithStakeholderFlags(tc.consuming, tc.acsDelta, 5),
			}}

			messages := messagesFromTx(tx, pinnedTmpl, "PublishMessage", zap.New(core))

			checkReadPartyStakeholderOutcome(t, tc, messages, publishWithoutReadPartyStakeholderCount(t)-countBefore, logs)
		})
	}
}

func TestGetTransactionByUpdateIDRequiresReadPartyStakeholder(t *testing.T) {
	updateID, err := ParseUpdateID(sampleUpdateIDHex)
	require.NoError(t, err)
	for _, tc := range readPartyStakeholderCases {
		t.Run(tc.name, func(t *testing.T) {
			core, logs := observer.New(zapcore.WarnLevel)
			countBefore := publishWithoutReadPartyStakeholderCount(t)
			service := &fakeUpdateByIDService{response: transactionResponse(sampleUpdateIDHex, 590,
				publishMessageEventWithStakeholderFlags(tc.consuming, tc.acsDelta, 5))}
			client := &grpcClient{update: service, readAsParty: governanceParty, logger: zap.New(core)}

			transaction, err := client.GetTransactionByUpdateID(context.Background(), updateID, pinnedTmpl, "PublishMessage")
			require.NoError(t, err)

			checkReadPartyStakeholderOutcome(t, tc, transaction.Messages, publishWithoutReadPartyStakeholderCount(t)-countBefore, logs)
		})
	}
}
