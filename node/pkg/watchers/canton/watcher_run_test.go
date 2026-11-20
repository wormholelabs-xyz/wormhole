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
	return startWatcherWithMode(t, fake, acceptedPackageIDs, true, defaultPackageMetadataRecheckInterval)
}

// startWatcherWithMode builds the watcher with the test approved list, so the
// test package ids are allowed outside unsafe dev mode.
func startWatcherWithMode(t *testing.T, fake *fakeClient, acceptedPackageIDs cantonclient.AcceptedPackageIDs, unsafeDevMode bool, packageMetadataRecheckInterval time.Duration) (<-chan *common.MessagePublication, chan<- *gossipv1.ObservationRequest) {
	t.Helper()
	msgC := make(chan *common.MessagePublication, 16)
	obsvReqC := make(chan *gossipv1.ObservationRequest, 4)

	w, err := newWatcherWithApprovedList(*testApprovedCorePackages(t), "canton:0", acceptedPackageIDs, sampleRegistrar, AuthConfig{}, unsafeDevMode, msgC, obsvReqC)
	require.NoError(t, err)
	w.packageMetadataRecheckInterval = packageMetadataRecheckInterval
	runWatcherUnderSupervisor(t, w, fake)
	return msgC, obsvReqC
}

// runWatcherUnderSupervisor injects the fake client and runs w until the test ends.
func runWatcherUnderSupervisor(t *testing.T, w *Watcher, fake *fakeClient) {
	t.Helper()
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

func requireEmittedSequence(t *testing.T, msgC <-chan *common.MessagePublication, wantSequence uint64, wantReobservation bool) {
	t.Helper()
	select {
	case mp := <-msgC:
		assert.Equal(t, wantSequence, mp.Sequence)
		assert.Equal(t, wantReobservation, mp.IsReobservation)
	case <-time.After(5 * time.Second):
		t.Fatalf("no observation with sequence %d emitted", wantSequence)
	}
}

func requireNothingEmitted(t *testing.T, msgC <-chan *common.MessagePublication) {
	t.Helper()
	select {
	case mp := <-msgC:
		t.Fatalf("emitted a publish the metadata gate must drop: %+v", mp)
	case <-time.After(300 * time.Millisecond):
	}
}

var (
	testCurrentVerifiedMetadata    = cantonclient.VettedPackageMetadata{PackageID: testCurrentPackageID, PackageName: cantonclient.CorePackageName, PackageVersion: testCurrentPackageVersion}
	testRetiringVerifiedMetadata   = cantonclient.VettedPackageMetadata{PackageID: testRetiringPackageID, PackageName: cantonclient.CorePackageName, PackageVersion: testRetiringPackageVersion}
	testCurrentMismatchedMetadata  = cantonclient.VettedPackageMetadata{PackageID: testCurrentPackageID, PackageName: "wormhole-core-lookalike", PackageVersion: testCurrentPackageVersion}
	testRetiringMismatchedMetadata = cantonclient.VettedPackageMetadata{PackageID: testRetiringPackageID, PackageName: cantonclient.CorePackageName, PackageVersion: "9.9.9"}
)

// Only a name or version mismatch blocks. "Not vetted" blocks only an unsafe
// dev mode role that never verified, and then drops the event without a rewind.
func TestRunGatesPublishOnPackageMetadata(t *testing.T) {
	bothVerified := []cantonclient.VettedPackageMetadata{testCurrentVerifiedMetadata, testRetiringVerifiedMetadata}
	cases := []struct {
		name          string
		unsafeDevMode bool
		responses     [][]cantonclient.VettedPackageMetadata
		metadataErr   error
		event         cantonclient.CantonMessageEvent
		wantSubscribe bool
		wantEmitted   bool
	}{
		{name: "verified current emits", responses: [][]cantonclient.VettedPackageMetadata{bothVerified}, event: eventWithRole(100, cantonclient.AcceptedPackageRoleCurrent), wantSubscribe: true, wantEmitted: true},
		{name: "verified retiring emits", responses: [][]cantonclient.VettedPackageMetadata{bothVerified}, event: eventWithRole(100, cantonclient.AcceptedPackageRoleRetiring), wantSubscribe: true, wantEmitted: true},
		{name: "dev mode verified retiring emits", unsafeDevMode: true, responses: [][]cantonclient.VettedPackageMetadata{bothVerified}, event: eventWithRole(100, cantonclient.AcceptedPackageRoleRetiring), wantSubscribe: true, wantEmitted: true},
		{name: "mismatched current stops before subscribe", responses: [][]cantonclient.VettedPackageMetadata{{testCurrentMismatchedMetadata, testRetiringVerifiedMetadata}}},
		{name: "mismatched retiring stops before subscribe", responses: [][]cantonclient.VettedPackageMetadata{{testCurrentVerifiedMetadata, testRetiringMismatchedMetadata}}},
		{name: "dev mode mismatched current stops before subscribe", unsafeDevMode: true, responses: [][]cantonclient.VettedPackageMetadata{{testCurrentMismatchedMetadata, testRetiringVerifiedMetadata}}},
		{name: "metadata rpc error stops before subscribe", metadataErr: errors.New("unavailable")},
		{name: "dev mode metadata rpc error stops before subscribe", unsafeDevMode: true, metadataErr: errors.New("unavailable")},
		{name: "not vetted current emits", responses: [][]cantonclient.VettedPackageMetadata{{}}, event: eventWithRole(100, cantonclient.AcceptedPackageRoleCurrent), wantSubscribe: true, wantEmitted: true},
		{name: "not vetted retiring with verified current emits", responses: [][]cantonclient.VettedPackageMetadata{{testCurrentVerifiedMetadata}}, event: eventWithRole(100, cantonclient.AcceptedPackageRoleRetiring), wantSubscribe: true, wantEmitted: true},
		{name: "dev mode not vetted then verified on demand emits", unsafeDevMode: true, responses: [][]cantonclient.VettedPackageMetadata{{}, {testCurrentVerifiedMetadata}}, event: eventWithRole(100, cantonclient.AcceptedPackageRoleCurrent), wantSubscribe: true, wantEmitted: true},
		{name: "dev mode never verified retiring drops without a rewind", unsafeDevMode: true, responses: [][]cantonclient.VettedPackageMetadata{{testCurrentVerifiedMetadata}}, event: eventWithRole(100, cantonclient.AcceptedPackageRoleRetiring), wantSubscribe: true},
	}
	for _, tc := range cases {
		t.Run(tc.name, func(t *testing.T) {
			fake := newFakeClient()
			fake.ledgerEnd = 50
			fake.vettedPackageMetadataResponses = tc.responses
			fake.vettedPackageMetadataErr = tc.metadataErr
			msgC, _ := startWatcherWithMode(t, fake, testVersionedWindowPackageIDs(t), tc.unsafeDevMode, defaultPackageMetadataRecheckInterval)

			if !tc.wantSubscribe {
				require.Eventually(t, func() bool { return fake.vettedPackageMetadataCallCount() >= 2 }, 15*time.Second, 50*time.Millisecond, "Run must retry the startup check")
				assert.Zero(t, fake.subscribeCount())
				return
			}
			fake.waitSubscribed(t)
			fake.pushEvent(tc.event)
			if tc.wantEmitted {
				requireEmittedSequence(t, msgC, 5, false)
				return
			}
			requireNothingEmitted(t, msgC)
			assert.Equal(t, 1, fake.subscribeCount(), "a dropped event must not restart the pump")
			// The pump stays live for the verified role.
			fake.pushEvent(eventWithRole(101, cantonclient.AcceptedPackageRoleCurrent))
			requireEmittedSequence(t, msgC, 5, false)
			assert.Equal(t, 1, fake.subscribeCount())
		})
	}
}

// After the unvet of a role that verified once, the recheck reports "not
// vetted". Stream and reobservation still emit, and the stream does not rewind.
func TestRunAdmitsPublishesAfterUnvet(t *testing.T) {
	const packageMetadataRecheckIntervalForTest = 10 * time.Millisecond
	for _, unsafeDevMode := range []bool{false, true} {
		t.Run(map[bool]string{false: "production mode", true: "unsafe dev mode"}[unsafeDevMode], func(t *testing.T) {
			fake := newFakeClient()
			fake.ledgerEnd = 50
			fake.vettedPackageMetadataResponses = [][]cantonclient.VettedPackageMetadata{
				{testCurrentVerifiedMetadata, testRetiringVerifiedMetadata},
				{testCurrentVerifiedMetadata},
			}
			retiringUpdateID := mustParseUpdateID(reobsUpdateIDHex)
			retiringTransaction := cantonclient.CantonTransaction{
				ParticipantOffset: 40,
				UpdateID:          retiringUpdateID,
				EffectiveAt:       time.Unix(1_700_000_001, 0).UTC(),
				Messages:          []cantonclient.CantonMessage{eventWithRole(40, cantonclient.AcceptedPackageRoleRetiring).Message},
			}
			retiringTransaction.Messages[0].Sequence = 9
			fake.byUpdateID[retiringUpdateID] = retiringTransaction
			msgC, obsvReqC := startWatcherWithMode(t, fake, testVersionedWindowPackageIDs(t), unsafeDevMode, packageMetadataRecheckIntervalForTest)
			fake.waitSubscribed(t)
			require.Eventually(t, func() bool { return fake.vettedPackageMetadataCallCount() >= 3 }, 5*time.Second, 10*time.Millisecond, "the recheck must run after the unvet")

			fake.pushEvent(eventWithRole(100, cantonclient.AcceptedPackageRoleRetiring))
			requireEmittedSequence(t, msgC, 5, false)

			obsvReqC <- &gossipv1.ObservationRequest{ChainId: uint32(vaa.ChainIDCanton), TxHash: retiringUpdateID.TxID()}
			requireEmittedSequence(t, msgC, 9, true)

			// No rewind: a stream restart resumes after the emitted event.
			assert.Equal(t, 1, fake.subscribeCount())
			fake.failSubscription(errors.New("stream ended: token expired"))
			fake.waitSubscribed(t)
			begin, _, _ := fake.subscribeArgs()
			assert.Equal(t, int64(100), begin)
			fake.pushEvent(eventWithRole(101, cantonclient.AcceptedPackageRoleRetiring))
			requireEmittedSequence(t, msgC, 5, false)
			assert.Equal(t, 2, fake.subscribeCount())
		})
	}
}

// A mismatch found by the recheck stops the watcher; restarts fail at the startup check.
func TestRunStopsOnMismatchAtRecheck(t *testing.T) {
	fake := newFakeClient()
	fake.ledgerEnd = 50
	fake.vettedPackageMetadataResponses = [][]cantonclient.VettedPackageMetadata{
		{testCurrentVerifiedMetadata, testRetiringVerifiedMetadata},
		{testCurrentVerifiedMetadata, testRetiringMismatchedMetadata},
	}
	_, _ = startWatcherWithMode(t, fake, testVersionedWindowPackageIDs(t), false, 10*time.Millisecond)
	fake.waitSubscribed(t)
	require.Eventually(t, func() bool { return fake.vettedPackageMetadataCallCount() >= 3 }, 15*time.Second, 10*time.Millisecond, "Run must restart and recheck at startup")
	assert.Equal(t, 1, fake.subscribeCount(), "the watcher must not subscribe again after a mismatch")
}

// Offset 0 is a valid resume point (ledger begin), distinct from "no resume point" (ledger end).
func TestRunResumesBeforeOffsetOne(t *testing.T) {
	fake := newFakeClient()
	fake.ledgerEnd = 77
	w, err := newWatcherWithApprovedList(*testApprovedCorePackages(t), "canton:0", testUpgradeWindowPackageIDs(t), "", AuthConfig{}, true, make(chan *common.MessagePublication, 1), nil)
	require.NoError(t, err)
	w.rewindResumeOffsetBefore(1)
	runWatcherUnderSupervisor(t, w, fake)
	fake.waitSubscribed(t)

	begin, _, _ := fake.subscribeArgs()
	assert.Equal(t, int64(0), begin)
}

// A refusal in the pump stops Run and rewinds to before the offset, so the restart redelivers the full transaction.
func TestRunRewindsBeforeOffsetOnPumpRefusal(t *testing.T) {
	bothVerified := []cantonclient.VettedPackageMetadata{testCurrentVerifiedMetadata, testRetiringVerifiedMetadata}
	cases := []struct {
		name              string
		responses         [][]cantonclient.VettedPackageMetadata
		errByCall         map[int]error
		emittedBeforeStop []cantonclient.CantonMessageEvent
		refusedEvent      cantonclient.CantonMessageEvent
	}{
		{
			name:         "mismatch found on demand",
			responses:    [][]cantonclient.VettedPackageMetadata{{}, {testCurrentMismatchedMetadata}, bothVerified},
			refusedEvent: eventWithRole(100, cantonclient.AcceptedPackageRoleCurrent),
		},
		{
			name:         "metadata rpc error on demand",
			responses:    [][]cantonclient.VettedPackageMetadata{{}, {}, bothVerified},
			errByCall:    map[int]error{2: errors.New("unavailable")},
			refusedEvent: eventWithRole(100, cantonclient.AcceptedPackageRoleCurrent),
		},
		{
			name:              "second message of one offset mismatched after the first emitted",
			responses:         [][]cantonclient.VettedPackageMetadata{{testCurrentVerifiedMetadata}, {testCurrentVerifiedMetadata, testRetiringMismatchedMetadata}, bothVerified},
			emittedBeforeStop: []cantonclient.CantonMessageEvent{eventWithRole(100, cantonclient.AcceptedPackageRoleCurrent)},
			refusedEvent:      eventWithRole(100, cantonclient.AcceptedPackageRoleRetiring),
		},
	}
	for _, tc := range cases {
		t.Run(tc.name, func(t *testing.T) {
			fake := newFakeClient()
			fake.ledgerEnd = 50
			fake.vettedPackageMetadataResponses = tc.responses
			fake.vettedPackageMetadataErrByCall = tc.errByCall
			msgC, _ := startWatcherWithMode(t, fake, testVersionedWindowPackageIDs(t), true, defaultPackageMetadataRecheckInterval)
			fake.waitSubscribed(t)
			begin, _, _ := fake.subscribeArgs()
			require.Equal(t, int64(50), begin)

			for _, event := range tc.emittedBeforeStop {
				fake.pushEvent(event)
				requireEmittedSequence(t, msgC, 5, false)
			}
			fake.pushEvent(tc.refusedEvent)
			fake.waitSubscribed(t)
			requireNothingEmitted(t, msgC)

			begin, _, _ = fake.subscribeArgs()
			assert.Equal(t, tc.refusedEvent.ParticipantOffset-1, begin, "the restart must resubscribe before the refused offset")
			assert.Equal(t, 2, fake.subscribeCount())
			assert.Equal(t, 3, fake.vettedPackageMetadataCallCount())
			fake.pushEvent(tc.refusedEvent)
			requireEmittedSequence(t, msgC, 5, false)
		})
	}
}

// Reobservation drops the messages of a mismatched role and emits the other role of the same transaction.
func TestHandleReobservationDropsMismatchedRole(t *testing.T) {
	for _, unsafeDevMode := range []bool{false, true} {
		t.Run(map[bool]string{false: "production mode", true: "unsafe dev mode"}[unsafeDevMode], func(t *testing.T) {
			msgC := make(chan *common.MessagePublication, 4)
			w, err := newWatcherWithApprovedList(*testApprovedCorePackages(t), "canton:0", testVersionedWindowPackageIDs(t), sampleRegistrar, AuthConfig{}, unsafeDevMode, msgC, nil)
			require.NoError(t, err)
			client := newFakeClient()
			client.vettedPackageMetadataResponses = [][]cantonclient.VettedPackageMetadata{{testCurrentVerifiedMetadata, testRetiringMismatchedMetadata}}
			require.ErrorIs(t, w.refreshPackageMetadata(context.Background(), zap.NewNop(), client), errPackageMetadataMismatched)

			reobsUpdateID := mustParseUpdateID(reobsUpdateIDHex)
			retiringMessage := eventWithRole(40, cantonclient.AcceptedPackageRoleRetiring).Message
			retiringMessage.Sequence = 8
			currentMessage := eventWithRole(40, cantonclient.AcceptedPackageRoleCurrent).Message
			currentMessage.Sequence = 9
			client.byUpdateID[reobsUpdateID] = cantonclient.CantonTransaction{
				ParticipantOffset: 40,
				UpdateID:          reobsUpdateID,
				EffectiveAt:       time.Unix(1_700_000_001, 0).UTC(),
				Messages:          []cantonclient.CantonMessage{retiringMessage, currentMessage},
			}
			w.handleReobservation(context.Background(), zap.NewNop(), client, &gossipv1.ObservationRequest{ChainId: uint32(vaa.ChainIDCanton), TxHash: reobsUpdateID.TxID()})

			requireEmittedSequence(t, msgC, 9, true)
			requireNothingEmitted(t, msgC)
			assert.Equal(t, 1, client.vettedPackageMetadataCallCount(), "a known mismatch must refuse without a new RPC")
		})
	}
}
