//go:build integration

// The watcher reads as the real guardianGovernance, which witnesses a nested
// PublishMessage on an Emitter of a rogue-governance registry
// (canton/test/daml/Test/WatcherWitnessOnly.daml). The event has acs_delta
// false, so the watcher drops it on the stream and on reobservation.
//
//	go test -tags integration -run TestCantonWatcherDropsWitnessOnlyPublish ./pkg/watchers/canton -v
package canton

import (
	"context"
	"encoding/json"
	"os"
	"path/filepath"
	"sync"
	"testing"
	"time"

	"github.com/certusone/wormhole/node/pkg/cantonclient"
	apiv2 "github.com/certusone/wormhole/node/pkg/cantonclient/proto/gen/com/daml/ledger/api/v2"
	"github.com/certusone/wormhole/node/pkg/common"
	gossipv1 "github.com/certusone/wormhole/node/pkg/proto/gossip/v1"
	"github.com/certusone/wormhole/node/pkg/supervisor"
	"github.com/prometheus/client_golang/prometheus"
	"github.com/stretchr/testify/assert"
	"github.com/stretchr/testify/require"
	"github.com/wormhole-foundation/wormhole/sdk/vaa"
	"go.uber.org/zap"
	"google.golang.org/grpc"
)

// Mirrors Test.WatcherWitnessOnly.
const (
	trustedWitnessOnlyNonce      = uint32(31)
	trustedWitnessOnlyPayloadHex = "a1a1"
	maxRecordedWitnessOnlyIDs    = 16
)

// witnessOnlyPublishes is the JSON output of Test.WatcherWitnessOnly:integrationWitnessOnlyPublish.
type witnessOnlyPublishes struct {
	TrustedOwner     string          `json:"trustedOwner"`
	TrustedEmitterID json.RawMessage `json:"trustedEmitterId"`
	TrustedSequence  json.RawMessage `json:"trustedSequence"`
	ForgedEmitterID  json.RawMessage `json:"forgedEmitterId"`
	ForgedSequence   json.RawMessage `json:"forgedSequence"`
}

// witnessOnlyPublishRecorder records the update id of each streamed
// transaction that holds a PublishMessage exercise with acs_delta false.
type witnessOnlyPublishRecorder struct {
	mutex     sync.Mutex
	updateIDs []string
}

func (r *witnessOnlyPublishRecorder) interceptGetUpdates(ctx context.Context, desc *grpc.StreamDesc, conn *grpc.ClientConn, method string, streamer grpc.Streamer, opts ...grpc.CallOption) (grpc.ClientStream, error) {
	stream, err := streamer(ctx, desc, conn, method, opts...)
	if err != nil || method != getUpdatesFullMethod {
		return stream, err
	}
	return &witnessOnlyRecordingStream{ClientStream: stream, recorder: r}, nil
}

func (r *witnessOnlyPublishRecorder) recordedUpdateIDs() []string {
	r.mutex.Lock()
	defer r.mutex.Unlock()
	return append([]string(nil), r.updateIDs...)
}

type witnessOnlyRecordingStream struct {
	grpc.ClientStream
	recorder *witnessOnlyPublishRecorder
}

func (s *witnessOnlyRecordingStream) RecvMsg(message any) error {
	if err := s.ClientStream.RecvMsg(message); err != nil {
		return err
	}
	response, isUpdatesResponse := message.(*apiv2.GetUpdatesResponse)
	if !isUpdatesResponse || response.GetTransaction() == nil {
		return nil
	}
	for _, event := range response.GetTransaction().GetEvents() {
		exercised := event.GetExercised()
		if exercised == nil || exercised.GetChoice() != "PublishMessage" || exercised.GetAcsDelta() {
			continue
		}
		s.recorder.mutex.Lock()
		if len(s.recorder.updateIDs) >= maxRecordedWitnessOnlyIDs {
			s.recorder.mutex.Unlock()
			panic("witnessOnlyPublishRecorder: update id capacity exceeded")
		}
		s.recorder.updateIDs = append(s.recorder.updateIDs, response.GetTransaction().GetUpdateId())
		s.recorder.mutex.Unlock()
		break
	}
	return nil
}

func TestCantonWatcherDropsWitnessOnlyPublish(t *testing.T) {
	rootCtx, cancel := context.WithTimeout(context.Background(), 8*time.Minute)
	defer cancel()

	dpm, testDar, port := startSandbox(t, rootCtx)
	ledgerAddr := "localhost:" + port

	cantonDir, err := filepath.Abs("../../../../canton")
	require.NoError(t, err)
	corePackageID := readDarMainPackageID(t, dpm, coreDarPath(t, cantonDir))

	var bridge realBridge
	runDamlScript(t, dpm, testDar, port, "Test.WatcherGovernanceReader:integrationGovernanceReaderSetup", "", &bridge)

	msgC := make(chan *common.MessagePublication, 16)
	obsvReqC := make(chan *gossipv1.ObservationRequest, 1)
	streamRecorder := newUpdateStreamRecorder()
	witnessOnlyRecorder := &witnessOnlyPublishRecorder{}
	w := NewWatcher(ledgerAddr, corePackageID, bridge.GuardianGovernance, AuthConfig{}, true, msgC, obsvReqC)
	w.extraLedgerDialOpts = []grpc.DialOption{grpc.WithChainStreamInterceptor(
		streamRecorder.interceptGetUpdates, witnessOnlyRecorder.interceptGetUpdates)}
	supervisor.New(rootCtx, zap.NewNop(), func(ctx context.Context) error {
		if err := supervisor.Run(ctx, "canton", w.Run); err != nil {
			return err
		}
		<-ctx.Done()
		return nil
	}, supervisor.WithPropagatePanic)
	select {
	case <-streamRecorder.subscribed:
	case <-time.After(60 * time.Second):
		t.Fatal("watcher did not subscribe within 60s")
	}

	droppedCountBefore := publishWithoutReadPartyStakeholderCount(t)
	bridgeInputPath := filepath.Join(t.TempDir(), "bridge.json")
	bridgeJSON, err := json.Marshal(bridge)
	require.NoError(t, err)
	require.NoError(t, os.WriteFile(bridgeInputPath, bridgeJSON, 0o600))
	var publishes witnessOnlyPublishes
	runDamlScript(t, dpm, testDar, port, "Test.WatcherWitnessOnly:integrationWitnessOnlyPublish", bridgeInputPath, &publishes)
	require.Equal(t, parseDamlUint64(t, publishes.TrustedEmitterID), parseDamlUint64(t, publishes.ForgedEmitterID))
	require.Equal(t, parseDamlUint64(t, publishes.TrustedSequence), parseDamlUint64(t, publishes.ForgedSequence))

	trustedPublish := observedPublish{
		EmitterChain:   vaa.ChainIDCanton,
		EmitterAddress: deriveEmitterAddress(bridge.Operator, publishes.TrustedOwner, parseDamlUint64(t, publishes.TrustedEmitterID)),
		Sequence:       parseDamlUint64(t, publishes.TrustedSequence),
		Nonce:          trustedWitnessOnlyNonce,
		Payload:        decodeHexPayload(t, trustedWitnessOnlyPayloadHex),
	}
	collected := collectMessagePublications(t, "trusted publish", msgC, 1, time.After(90*time.Second))
	assert.Equal(t, trustedPublish, toObservedPublish(collected[0]))

	require.Eventually(t, func() bool {
		return len(witnessOnlyRecorder.recordedUpdateIDs()) == 1
	}, 30*time.Second, 100*time.Millisecond, "the stream never delivered the witness-only publish")
	witnessOnlyUpdateID, err := cantonclient.ParseUpdateID(witnessOnlyRecorder.recordedUpdateIDs()[0])
	require.NoError(t, err)
	require.Eventually(t, func() bool {
		return publishWithoutReadPartyStakeholderCount(t) == droppedCountBefore+1
	}, 10*time.Second, 50*time.Millisecond, "the stream must count the witness-only publish exactly once")
	assert.Never(t, func() bool {
		return len(msgC) > 0
	}, unexpectedEmissionWindow, 50*time.Millisecond, "watcher emitted the witness-only publish")

	reobserved := reobserveAndCollect(t, obsvReqC, msgC, witnessOnlyUpdateID.TxID(), reobservationWindow)
	assert.Empty(t, reobserved, "reobservation emitted the witness-only publish")
	assert.Equal(t, droppedCountBefore+2, publishWithoutReadPartyStakeholderCount(t),
		"reobservation must count the witness-only publish again")
}

// publishWithoutReadPartyStakeholderCount reads wormhole_canton_publish_without_read_party_stakeholder_total.
func publishWithoutReadPartyStakeholderCount(t *testing.T) float64 {
	t.Helper()
	metricFamilies, err := prometheus.DefaultGatherer.Gather()
	require.NoError(t, err)
	for _, metricFamily := range metricFamilies {
		if metricFamily.GetName() == "wormhole_canton_publish_without_read_party_stakeholder_total" {
			require.Len(t, metricFamily.GetMetric(), 1)
			return metricFamily.GetMetric()[0].GetCounter().GetValue()
		}
	}
	return 0
}
