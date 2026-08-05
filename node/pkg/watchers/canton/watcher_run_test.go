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
	msgC := make(chan *common.MessagePublication, 16)
	obsvReqC := make(chan *gossipv1.ObservationRequest, 4)

	w := NewWatcher("canton:0", "pkg", "", true, msgC, obsvReqC)
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

func sampleEvent(offset int64) cantonclient.CantonMessageEvent {
	return cantonclient.CantonMessageEvent{
		Offset:      offset,
		UpdateID:    "u",
		EffectiveAt: time.Unix(1_700_000_000, 0).UTC(),
		Message: cantonclient.CantonMessage{
			Registrar: sampleRegistrar,
			Owner:     sampleOwner,
			EmitterID: sampleEmitterID,
			Sequence:  5,
			Nonce:     42,
			Payload:   []byte{0x11, 0x22},
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
	assert.Equal(t, cantonclient.TemplateID{PackageID: "pkg", ModuleName: "Wormhole.Core.State", EntityName: "Emitter"}, tmpl)

	fake.pushEvent(sampleEvent(100))

	select {
	case mp := <-msgC:
		assert.False(t, mp.IsReobservation)
		assert.Equal(t, vaa.ChainIDCanton, mp.EmitterChain)
		assert.Equal(t, uint64(5), mp.Sequence)
		assert.Equal(t, uint32(42), mp.Nonce)
		assert.Equal(t, []byte{0x11, 0x22}, mp.Payload)
		assert.Equal(t, wantAddr(t), mp.EmitterAddress)
		assert.Equal(t, cantonclient.OffsetToTxID(100), mp.TxID)
	case <-time.After(5 * time.Second):
		t.Fatal("no observation emitted")
	}
}

// TestRunReobservation proves an ObservationRequest on obsvReqC drives
// GetUpdateByOffset and re-emits with IsReobservation=true.
func TestRunReobservation(t *testing.T) {
	fake := newFakeClient()
	const offset int64 = 555
	fake.byOffset[offset] = cantonclient.CantonTransaction{
		Offset:      offset,
		UpdateID:    "reobs",
		EffectiveAt: time.Unix(1_700_000_001, 0).UTC(),
		Messages: []cantonclient.CantonMessage{{
			Registrar: sampleRegistrar,
			Owner:     sampleOwner,
			EmitterID: sampleEmitterID,
			Sequence:  9,
			Payload:   []byte{0xAB},
		}},
	}
	msgC, obsvReqC := startWatcher(t, fake)
	fake.waitSubscribed(t)

	obsvReqC <- &gossipv1.ObservationRequest{
		ChainId: uint32(vaa.ChainIDCanton),
		TxHash:  cantonclient.OffsetToTxID(offset),
	}

	select {
	case mp := <-msgC:
		assert.True(t, mp.IsReobservation)
		assert.Equal(t, uint64(9), mp.Sequence)
		assert.Equal(t, cantonclient.OffsetToTxID(offset), mp.TxID)
	case <-time.After(5 * time.Second):
		t.Fatal("no reobservation emitted")
	}
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

// TestRunReobservationErrorDoesNotEmit proves a GetUpdateByOffset failure is
// handled gracefully — no publication, no crash.
func TestRunReobservationErrorDoesNotEmit(t *testing.T) {
	fake := newFakeClient()
	fake.getUpdateErr = errors.New("not found")
	msgC, obsvReqC := startWatcher(t, fake)
	fake.waitSubscribed(t)

	obsvReqC <- &gossipv1.ObservationRequest{
		ChainId: uint32(vaa.ChainIDCanton),
		TxHash:  cantonclient.OffsetToTxID(1),
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
	w := testWatcher(msgC)
	w.processMessage(zap.NewNop(), cantonclient.CantonMessageEvent{
		Offset:      1,
		EffectiveAt: time.Time{}, // zero
		Message:     cantonclient.CantonMessage{Registrar: sampleRegistrar, Owner: sampleOwner},
	}, false)

	select {
	case <-msgC:
		t.Fatal("expected no observation for a zero effective time")
	default:
	}
}
