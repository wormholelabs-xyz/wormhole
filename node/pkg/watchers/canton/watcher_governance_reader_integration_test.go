//go:build integration

// Production watcher configuration against a live sandbox: readAsParty is the
// real guardianGovernance and packageID is the wormhole-core package id.
//
//	go test -tags integration -run TestCantonWatcherGovernanceReaderIntegration ./pkg/watchers/canton -v
package canton

import (
	"context"
	"encoding/hex"
	"encoding/json"
	"os"
	"os/exec"
	"path/filepath"
	"strconv"
	"strings"
	"sync"
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
	"google.golang.org/grpc/credentials/insecure"
)

// Mirrors Test.WatcherGovernanceReader.
const (
	trustedPublishNonce      = uint32(42)
	trustedPublishPayloadHex = "11223344"
	rogueShadowNonce         = uint32(66)
	rogueShadowPayloadHex    = "deadbeef"

	// Extra wait after the expected messages, before checking for unexpected ones.
	governanceReaderQuietWindow = 15 * time.Second
)

// realBridge is the JSON output of Test.WatcherGovernanceReader:integrationGovernanceReaderSetup.
type realBridge struct {
	Operator           string `json:"operator"`
	GuardianGovernance string `json:"guardianGovernance"`
	CoreStateCid       string `json:"coreStateCid"`
}

// governanceReaderPublishes is the JSON output of
// Test.WatcherGovernanceReader:integrationGovernanceReaderPublish.
type governanceReaderPublishes struct {
	TrustedOwner        string          `json:"trustedOwner"`
	TrustedEmitterID    json.RawMessage `json:"trustedEmitterId"`
	TrustedSequence     json.RawMessage `json:"trustedSequence"`
	RogueOwner          string          `json:"rogueOwner"`
	RogueGovernance     string          `json:"rogueGovernance"`
	RogueShadowSequence json.RawMessage `json:"rogueShadowSequence"`
}

// observedPublish is the subset of a MessagePublication the cases compare.
type observedPublish struct {
	EmitterChain   vaa.ChainID
	EmitterAddress vaa.Address
	Sequence       uint64
	Nonce          uint32
	Payload        []byte
}

// subscriptionSignalingClient closes subscribed once the watcher's update
// stream has its start offset fixed.
type subscriptionSignalingClient struct {
	cantonclient.CantonClient
	subscribed chan struct{}
	once       sync.Once
}

func (c *subscriptionSignalingClient) SubscribeUpdates(ctx context.Context, beginExclusive int64, tmpl cantonclient.TemplateID, choiceName string, out chan<- cantonclient.CantonMessageEvent) (*cantonclient.Subscription, error) {
	subscription, err := c.CantonClient.SubscribeUpdates(ctx, beginExclusive, tmpl, choiceName, out)
	if err == nil {
		c.once.Do(func() { close(c.subscribed) })
	}
	return subscription, err
}

// parseDamlUint64 accepts the Daml-LF JSON Int64 encoding (quoted or bare).
func parseDamlUint64(t *testing.T, encoded json.RawMessage) uint64 {
	t.Helper()
	parsed, err := strconv.ParseUint(strings.Trim(string(encoded), `"`), 10, 64)
	require.NoErrorf(t, err, "Daml Int64 %s", encoded)
	return parsed
}

func decodeHexPayload(t *testing.T, payloadHex string) []byte {
	t.Helper()
	payload, err := hex.DecodeString(payloadHex)
	require.NoError(t, err)
	return payload
}

// readDarMainPackageID returns the main package id of a DAR via `dpm damlc inspect-dar`.
func readDarMainPackageID(t *testing.T, dpm, darPath string) string {
	t.Helper()
	out, err := exec.Command(dpm, "damlc", "inspect-dar", "--json", darPath).Output()
	require.NoErrorf(t, err, "inspect-dar %s", darPath)
	var inspected struct {
		MainPackageID string `json:"main_package_id"`
	}
	require.NoError(t, json.Unmarshal(out, &inspected))
	require.Len(t, inspected.MainPackageID, 64, "package id is a 64-char hex hash")
	return inspected.MainPackageID
}

// runDamlScript runs scriptName from the test DAR and decodes its JSON output
// into result. inputPath is the script argument file, or "" for none.
func runDamlScript(t *testing.T, dpm, dar, port, scriptName, inputPath string, result any) {
	t.Helper()
	outputPath := filepath.Join(t.TempDir(), "output.json")
	args := []string{"script", "--dar", dar, "--upload-dar", "yes",
		"--script-name", scriptName,
		"--ledger-host", "localhost", "--ledger-port", port,
		"--output-file", outputPath}
	if inputPath != "" {
		args = append(args, "--input-file", inputPath)
	}
	out, err := exec.Command(dpm, args...).CombinedOutput()
	require.NoErrorf(t, err, "dpm script %s failed: %s", scriptName, out)
	raw, err := os.ReadFile(outputPath)
	require.NoError(t, err)
	require.NoErrorf(t, json.Unmarshal(raw, result), "decode %s output: %s", scriptName, raw)
}

func collectObservedPublishes(t *testing.T, caseName string, msgC <-chan *common.MessagePublication, wantCount int, deadline <-chan time.Time) []observedPublish {
	t.Helper()
	observed := make([]observedPublish, 0, wantCount)
	for len(observed) < wantCount {
		select {
		case msg := <-msgC:
			require.Falsef(t, msg.IsReobservation, "%s: live observation flagged as reobservation", caseName)
			observed = append(observed, observedPublish{
				EmitterChain:   msg.EmitterChain,
				EmitterAddress: msg.EmitterAddress,
				Sequence:       msg.Sequence,
				Nonce:          msg.Nonce,
				Payload:        msg.Payload,
			})
		case <-deadline:
			t.Fatalf("%s: received %d of %d expected messages before the deadline", caseName, len(observed), wantCount)
		}
	}
	return observed
}

func TestCantonWatcherGovernanceReaderIntegration(t *testing.T) {
	rootCtx, cancel := context.WithTimeout(context.Background(), 8*time.Minute)
	defer cancel()

	dpm, testDar, port := startSandbox(t, rootCtx)
	ledgerAddr := "localhost:" + port

	cantonDir, err := filepath.Abs("../../../../canton")
	require.NoError(t, err)
	corePackageID := readDarMainPackageID(t, dpm, filepath.Join(cantonDir, "core", ".daml", "dist", "wormhole-core-0.6.0.dar"))
	testPackageID := readDarMainPackageID(t, dpm, testDar)
	require.NotEqual(t, corePackageID, testPackageID)

	var bridge realBridge
	runDamlScript(t, dpm, testDar, port, "Test.WatcherGovernanceReader:integrationGovernanceReaderSetup", "", &bridge)
	require.Contains(t, bridge.GuardianGovernance, "::", "guardianGovernance must be a full party id")
	require.NotEqual(t, bridge.Operator, bridge.GuardianGovernance)

	type watcherCase struct {
		name               string
		readAsParty        string
		packageID          string
		wantRogueShadow    bool
		wantTrustedPublish bool
	}
	cases := []watcherCase{
		{
			name:               "governance reader with core package emits only the trusted publish",
			readAsParty:        bridge.GuardianGovernance,
			packageID:          corePackageID,
			wantTrustedPublish: true,
		},
		{
			name:        "governance reader with wrong package emits nothing",
			readAsParty: bridge.GuardianGovernance,
			packageID:   testPackageID,
		},
		{
			// Control: proves the rogue shadow publish is on the ledger stream.
			name:               "wildcard reader with core package also emits the rogue shadow publish",
			readAsParty:        "",
			packageID:          corePackageID,
			wantRogueShadow:    true,
			wantTrustedPublish: true,
		},
	}

	msgChannels := make([]chan *common.MessagePublication, len(cases))
	for caseIndex, tc := range cases {
		msgC := make(chan *common.MessagePublication, 16)
		msgChannels[caseIndex] = msgC
		grpcClient, err := cantonclient.NewCantonGrpcClient(ledgerAddr, tc.readAsParty, zap.NewNop(), insecure.NewCredentials())
		require.NoError(t, err)
		t.Cleanup(func() { _ = grpcClient.Close() })
		signalingClient := &subscriptionSignalingClient{CantonClient: grpcClient, subscribed: make(chan struct{})}

		w := NewWatcher(ledgerAddr, tc.packageID, tc.readAsParty, AuthConfig{}, true, msgC, make(chan *gossipv1.ObservationRequest))
		w.cantonClient = signalingClient
		supervisor.New(rootCtx, zap.NewNop(), func(ctx context.Context) error {
			if err := supervisor.Run(ctx, "canton", w.Run); err != nil {
				return err
			}
			<-ctx.Done()
			return nil
		}, supervisor.WithPropagatePanic)

		select {
		case <-signalingClient.subscribed:
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
	require.NotEqual(t, bridge.GuardianGovernance, publishes.TrustedOwner)
	require.NotEqual(t, bridge.GuardianGovernance, publishes.RogueGovernance)

	trustedPublish := observedPublish{
		EmitterChain:   vaa.ChainIDCanton,
		EmitterAddress: deriveEmitterAddress(bridge.Operator, publishes.TrustedOwner, parseDamlUint64(t, publishes.TrustedEmitterID)),
		Sequence:       parseDamlUint64(t, publishes.TrustedSequence),
		Nonce:          trustedPublishNonce,
		Payload:        decodeHexPayload(t, trustedPublishPayloadHex),
	}
	rogueShadowPublish := observedPublish{
		EmitterChain:   vaa.ChainIDCanton,
		EmitterAddress: deriveEmitterAddress(bridge.Operator, publishes.RogueOwner, 0),
		Sequence:       parseDamlUint64(t, publishes.RogueShadowSequence),
		Nonce:          rogueShadowNonce,
		Payload:        decodeHexPayload(t, rogueShadowPayloadHex),
	}
	require.NotEqual(t, trustedPublish.EmitterAddress, rogueShadowPublish.EmitterAddress)

	deadline := time.After(90 * time.Second)
	for caseIndex, tc := range cases {
		var wantPublishes []observedPublish
		if tc.wantRogueShadow {
			wantPublishes = append(wantPublishes, rogueShadowPublish)
		}
		if tc.wantTrustedPublish {
			wantPublishes = append(wantPublishes, trustedPublish)
		}
		observed := collectObservedPublishes(t, tc.name, msgChannels[caseIndex], len(wantPublishes), deadline)
		assert.ElementsMatchf(t, wantPublishes, observed, "%s", tc.name)
	}

	<-time.After(governanceReaderQuietWindow)
	for caseIndex, tc := range cases {
		select {
		case extra := <-msgChannels[caseIndex]:
			t.Errorf("%s: unexpected extra message: emitter %s sequence %d nonce %d",
				tc.name, extra.EmitterAddress, extra.Sequence, extra.Nonce)
		default:
		}
	}
}
