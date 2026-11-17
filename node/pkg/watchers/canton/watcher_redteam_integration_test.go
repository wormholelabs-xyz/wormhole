//go:build integration

// Red-team A2 (canton/REDTEAM.md): a look-alike package that declares
// Wormhole.Core.State.Emitter.PublishMessage, observed by the real
// guardianGovernance, forges a message under a real emitter's identity.
//
//	go test -tags integration -run TestCantonWatcherIgnoresLookalikePackage ./pkg/watchers/canton -v
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

// Mirrors canton/test-lookalike/daml/Lookalike/Publish.daml.
const (
	lookalikeNonce      = uint32(77)
	lookalikePayloadHex = "f0f0"
)

// lookalikeTarget is the input of Lookalike.Publish:integrationLookalikePublish.
type lookalikeTarget struct {
	Operator           string          `json:"operator"`
	GuardianGovernance string          `json:"guardianGovernance"`
	VictimOwner        string          `json:"victimOwner"`
	VictimEmitterID    json.RawMessage `json:"victimEmitterId"`
	VictimSequence     json.RawMessage `json:"victimSequence"`
}

func TestCantonWatcherIgnoresLookalikePackage(t *testing.T) {
	rootCtx, cancel := context.WithTimeout(context.Background(), 8*time.Minute)
	defer cancel()

	dpm, testDar, port := startSandbox(t, rootCtx)
	ledgerAddr := "localhost:" + port

	cantonDir, err := filepath.Abs("../../../../canton")
	require.NoError(t, err)
	corePackageID := readDarMainPackageID(t, dpm, coreDarPath(t, cantonDir))
	lookalikeDar := filepath.Join(cantonDir, "test-lookalike", ".daml", "dist", "wormhole-core-lookalike-0.1.0.dar")
	lookalikePackageID := readDarMainPackageID(t, dpm, lookalikeDar)
	require.NotEqual(t, corePackageID, lookalikePackageID)

	var bridge realBridge
	runDamlScript(t, dpm, testDar, port, "Test.WatcherGovernanceReader:integrationGovernanceReaderSetup", "", &bridge)

	type watcherCase struct {
		name              string
		packageID         string
		wantForgedPublish bool
	}
	cases := []watcherCase{
		{name: "governance reader pinned to the core package ignores the look-alike", packageID: corePackageID},
		// Control: proves the forged publish reaches guardianGovernance's stream and decodes.
		{name: "governance reader without a package pin emits the forgery", packageID: "", wantForgedPublish: true},
	}

	msgChannels := make([]chan *common.MessagePublication, len(cases))
	obsvReqChannels := make([]chan *gossipv1.ObservationRequest, len(cases))
	streamRecorders := make([]*updateStreamRecorder, len(cases))
	for caseIndex, tc := range cases {
		msgC := make(chan *common.MessagePublication, 16)
		msgChannels[caseIndex] = msgC
		obsvReqC := make(chan *gossipv1.ObservationRequest, 1)
		obsvReqChannels[caseIndex] = obsvReqC
		streamRecorder := newUpdateStreamRecorder()
		streamRecorders[caseIndex] = streamRecorder

		w := NewWatcher(ledgerAddr, tc.packageID, bridge.GuardianGovernance, AuthConfig{}, true, msgC, obsvReqC)
		w.extraLedgerDialOpts = []grpc.DialOption{grpc.WithStreamInterceptor(streamRecorder.interceptGetUpdates)}
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
			t.Fatalf("%s: watcher did not subscribe within 60s", tc.name)
		}
	}

	bridgeInputPath := filepath.Join(t.TempDir(), "bridge.json")
	bridgeJSON, err := json.Marshal(bridge)
	require.NoError(t, err)
	require.NoError(t, os.WriteFile(bridgeInputPath, bridgeJSON, 0o600))
	var publishes governanceReaderPublishes
	runDamlScript(t, dpm, testDar, port, "Test.WatcherGovernanceReader:integrationGovernanceReaderPublish", bridgeInputPath, &publishes)

	targetInputPath := filepath.Join(t.TempDir(), "lookalike-target.json")
	targetJSON, err := json.Marshal(lookalikeTarget{
		Operator:           bridge.Operator,
		GuardianGovernance: bridge.GuardianGovernance,
		VictimOwner:        publishes.TrustedOwner,
		VictimEmitterID:    publishes.TrustedEmitterID,
		VictimSequence:     publishes.TrustedSequence,
	})
	require.NoError(t, err)
	require.NoError(t, os.WriteFile(targetInputPath, targetJSON, 0o600))
	var forger string
	runDamlScript(t, dpm, lookalikeDar, port, "Lookalike.Publish:integrationLookalikePublish", targetInputPath, &forger)
	require.NotEqual(t, publishes.TrustedOwner, forger)

	victimAddress := deriveEmitterAddress(bridge.Operator, publishes.TrustedOwner, parseDamlUint64(t, publishes.TrustedEmitterID))
	trustedPublish := observedPublish{
		EmitterChain:   vaa.ChainIDCanton,
		EmitterAddress: victimAddress,
		Sequence:       parseDamlUint64(t, publishes.TrustedSequence),
		Nonce:          trustedPublishNonce,
		Payload:        decodeHexPayload(t, trustedPublishPayloadHex),
	}
	// Same (emitter, sequence) as the trusted publish, different body.
	forgedPublish := trustedPublish
	forgedPublish.Nonce = lookalikeNonce
	forgedPublish.Payload = decodeHexPayload(t, lookalikePayloadHex)

	deadline := time.After(90 * time.Second)
	var forgedPublishUpdateID cantonclient.UpdateID
	for caseIndex, tc := range cases {
		wantPublishes := []observedPublish{trustedPublish}
		if tc.wantForgedPublish {
			wantPublishes = append(wantPublishes, forgedPublish)
		}
		collected := collectMessagePublications(t, tc.name, msgChannels[caseIndex], len(wantPublishes), deadline)
		observed := make([]observedPublish, 0, len(collected))
		for _, msg := range collected {
			observed = append(observed, toObservedPublish(msg))
			if msg.Nonce == lookalikeNonce {
				updateID, err := cantonclient.UpdateIDFromTxID(msg.TxID)
				require.NoError(t, err)
				forgedPublishUpdateID = updateID
			}
		}
		assert.ElementsMatchf(t, wantPublishes, observed, "%s", tc.name)
	}
	require.False(t, forgedPublishUpdateID.IsZero(), "the unpinned control never emitted the forgery")

	// The pinned stream received the forged transaction and filtered it.
	require.Eventually(t, func() bool {
		return streamRecorders[0].hasDeliveredUpdateID(forgedPublishUpdateID)
	}, 30*time.Second, 100*time.Millisecond, "pinned stream never delivered update %s", forgedPublishUpdateID)
	assert.Never(t, func() bool {
		return len(msgChannels[0]) > 0
	}, unexpectedEmissionWindow, 50*time.Millisecond, "pinned watcher emitted a message after the trusted publish")

	// Reobservation by update id keeps the package pin.
	for caseIndex, tc := range cases {
		reobserved := reobserveAndCollect(t, obsvReqChannels[caseIndex], msgChannels[caseIndex], forgedPublishUpdateID.TxID(), reobservationWindow)
		var wantReobserved []observedPublish
		if tc.wantForgedPublish {
			wantReobserved = []observedPublish{forgedPublish}
		}
		assert.Equalf(t, wantReobserved, toObservedPublishes(reobserved), "%s: forged publish reobservation", tc.name)
	}
}
