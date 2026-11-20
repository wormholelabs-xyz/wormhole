package cantonclient

import (
	"context"
	"testing"
	"time"

	"github.com/stretchr/testify/assert"
	"github.com/stretchr/testify/require"
	"go.uber.org/zap"

	apiv2 "github.com/certusone/wormhole/node/pkg/cantonclient/proto/gen/com/daml/ledger/api/v2"
)

const (
	rogueGovernanceParty = "rogueGovernance::1220ffff"
	operatorParty        = "operator::1220aaaa"
	ownerParty           = "owner::1220bbbb"
)

func emitterCreatedEvent(contractID string, packageID string, entityName string, signatories []string, observers []string) *apiv2.Event {
	return &apiv2.Event{Event: &apiv2.Event_Created{Created: &apiv2.CreatedEvent{
		ContractId:  contractID,
		TemplateId:  &apiv2.Identifier{PackageId: packageID, ModuleName: "Wormhole.Core.State", EntityName: entityName},
		Signatories: signatories,
		Observers:   observers,
		AcsDelta:    false,
	}}}
}

func publishOnContractEvent(contractID string, consuming bool, acsDelta bool, sequence int64) *apiv2.Event {
	event := publishMessageEvent(pinnedPackageID, sequence)
	event.GetExercised().ContractId = contractID
	event.GetExercised().Consuming = consuming
	event.GetExercised().AcsDelta = acsDelta
	return event
}

var realEmitterSignatories = []string{operatorParty, ownerParty, governanceParty}
var rogueEmitterSignatories = []string{operatorParty, ownerParty, rogueGovernanceParty}

type transientEmitterCase struct {
	name          string
	events        []*apiv2.Event
	wantSequences []uint64
	wantDropped   float64
}

var transientEmitterCases = []transientEmitterCase{
	{
		name: "two publishes on one real emitter are both emitted",
		events: []*apiv2.Event{
			publishOnContractEvent("emitter-0", true, true, 0),
			emitterCreatedEvent("emitter-1", pinnedPackageID, "Emitter", realEmitterSignatories, nil),
			publishOnContractEvent("emitter-1", true, false, 1),
		},
		wantSequences: []uint64{0, 1},
	},
	{
		name: "a publish on a real emitter that RegisterEmitter created in the transaction is emitted",
		events: []*apiv2.Event{
			emitterCreatedEvent("emitter-registered", pinnedPackageID, "Emitter", realEmitterSignatories, nil),
			publishOnContractEvent("emitter-registered", true, false, 0),
		},
		wantSequences: []uint64{0},
	},
	{
		name: "a publish on a transient rogue emitter is dropped",
		events: []*apiv2.Event{
			emitterCreatedEvent("emitter-rogue", pinnedPackageID, "Emitter", rogueEmitterSignatories, nil),
			publishOnContractEvent("emitter-rogue", true, false, 0),
		},
		wantDropped: 1,
	},
	{
		name: "a transient rogue emitter that the read party observes is dropped",
		events: []*apiv2.Event{
			emitterCreatedEvent("emitter-rogue", pinnedPackageID, "Emitter", rogueEmitterSignatories, []string{governanceParty}),
			publishOnContractEvent("emitter-rogue", true, false, 0),
		},
		wantDropped: 1,
	},
	{
		name: "a transient emitter of another package is dropped",
		events: []*apiv2.Event{
			emitterCreatedEvent("emitter-lookalike", lookalikePackageID, "Emitter", realEmitterSignatories, nil),
			publishOnContractEvent("emitter-lookalike", true, false, 0),
		},
		wantDropped: 1,
	},
	{
		name: "a transient contract of another template is dropped",
		events: []*apiv2.Event{
			emitterCreatedEvent("registry-0", pinnedPackageID, "EmitterRegistry", realEmitterSignatories, nil),
			publishOnContractEvent("registry-0", true, false, 0),
		},
		wantDropped: 1,
	},
	{
		name: "a real emitter create with another contract id does not unlock the publish",
		events: []*apiv2.Event{
			emitterCreatedEvent("emitter-real", pinnedPackageID, "Emitter", realEmitterSignatories, nil),
			publishOnContractEvent("emitter-witnessed", true, false, 0),
		},
		wantDropped: 1,
	},
	{
		name: "a non-consuming exercise on a transient real emitter is dropped",
		events: []*apiv2.Event{
			emitterCreatedEvent("emitter-registered", pinnedPackageID, "Emitter", realEmitterSignatories, nil),
			publishOnContractEvent("emitter-registered", false, false, 0),
		},
		wantDropped: 1,
	},
}

func sequencesOf(messages []CantonMessage) []uint64 {
	var sequences []uint64
	for _, message := range messages {
		sequences = append(sequences, message.Sequence)
	}
	return sequences
}

func TestGetTransactionByUpdateIDOnTransientEmitters(t *testing.T) {
	updateID, err := ParseUpdateID(sampleUpdateIDHex)
	require.NoError(t, err)
	for _, tc := range transientEmitterCases {
		t.Run(tc.name, func(t *testing.T) {
			countBefore := publishWithoutReadPartyStakeholderCount(t)
			service := &fakeUpdateByIDService{response: transactionResponse(sampleUpdateIDHex, 590, tc.events...)}
			client := &grpcClient{update: service, readScope: governanceReadScope, logger: zap.NewNop()}

			transaction, err := client.GetTransactionByUpdateID(context.Background(), updateID, pinnedTmpl, "PublishMessage")
			require.NoError(t, err)

			assert.Equal(t, tc.wantSequences, sequencesOf(transaction.Messages))
			assert.Equal(t, tc.wantDropped, publishWithoutReadPartyStakeholderCount(t)-countBefore)
		})
	}
}

func TestSubscribeUpdatesOnTransientEmitters(t *testing.T) {
	for _, tc := range transientEmitterCases {
		t.Run(tc.name, func(t *testing.T) {
			countBefore := publishWithoutReadPartyStakeholderCount(t)
			delivered := false
			streamDone := make(chan struct{})
			defer close(streamDone)
			stream := &fakeStream{recv: func() (*apiv2.GetUpdatesResponse, error) {
				if !delivered {
					delivered = true
					return &apiv2.GetUpdatesResponse{Update: &apiv2.GetUpdatesResponse_Transaction{Transaction: &apiv2.Transaction{
						UpdateId: sampleUpdateIDHex, Offset: 590, Events: tc.events,
					}}}, nil
				}
				<-streamDone
				return nil, context.Canceled
			}}
			client := &grpcClient{update: &fakeUpdateService{stream: stream}, readScope: governanceReadScope, logger: zap.NewNop()}
			out := make(chan CantonMessageEvent, len(tc.events))
			subscription, err := client.SubscribeUpdates(context.Background(), 0, pinnedTmpl, "PublishMessage", out)
			require.NoError(t, err)
			defer subscription.Unsubscribe()

			var sequences []uint64
			for range tc.wantSequences {
				select {
				case event := <-out:
					sequences = append(sequences, event.Message.Sequence)
				case <-time.After(2 * time.Second):
					t.Fatalf("received %d of %d messages", len(sequences), len(tc.wantSequences))
				}
			}
			assert.Equal(t, tc.wantSequences, sequences)
			require.Eventually(t, func() bool {
				return publishWithoutReadPartyStakeholderCount(t)-countBefore == tc.wantDropped
			}, 2*time.Second, 10*time.Millisecond)
			assert.Never(t, func() bool { return len(out) > 0 }, 100*time.Millisecond, 10*time.Millisecond)
		})
	}
}

func publishUnderPackageOnContractEvent(contractID string, packageID string, sequence int64) *apiv2.Event {
	event := publishMessageEvent(packageID, sequence)
	event.GetExercised().ContractId = contractID
	event.GetExercised().AcsDelta = false
	return event
}

func TestMessagesFromTxOnTransientEmittersInUpgradeWindow(t *testing.T) {
	beforeDeadline := testRetiringDeadline.Add(-time.Second)
	cases := []struct {
		name                       string
		events                     []*apiv2.Event
		recordTime                 time.Time
		wantRoles                  []AcceptedPackageRole
		wantDropped                float64
		wantAfterDeadlineIncrement float64
	}{
		{
			name: "retiring create and retiring publish before the deadline are emitted as retiring",
			events: []*apiv2.Event{
				emitterCreatedEvent("emitter-registered", retiringPinnedPackageID, "Emitter", realEmitterSignatories, nil),
				publishUnderPackageOnContractEvent("emitter-registered", retiringPinnedPackageID, 0),
			},
			recordTime: beforeDeadline,
			wantRoles:  []AcceptedPackageRole{AcceptedPackageRoleRetiring},
		},
		{
			name: "current create and current publish are emitted as current",
			events: []*apiv2.Event{
				emitterCreatedEvent("emitter-registered", pinnedPackageID, "Emitter", realEmitterSignatories, nil),
				publishUnderPackageOnContractEvent("emitter-registered", pinnedPackageID, 0),
			},
			recordTime: beforeDeadline,
			wantRoles:  []AcceptedPackageRole{AcceptedPackageRoleCurrent},
		},
		{
			name: "current create and retiring publish are dropped",
			events: []*apiv2.Event{
				emitterCreatedEvent("emitter-registered", pinnedPackageID, "Emitter", realEmitterSignatories, nil),
				publishUnderPackageOnContractEvent("emitter-registered", retiringPinnedPackageID, 0),
			},
			recordTime:  beforeDeadline,
			wantDropped: 1,
		},
		{
			name: "retiring create and current publish are dropped",
			events: []*apiv2.Event{
				emitterCreatedEvent("emitter-registered", retiringPinnedPackageID, "Emitter", realEmitterSignatories, nil),
				publishUnderPackageOnContractEvent("emitter-registered", pinnedPackageID, 0),
			},
			recordTime:  beforeDeadline,
			wantDropped: 1,
		},
		{
			name: "retiring create and retiring publish at the deadline are dropped by the deadline",
			events: []*apiv2.Event{
				emitterCreatedEvent("emitter-registered", retiringPinnedPackageID, "Emitter", realEmitterSignatories, nil),
				publishUnderPackageOnContractEvent("emitter-registered", retiringPinnedPackageID, 0),
			},
			recordTime:                 testRetiringDeadline,
			wantAfterDeadlineIncrement: 1,
		},
	}
	for _, tc := range cases {
		t.Run(tc.name, func(t *testing.T) {
			droppedBefore := publishWithoutReadPartyStakeholderCount(t)
			afterDeadlineBefore := retiringPackagePublishAfterDeadlineCount()
			transaction := withLedgerTimes(&apiv2.Transaction{UpdateId: sampleUpdateIDHex, Events: tc.events}, sampleEffectiveAt, tc.recordTime)

			messages := messagesFromTx(transaction, upgradeWindowTmpl, "PublishMessage", governanceReadScope, zap.NewNop())

			var roles []AcceptedPackageRole
			for _, message := range messages {
				roles = append(roles, message.AcceptedPackageRole)
			}
			assert.Equal(t, tc.wantRoles, roles)
			assert.Equal(t, tc.wantDropped, publishWithoutReadPartyStakeholderCount(t)-droppedBefore)
			assert.Equal(t, tc.wantAfterDeadlineIncrement, retiringPackagePublishAfterDeadlineCount()-afterDeadlineBefore)
		})
	}
}
