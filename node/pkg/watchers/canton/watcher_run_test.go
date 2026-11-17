package canton

import (
	"context"
	"errors"
	"testing"
	"time"

	"github.com/certusone/wormhole/node/pkg/cantonclient"
	"github.com/certusone/wormhole/node/pkg/common"
	gossipv1 "github.com/certusone/wormhole/node/pkg/proto/gossip/v1"
	"github.com/certusone/wormhole/node/pkg/supervisor"
	"github.com/stretchr/testify/assert"
	"github.com/stretchr/testify/require"
	"github.com/wormhole-foundation/wormhole/sdk/vaa"
	"go.uber.org/zap"
)

// startWatcher runs the real Run loop under a supervisor with an injected fake
// client, so the supervisor restarts Run on error just as guardiand does.
func startWatcher(t *testing.T, fake *fakeClient) (<-chan *common.MessagePublication, chan<- *gossipv1.ObservationRequest) {
	t.Helper()
	return startWatcherWith(t, fake, testUpgradeWindowPackageIDs(t))
}

func startWatcherWith(t *testing.T, fake *fakeClient, acceptedPackageIDs cantonclient.AcceptedPackageIDs) (<-chan *common.MessagePublication, chan<- *gossipv1.ObservationRequest) {
	t.Helper()
	msgC := make(chan *common.MessagePublication, 16)
	obsvReqC := make(chan *gossipv1.ObservationRequest, 4)

	w, err := NewWatcher("canton:0", acceptedPackageIDs, "", AuthConfig{}, true, msgC, obsvReqC)
	require.NoError(t, err)
	w.cantonClient = fake // inject the fake via the DI seam

	ctx, cancel := context.WithCancel(context.Background())
	t.Cleanup(cancel)
	supervisor.New(ctx, zap.NewNop(), func(sctx context.Context) error {
		if err := supervisor.Run(sctx, "canton", w.Run); err != nil {
			return err
		}
		<-sctx.Done()
		return nil
	})
	return msgC, obsvReqC
}

func testUpgradeWindowPackageIDs(t *testing.T) cantonclient.AcceptedPackageIDs {
	t.Helper()
	accepted, err := cantonclient.ParseAcceptedPackageIDs(cantonclient.CurrentPackageConfig{PackageIDHex: testCurrentPackageID}, cantonclient.RetiringPackageConfig{PackageIDHex: testRetiringPackageID, AcceptedUntilUTC: retiringDeadlineFromNow(24 * time.Hour)}, time.Now())
	require.NoError(t, err)
	return accepted
}

func sampleEvent(participantOffset int64) cantonclient.CantonMessageEvent {
	return cantonclient.CantonMessageEvent{
		ParticipantOffset: participantOffset,
		UpdateID:          mustParseUpdateID(sampleUpdateIDHex),
		EffectiveAt:       time.Unix(1_700_000_000, 0).UTC(),
		Message: cantonclient.CantonMessage{
			AcceptedPackageRole: cantonclient.AcceptedPackageRoleCurrent,
			Registrar:           sampleRegistrar,
			Owner:               sampleOwner,
			EmitterID:           sampleEmitterID,
			Sequence:            5,
			Nonce:               42,
			Payload:             []byte{0x11, 0x22},
		},
	}
}

// TestRunEmitsObservation proves the data-pump goroutine turns a streamed event
// into a MessagePublication on msgC.
func TestRunEmitsObservation(t *testing.T) {
	fake := newFakeClient()
	fake.ledgerEnd = 77 // the offset the data pump must stream from
	msgC, _ := startWatcher(t, fake)
	fake.waitSubscribed(t)

	// The watcher must subscribe from the captured ledger end, filtering on the
	// core-bridge template (with its configured package id) and PublishMessage.
	begin, tmpl, choice := fake.subscribeArgs()
	assert.Equal(t, int64(77), begin)
	assert.Equal(t, "PublishMessage", choice)
	assert.Equal(t, cantonclient.TemplateID{AcceptedPackageIDs: testUpgradeWindowPackageIDs(t), ModuleName: "Wormhole.Core.State", EntityName: "Emitter"}, tmpl)

	fake.pushEvent(sampleEvent(100))

	select {
	case mp := <-msgC:
		assert.False(t, mp.IsReobservation)
		assert.Equal(t, vaa.ChainIDCanton, mp.EmitterChain)
		assert.Equal(t, uint64(5), mp.Sequence)
		assert.Equal(t, uint32(42), mp.Nonce)
		assert.Equal(t, []byte{0x11, 0x22}, mp.Payload)
		assert.Equal(t, wantAddr(t), mp.EmitterAddress)
		assert.Equal(t, mustParseUpdateID(sampleUpdateIDHex).TxID(), mp.TxID)
	case <-time.After(5 * time.Second):
		t.Fatal("no observation emitted")
	}
}

// TestRunReobservation proves an ObservationRequest on obsvReqC drives
// GetTransactionByUpdateID and re-emits with IsReobservation=true.
func TestRunReobservation(t *testing.T) {
	fake := newFakeClient()
	reobsUpdateID := mustParseUpdateID(reobsUpdateIDHex)
	fake.byUpdateID[reobsUpdateID] = cantonclient.CantonTransaction{
		ParticipantOffset: 555,
		UpdateID:          reobsUpdateID,
		EffectiveAt:       time.Unix(1_700_000_001, 0).UTC(),
		Messages: []cantonclient.CantonMessage{{
			AcceptedPackageRole: cantonclient.AcceptedPackageRoleCurrent,
			Registrar:           sampleRegistrar,
			Owner:               sampleOwner,
			EmitterID:           sampleEmitterID,
			Sequence:            9,
			Payload:             []byte{0xAB},
		}},
	}
	msgC, obsvReqC := startWatcher(t, fake)
	fake.waitSubscribed(t)

	obsvReqC <- &gossipv1.ObservationRequest{
		ChainId: uint32(vaa.ChainIDCanton),
		TxHash:  reobsUpdateID.TxID(),
	}

	select {
	case mp := <-msgC:
		assert.True(t, mp.IsReobservation)
		assert.Equal(t, uint64(9), mp.Sequence)
		assert.Equal(t, reobsUpdateID.TxID(), mp.TxID)
	case <-time.After(5 * time.Second):
		t.Fatal("no reobservation emitted")
	}
	_, streamTmpl, _ := fake.subscribeArgs()
	assert.Equal(t, streamTmpl, fake.latestGetTransactionTmpl(), "reobservation must use the stream's accepted package ids")
}

// TestRunResubscribesOnStreamError proves a terminal subscription error (which
// is how the client surfaces a stream fault or a clean io.EOF end) makes Run
// return so the supervisor restarts it and re-subscribes.
func TestRunResubscribesOnStreamError(t *testing.T) {
	fake := newFakeClient()
	_, _ = startWatcher(t, fake)
	fake.waitSubscribed(t)
	require.Equal(t, 1, fake.subscribeCount())

	fake.failSubscription(errors.New("boom"))

	require.Eventually(t, func() bool {
		return fake.subscribeCount() >= 2
	}, 15*time.Second, 100*time.Millisecond, "watcher did not re-subscribe after a stream error")
}

// TestRunReobservationErrorDoesNotEmit proves a GetTransactionByUpdateID failure is
// handled gracefully — no publication, no crash.
func TestRunReobservationErrorDoesNotEmit(t *testing.T) {
	fake := newFakeClient()
	fake.getTransactionErr = errors.New("not found")
	msgC, obsvReqC := startWatcher(t, fake)
	fake.waitSubscribed(t)

	obsvReqC <- &gossipv1.ObservationRequest{
		ChainId: uint32(vaa.ChainIDCanton),
		TxHash:  mustParseUpdateID(sampleUpdateIDHex).TxID(),
	}

	select {
	case mp := <-msgC:
		t.Fatalf("expected no publication on reobservation error, got %+v", mp)
	case <-time.After(750 * time.Millisecond):
		// no publication — correct
	}
}

// TestRunFailsFastOnStartupError proves a GetLedgerEnd failure returns before
// the watcher subscribes, so it never reports healthy while blind.
func TestRunFailsFastOnStartupError(t *testing.T) {
	fake := newFakeClient()
	fake.ledgerEndErr = errors.New("participant unreachable")
	_, _ = startWatcher(t, fake)

	require.Eventually(t, func() bool {
		fake.mu.Lock()
		defer fake.mu.Unlock()
		return fake.ledgerEndCalls >= 2 // Run keeps failing at startup and being restarted
	}, 15*time.Second, 100*time.Millisecond, "startup error did not cause repeated Run restarts")

	assert.Equal(t, 0, fake.subscribeCount(), "must not subscribe when GetLedgerEnd fails")
}

// TestProcessMessageDropsZeroTimestamp covers the zero-effective-time guard.
func TestProcessMessageDropsZeroTimestamp(t *testing.T) {
	msgC := make(chan *common.MessagePublication, 1)
	w := testWatcher(t, msgC)
	w.processMessage(zap.NewNop(), cantonclient.CantonMessageEvent{
		ParticipantOffset: 1,
		UpdateID:          mustParseUpdateID(sampleUpdateIDHex),
		EffectiveAt:       time.Time{},
		Message:           cantonclient.CantonMessage{AcceptedPackageRole: cantonclient.AcceptedPackageRoleCurrent, Registrar: sampleRegistrar, Owner: sampleOwner},
	}, false)

	select {
	case <-msgC:
		t.Fatal("expected no observation for a zero effective time")
	default:
	}
}

// TestRunResumesFromLastOffset proves a supervisor-restarted Run resubscribes
// from the last processed stream offset rather than the current ledger end.
// Canton terminates long-lived streams when their access token expires, so
// stream death and restart is routine; resuming from ledger end would silently
// drop anything published during the reconnect window.
func TestRunResumesFromLastOffset(t *testing.T) {
	fake := newFakeClient()
	fake.ledgerEnd = 77
	msgC, _ := startWatcher(t, fake)
	fake.waitSubscribed(t)

	fake.pushEvent(sampleEvent(100))
	select {
	case <-msgC:
	case <-time.After(5 * time.Second):
		t.Fatal("no observation emitted")
	}

	fake.failSubscription(errors.New("stream ended: token expired"))
	fake.waitSubscribed(t) // supervisor restarts Run, which resubscribes

	begin, _, _ := fake.subscribeArgs()
	assert.Equal(t, int64(100), begin, "resume must start from the last processed offset, not ledger end")
	require.GreaterOrEqual(t, fake.subscribeCount(), 2)
}

func testVersionedWindowPackageIDs(t *testing.T) cantonclient.AcceptedPackageIDs {
	t.Helper()
	accepted, err := cantonclient.ParseAcceptedPackageIDs(
		cantonclient.CurrentPackageConfig{PackageIDHex: testCurrentPackageID, Version: testCurrentPackageVersion},
		cantonclient.RetiringPackageConfig{PackageIDHex: testRetiringPackageID, AcceptedUntilUTC: retiringDeadlineFromNow(24 * time.Hour), Version: testRetiringPackageVersion},
		time.Now())
	require.NoError(t, err)
	return accepted
}

func eventWithRole(participantOffset int64, role cantonclient.AcceptedPackageRole) cantonclient.CantonMessageEvent {
	event := sampleEvent(participantOffset)
	event.Message.AcceptedPackageRole = role
	return event
}

func TestRunGatesPublishOnPackageMetadata(t *testing.T) {
	currentVerified := cantonclient.VettedPackageMetadata{PackageID: testCurrentPackageID, PackageName: cantonclient.CorePackageName, PackageVersion: testCurrentPackageVersion}
	retiringVerified := cantonclient.VettedPackageMetadata{PackageID: testRetiringPackageID, PackageName: cantonclient.CorePackageName, PackageVersion: testRetiringPackageVersion}
	currentMismatched := cantonclient.VettedPackageMetadata{PackageID: testCurrentPackageID, PackageName: "wormhole-core-lookalike", PackageVersion: testCurrentPackageVersion}
	cases := []struct {
		name                  string
		responses             [][]cantonclient.VettedPackageMetadata
		metadataErr           error
		event                 cantonclient.CantonMessageEvent
		wantSubscribe         bool
		wantEmitted           bool
		wantResubscribeOffset int64
	}{
		{name: "verified emits", responses: [][]cantonclient.VettedPackageMetadata{{currentVerified, retiringVerified}}, event: eventWithRole(100, cantonclient.AcceptedPackageRoleCurrent), wantSubscribe: true, wantEmitted: true},
		{name: "verified retiring emits", responses: [][]cantonclient.VettedPackageMetadata{{currentVerified, retiringVerified}}, event: eventWithRole(100, cantonclient.AcceptedPackageRoleRetiring), wantSubscribe: true, wantEmitted: true},
		{name: "mismatched returns an error before subscribe", responses: [][]cantonclient.VettedPackageMetadata{{currentMismatched, retiringVerified}}},
		{name: "metadata rpc error returns an error before subscribe", metadataErr: errors.New("unavailable")},
		{name: "not yet vetted then a recheck that verifies emits", responses: [][]cantonclient.VettedPackageMetadata{{}, {currentVerified}}, event: eventWithRole(100, cantonclient.AcceptedPackageRoleCurrent), wantSubscribe: true, wantEmitted: true},
		{name: "recheck still not vetted fails the pump and rewinds", responses: [][]cantonclient.VettedPackageMetadata{{}}, event: eventWithRole(100, cantonclient.AcceptedPackageRoleCurrent), wantSubscribe: true, wantResubscribeOffset: 99},
		{name: "unverified retiring with verified current fails the pump", responses: [][]cantonclient.VettedPackageMetadata{{currentVerified}}, event: eventWithRole(100, cantonclient.AcceptedPackageRoleRetiring), wantSubscribe: true, wantResubscribeOffset: 99},
	}
	for _, tc := range cases {
		t.Run(tc.name, func(t *testing.T) {
			fake := newFakeClient()
			fake.ledgerEnd = 50
			fake.vettedPackageMetadataResponses = tc.responses
			fake.vettedPackageMetadataErr = tc.metadataErr
			msgC, _ := startWatcherWith(t, fake, testVersionedWindowPackageIDs(t))

			if !tc.wantSubscribe {
				require.Eventually(t, func() bool { return fake.vettedPackageMetadataCallCount() >= 2 }, 15*time.Second, 50*time.Millisecond, "Run must retry the startup check")
				assert.Zero(t, fake.subscribeCount())
				return
			}
			fake.waitSubscribed(t)
			fake.pushEvent(tc.event)
			if tc.wantEmitted {
				select {
				case mp := <-msgC:
					assert.Equal(t, uint64(5), mp.Sequence)
				case <-time.After(5 * time.Second):
					t.Fatal("no observation emitted")
				}
				return
			}
			fake.waitSubscribed(t)
			begin, _, _ := fake.subscribeArgs()
			assert.Equal(t, tc.wantResubscribeOffset, begin, "the pump must resume before the unverified event")
			select {
			case mp := <-msgC:
				t.Fatalf("emitted a publish from an unverified package: %+v", mp)
			case <-time.After(300 * time.Millisecond):
			}
		})
	}
}
