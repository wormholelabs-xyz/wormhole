package cantonclient

import (
	"fmt"
	"testing"
	"time"

	"github.com/prometheus/client_golang/prometheus/testutil"
	"github.com/stretchr/testify/assert"
	"github.com/stretchr/testify/require"
	"go.uber.org/zap"
	"go.uber.org/zap/zapcore"
	"go.uber.org/zap/zaptest/observer"

	apiv2 "github.com/certusone/wormhole/node/pkg/cantonclient/proto/gen/com/daml/ledger/api/v2"
)

func unrelatedTemplateEvent(packageID string) *apiv2.Event {
	return &apiv2.Event{Event: &apiv2.Event_Exercised{Exercised: &apiv2.ExercisedEvent{
		TemplateId:     &apiv2.Identifier{PackageId: packageID, ModuleName: "Wormhole.Core.State", EntityName: "EmitterRegistry"},
		Choice:         "PublishMessage",
		ExerciseResult: tupleResult("00deadbeef", wormholeRecord(nil)),
	}}}
}

func unpinnedPublishCount(executingPackageID string) float64 {
	return testutil.ToFloat64(unpinnedPublishMessages.WithLabelValues(executingPackageID))
}

func retiringPackagePublishAfterDeadlineCount() float64 {
	return testutil.ToFloat64(retiringPackagePublishesAfterDeadline)
}

func publishWithoutRecordTimeCount() float64 {
	return testutil.ToFloat64(publishesWithoutRecordTime)
}

// upgradeWindowTmpl accepts pinnedPackageID and, before testRetiringDeadline, retiringPinnedPackageID.
var upgradeWindowTmpl = TemplateID{AcceptedPackageIDs: mustParseAcceptedPackageIDs(pinnedPackageID, retiringPinnedPackageID), ModuleName: "Wormhole.Core.State", EntityName: "Emitter"}

// rollbackTmpl is a rollback: the retiring id of upgradeWindowTmpl is current again.
var rollbackTmpl = TemplateID{AcceptedPackageIDs: mustParseAcceptedPackageIDs(retiringPinnedPackageID, ""), ModuleName: "Wormhole.Core.State", EntityName: "Emitter"}

func TestMessagesFromTxCountsUnpinnedPublishes(t *testing.T) {
	cases := []struct {
		name                  string
		tmpl                  TemplateID
		executingPackageID    string
		event                 *apiv2.Event
		wantMessages          int
		wantUnpinnedIncrement float64
	}{
		{name: "primary package is emitted", tmpl: pinnedTmpl, executingPackageID: pinnedPackageID, event: publishMessageEvent(pinnedPackageID, 5), wantMessages: 1},
		{name: "retiring package without a window is counted", tmpl: pinnedTmpl, executingPackageID: retiringPinnedPackageID, event: publishMessageEvent(retiringPinnedPackageID, 6), wantUnpinnedIncrement: 1},
		{name: "other package with the pinned template is counted", tmpl: pinnedTmpl, executingPackageID: lookalikePackageID, event: publishMessageEvent(lookalikePackageID, 6), wantUnpinnedIncrement: 1},
		{name: "unrelated template is ignored", tmpl: pinnedTmpl, executingPackageID: "unrelated-package", event: unrelatedTemplateEvent("unrelated-package")},
		{name: "unrelated template under the pinned package is ignored", tmpl: pinnedTmpl, executingPackageID: pinnedPackageID, event: unrelatedTemplateEvent(pinnedPackageID)},
		{name: "window: current package is emitted", tmpl: upgradeWindowTmpl, executingPackageID: pinnedPackageID, event: publishMessageEvent(pinnedPackageID, 5), wantMessages: 1},
		{name: "window: retiring package before the deadline is emitted", tmpl: upgradeWindowTmpl, executingPackageID: retiringPinnedPackageID, event: publishMessageEvent(retiringPinnedPackageID, 6), wantMessages: 1},
		{name: "window: other package is counted", tmpl: upgradeWindowTmpl, executingPackageID: lookalikePackageID, event: publishMessageEvent(lookalikePackageID, 7), wantUnpinnedIncrement: 1},
		{name: "window: empty package id is counted", tmpl: upgradeWindowTmpl, executingPackageID: "", event: publishMessageEvent("", 8), wantUnpinnedIncrement: 1},
		{name: "window: unrelated template under the retiring package is ignored", tmpl: upgradeWindowTmpl, executingPackageID: retiringPinnedPackageID, event: unrelatedTemplateEvent(retiringPinnedPackageID)},
		{name: "rollback: former retiring package is emitted", tmpl: rollbackTmpl, executingPackageID: retiringPinnedPackageID, event: publishMessageEvent(retiringPinnedPackageID, 9), wantMessages: 1},
		{name: "rollback: former current package is counted", tmpl: rollbackTmpl, executingPackageID: pinnedPackageID, event: publishMessageEvent(pinnedPackageID, 10), wantUnpinnedIncrement: 1},
	}
	for _, tc := range cases {
		t.Run(tc.name, func(t *testing.T) {
			core, logs := observer.New(zapcore.WarnLevel)
			countBefore := unpinnedPublishCount(tc.executingPackageID)
			tx := withLedgerTimes(&apiv2.Transaction{UpdateId: sampleUpdateIDHex, Events: []*apiv2.Event{tc.event}}, sampleEffectiveAt, testRetiringDeadline.Add(-time.Second))

			messages := messagesFromTx(tx, tc.tmpl, "PublishMessage", governanceParty, zap.New(core))

			assert.Len(t, messages, tc.wantMessages)
			assert.Equal(t, tc.wantUnpinnedIncrement, unpinnedPublishCount(tc.executingPackageID)-countBefore)
			if tc.wantUnpinnedIncrement == 0 {
				assert.Zero(t, logs.Len())
				return
			}
			require.Equal(t, 1, logs.Len())
			fields := logs.All()[0].ContextMap()
			assert.Equal(t, sampleUpdateIDHex, fields["updateId"])
			assert.Equal(t, tc.executingPackageID, fields["executingPackageId"])
			assert.Equal(t, tc.tmpl.AcceptedPackageIDs.String(), fields["acceptedPackageIds"])
		})
	}
}

func TestMessagesFromTxAppliesRetiringDeadline(t *testing.T) {
	cases := []struct {
		name                       string
		recordTime                 time.Time
		executingPackageID         string
		wantMessages               int
		wantAfterDeadlineIncrement float64
		wantMissingRecordTime      float64
		wantUnpinnedIncrement      float64
	}{
		{name: "retiring before the deadline", recordTime: testRetiringDeadline.Add(-time.Nanosecond), executingPackageID: retiringPinnedPackageID, wantMessages: 1},
		{name: "retiring at the deadline", recordTime: testRetiringDeadline, executingPackageID: retiringPinnedPackageID, wantAfterDeadlineIncrement: 1},
		{name: "retiring after the deadline", recordTime: testRetiringDeadline.Add(time.Minute), executingPackageID: retiringPinnedPackageID, wantAfterDeadlineIncrement: 1},
		{name: "retiring without record time", executingPackageID: retiringPinnedPackageID, wantMissingRecordTime: 1},
		{name: "current after the deadline", recordTime: testRetiringDeadline.Add(time.Minute), executingPackageID: pinnedPackageID, wantMessages: 1},
		{name: "look-alike before the deadline", recordTime: testRetiringDeadline.Add(-time.Second), executingPackageID: lookalikePackageID, wantUnpinnedIncrement: 1},
	}
	for _, tc := range cases {
		t.Run(tc.name, func(t *testing.T) {
			core, logs := observer.New(zapcore.WarnLevel)
			afterDeadlineBefore := retiringPackagePublishAfterDeadlineCount()
			missingRecordTimeBefore := publishWithoutRecordTimeCount()
			unpinnedBefore := unpinnedPublishCount(tc.executingPackageID)
			tx := withLedgerTimes(&apiv2.Transaction{UpdateId: sampleUpdateIDHex, Events: []*apiv2.Event{publishMessageEvent(tc.executingPackageID, 3)}}, sampleEffectiveAt, tc.recordTime)

			messages := messagesFromTx(tx, upgradeWindowTmpl, "PublishMessage", governanceParty, zap.New(core))

			assert.Len(t, messages, tc.wantMessages)
			assert.Equal(t, tc.wantAfterDeadlineIncrement, retiringPackagePublishAfterDeadlineCount()-afterDeadlineBefore)
			assert.Equal(t, tc.wantMissingRecordTime, publishWithoutRecordTimeCount()-missingRecordTimeBefore)
			assert.Equal(t, tc.wantUnpinnedIncrement, unpinnedPublishCount(tc.executingPackageID)-unpinnedBefore)
			wantWarnings := int(tc.wantAfterDeadlineIncrement + tc.wantMissingRecordTime + tc.wantUnpinnedIncrement)
			require.Equal(t, wantWarnings, logs.Len())
			if wantWarnings == 1 {
				assert.Equal(t, sampleUpdateIDHex, logs.All()[0].ContextMap()["updateId"])
			}
		})
	}
}

func TestMessagesFromTxWithoutPinCountsNothing(t *testing.T) {
	core, logs := observer.New(zapcore.WarnLevel)
	unpinnedTmpl := pinnedTmpl
	unpinnedTmpl.AcceptedPackageIDs = AcceptAnyPackageID()
	countBefore := unpinnedPublishCount(lookalikePackageID)
	tx := &apiv2.Transaction{UpdateId: sampleUpdateIDHex, Events: []*apiv2.Event{publishMessageEvent(lookalikePackageID, 7)}}

	messages := messagesFromTx(tx, unpinnedTmpl, "PublishMessage", governanceParty, zap.New(core))

	assert.Len(t, messages, 1)
	assert.Zero(t, unpinnedPublishCount(lookalikePackageID)-countBefore)
	assert.Zero(t, logs.Len())
}

func TestUnpinnedPackageLabelsCapDistinctValues(t *testing.T) {
	labels := newUnpinnedPackageLabels()
	for labelIndex := 0; labelIndex < maxDistinctUnpinnedPackageLabels; labelIndex++ {
		packageID := fmt.Sprintf("package-%d", labelIndex)
		require.Equal(t, packageID, labels.labelForPackageID(packageID))
	}
	assert.Equal(t, unpinnedPackageOverflowLabel, labels.labelForPackageID("package-overflow"))
	assert.Equal(t, "package-0", labels.labelForPackageID("package-0"), "a labeled package id keeps its label after the cap")
	assert.Equal(t, unpinnedPackageOverflowLabel, labels.labelForPackageID("package-overflow"))
	assert.Len(t, labels.labeledPackageIDs, maxDistinctUnpinnedPackageLabels)
}
