//go:build integration

// The watcher reads as the real guardianGovernance. Each transaction of
// Test.WatcherTransientEmitter publishes on an Emitter that the same
// transaction creates, so the participant sets acs_delta false on that
// publish. The watcher must emit each publish on a real Emitter and drop the
// publish on a rogue Emitter, on the stream and on reobservation.
//
//	go test -tags integration -run TestCantonWatcherPublishesOnTransientEmitters ./pkg/watchers/canton -v
package canton

import (
	"context"
	"encoding/json"
	"os"
	"path/filepath"
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
	"google.golang.org/grpc"
)

// Mirrors Test.WatcherTransientEmitter.
const (
	firstDoublePublishNonce       = uint32(61)
	firstDoublePublishPayloadHex  = "6161"
	secondDoublePublishNonce      = uint32(62)
	secondDoublePublishPayloadHex = "6262"
	registerAndPublishNonce       = uint32(63)
	registerAndPublishPayloadHex  = "6363"

	// Transactions in ledger order that hold a publish with acs_delta false.
	transientEmitterTransactionCount = 3
)

// transientEmitterPublishes is the JSON output of Test.WatcherTransientEmitter:integrationTransientEmitterPublish.
type transientEmitterPublishes struct {
	Owner                       string          `json:"owner"`
	DoublePublishEmitterID      json.RawMessage `json:"doublePublishEmitterId"`
	FirstDoublePublishSequence  json.RawMessage `json:"firstDoublePublishSequence"`
	SecondDoublePublishSequence json.RawMessage `json:"secondDoublePublishSequence"`
	RegisteredEmitterID         json.RawMessage `json:"registeredEmitterId"`
	RegisteredEmitterSequence   json.RawMessage `json:"registeredEmitterSequence"`
	RogueOwner                  string          `json:"rogueOwner"`
	RogueGovernance             string          `json:"rogueGovernance"`
	RogueEmitterID              json.RawMessage `json:"rogueEmitterId"`
	RogueEmitterSequence        json.RawMessage `json:"rogueEmitterSequence"`
}

func TestCantonWatcherPublishesOnTransientEmitters(t *testing.T) {
	rootCtx, cancel := context.WithTimeout(context.Background(), 8*time.Minute)
	defer cancel()

	dpm, testDar, port := startSandbox(t, rootCtx)
	cantonDir, err := filepath.Abs("../../../../canton")
	require.NoError(t, err)
	corePackageID := readDarMainPackageID(t, dpm, coreDarPath(t, cantonDir))

	var bridge realBridge
	runDamlScript(t, dpm, testDar, port, "Test.WatcherGovernanceReader:integrationGovernanceReaderSetup", "", &bridge)

	msgC := make(chan *common.MessagePublication, 16)
	obsvReqC := make(chan *gossipv1.ObservationRequest, 1)
	streamRecorder := newUpdateStreamRecorder()
	transientPublishRecorder := &witnessOnlyPublishRecorder{}
	acceptedPackageIDs, err := cantonclient.ParseAcceptedPackageIDs(cantonclient.CurrentPackageConfig{PackageIDHex: corePackageID}, cantonclient.RetiringPackageConfig{}, time.Now())
	require.NoError(t, err)
	w, err := NewWatcher("localhost:"+port, acceptedPackageIDs, bridge.GuardianGovernance, AuthConfig{}, true, msgC, obsvReqC)
	require.NoError(t, err)
	w.extraLedgerDialOpts = []grpc.DialOption{grpc.WithChainStreamInterceptor(
		streamRecorder.interceptGetUpdates, transientPublishRecorder.interceptGetUpdates)}
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
	var publishes transientEmitterPublishes
	runDamlScript(t, dpm, testDar, port, "Test.WatcherTransientEmitter:integrationTransientEmitterPublish", bridgeInputPath, &publishes)
	require.Equal(t, uint64(0), parseDamlUint64(t, publishes.FirstDoublePublishSequence))
	require.Equal(t, uint64(1), parseDamlUint64(t, publishes.SecondDoublePublishSequence))
	require.Equal(t, uint64(0), parseDamlUint64(t, publishes.RegisteredEmitterSequence))
	require.Equal(t, parseDamlUint64(t, publishes.DoublePublishEmitterID)+1, parseDamlUint64(t, publishes.RegisteredEmitterID))
	require.NotEqual(t, bridge.GuardianGovernance, publishes.RogueGovernance)

	doublePublishAddress := DeriveEmitterAddress(bridge.GuardianGovernance, publishes.Owner, parseDamlUint64(t, publishes.DoublePublishEmitterID))
	wantDoublePublish := []observedPublish{
		{
			EmitterChain:   vaa.ChainIDCanton,
			EmitterAddress: doublePublishAddress,
			Sequence:       0,
			Nonce:          firstDoublePublishNonce,
			Payload:        decodeHexPayload(t, firstDoublePublishPayloadHex),
		},
		{
			EmitterChain:   vaa.ChainIDCanton,
			EmitterAddress: doublePublishAddress,
			Sequence:       1,
			Nonce:          secondDoublePublishNonce,
			Payload:        decodeHexPayload(t, secondDoublePublishPayloadHex),
		},
	}
	wantRegisterAndPublish := []observedPublish{{
		EmitterChain:   vaa.ChainIDCanton,
		EmitterAddress: DeriveEmitterAddress(bridge.GuardianGovernance, publishes.Owner, parseDamlUint64(t, publishes.RegisteredEmitterID)),
		Sequence:       0,
		Nonce:          registerAndPublishNonce,
		Payload:        decodeHexPayload(t, registerAndPublishPayloadHex),
	}}

	collected := collectMessagePublications(t, "publishes on transient emitters", msgC,
		len(wantDoublePublish)+len(wantRegisterAndPublish), time.After(90*time.Second))
	assert.Equal(t, append(append([]observedPublish(nil), wantDoublePublish...), wantRegisterAndPublish...),
		toObservedPublishes(collected))

	require.Eventually(t, func() bool {
		return len(transientPublishRecorder.recordedUpdateIDs()) == transientEmitterTransactionCount
	}, 30*time.Second, 100*time.Millisecond, "the stream must deliver each transaction with a transient-emitter publish")
	require.Eventually(t, func() bool {
		return publishWithoutReadPartyStakeholderCount(t) == droppedCountBefore+1
	}, 10*time.Second, 50*time.Millisecond, "the stream must drop only the rogue publish")
	assert.Never(t, func() bool {
		return len(msgC) > 0
	}, unexpectedEmissionWindow, 50*time.Millisecond, "watcher emitted the rogue publish")

	recordedUpdateIDs := transientPublishRecorder.recordedUpdateIDs()
	reobservationCases := []struct {
		name        string
		updateIDHex string
		want        []observedPublish
	}{
		{name: "two publishes on one emitter", updateIDHex: recordedUpdateIDs[0], want: wantDoublePublish},
		{name: "RegisterEmitter and a publish", updateIDHex: recordedUpdateIDs[1], want: wantRegisterAndPublish},
		{name: "rogue RegisterEmitter and a publish", updateIDHex: recordedUpdateIDs[2], want: nil},
	}
	for _, reobservationCase := range reobservationCases {
		updateID, err := cantonclient.ParseUpdateID(reobservationCase.updateIDHex)
		require.NoError(t, err)
		reobserved := reobserveAndCollect(t, obsvReqC, msgC, updateID.TxID(), reobservationWindow)
		assert.Equalf(t, reobservationCase.want, toObservedPublishes(reobserved), "reobservation: %s", reobservationCase.name)
	}
	assert.Equal(t, droppedCountBefore+2, publishWithoutReadPartyStakeholderCount(t),
		"reobservation must drop only the rogue publish")
}
