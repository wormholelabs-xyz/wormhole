//go:build integration

// Upgrade window (canton/README.md section 7.5, proposal U3): a watcher that
// accepts the primary and the next wormhole-core package ids emits publishes
// executed under either version and drops a look-alike package.
//
//	go test -tags integration -run TestCantonWatcherUpgradeWindow ./pkg/watchers/canton -v
package canton

import (
	"context"
	"encoding/json"
	"os"
	"path/filepath"
	"regexp"
	"strings"
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

// Mirrors Test.WatcherUpgradeWindow.
const (
	upgradeWindowNonce      = uint32(43)
	upgradeWindowPayloadHex = "55667788"
	// Appended to the core version: an upgrade-compatible copy with a new package id.
	nextCoreVersionSuffix = ".1"
	nextTestPackageName   = "wormhole-core-test-upgrade-window"
)

// upgradeWindowPublishInput is the input of Test.WatcherUpgradeWindow:integrationUpgradeWindowPublish.
type upgradeWindowPublishInput struct {
	Bridge    realBridge `json:"bridge"`
	OwnerHint string     `json:"ownerHint"`
}

// upgradeWindowPublish is its JSON output.
type upgradeWindowPublish struct {
	Owner     string          `json:"owner"`
	EmitterID json.RawMessage `json:"emitterId"`
	Sequence  json.RawMessage `json:"sequence"`
}

// replaceExactlyOnce fails the test unless pattern matches exactly once.
func replaceExactlyOnce(t *testing.T, text string, pattern *regexp.Regexp, replacement string) string {
	t.Helper()
	require.Lenf(t, pattern.FindAllStringIndex(text, -1), 1, "pattern %s must match exactly once", pattern)
	return pattern.ReplaceAllLiteralString(text, replacement)
}

// copyDamlProject copies a project's daml.yaml (after rewriteDamlYAML) and its daml/ sources.
func copyDamlProject(t *testing.T, sourceDir, destinationDir string, rewriteDamlYAML func(string) string) {
	t.Helper()
	require.NoError(t, os.CopyFS(filepath.Join(destinationDir, "daml"), os.DirFS(filepath.Join(sourceDir, "daml"))))
	damlYAML, err := os.ReadFile(filepath.Join(sourceDir, "daml.yaml"))
	require.NoError(t, err)
	require.NoError(t, os.WriteFile(filepath.Join(destinationDir, "daml.yaml"), []byte(rewriteDamlYAML(string(damlYAML))), 0o600))
}

// buildNextCoreVersion builds an upgrade-compatible copy of wormhole-core with
// a higher version, and a copy of the test package against it. It returns the
// next core DAR and the next test DAR.
func buildNextCoreVersion(t *testing.T, dpm, cantonDir string) (nextCoreDar, nextTestDar string) {
	t.Helper()
	coreDar := coreDarPath(t, cantonDir)
	coreVersion := strings.TrimSuffix(strings.TrimPrefix(filepath.Base(coreDar), "wormhole-core-"), ".dar")
	nextCoreVersion := coreVersion + nextCoreVersionSuffix
	vendoredDarsDir := filepath.Join(cantonDir, "dars") + string(filepath.Separator)

	nextCoreDir := filepath.Join(t.TempDir(), "core")
	copyDamlProject(t, filepath.Join(cantonDir, "core"), nextCoreDir, func(damlYAML string) string {
		damlYAML = replaceExactlyOnce(t, damlYAML, regexp.MustCompile(`(?m)^version: \S+$`), "version: "+nextCoreVersion)
		damlYAML = replaceExactlyOnce(t, damlYAML, regexp.MustCompile(`(?m)^upgrades: \S+$`), "upgrades: "+coreDar)
		return strings.ReplaceAll(damlYAML, "../dars/", vendoredDarsDir)
	})
	runCmd(t, nextCoreDir, dpm, "build")
	nextCoreDar = filepath.Join(nextCoreDir, ".daml", "dist", "wormhole-core-"+nextCoreVersion+".dar")

	nextTestDir := filepath.Join(t.TempDir(), "test")
	copyDamlProject(t, filepath.Join(cantonDir, "test"), nextTestDir, func(damlYAML string) string {
		damlYAML = replaceExactlyOnce(t, damlYAML, regexp.MustCompile(`(?m)^name: wormhole-core-test$`), "name: "+nextTestPackageName)
		damlYAML = replaceExactlyOnce(t, damlYAML, regexp.MustCompile(`(?m)^  - \.\./core/\.daml/dist/\S+\.dar$`), "  - "+nextCoreDar)
		return strings.ReplaceAll(damlYAML, "../dars/", vendoredDarsDir)
	})
	runCmd(t, nextTestDir, dpm, "build")
	nextTestDar = filepath.Join(nextTestDir, ".daml", "dist", nextTestPackageName+"-0.1.0.dar")
	return nextCoreDar, nextTestDar
}

func runUpgradeWindowPublish(t *testing.T, dpm, dar, port string, bridge realBridge, ownerHint string) upgradeWindowPublish {
	t.Helper()
	inputPath := filepath.Join(t.TempDir(), "upgrade-window-input.json")
	inputJSON, err := json.Marshal(upgradeWindowPublishInput{Bridge: bridge, OwnerHint: ownerHint})
	require.NoError(t, err)
	require.NoError(t, os.WriteFile(inputPath, inputJSON, 0o600))
	var published upgradeWindowPublish
	runDamlScript(t, dpm, dar, port, "Test.WatcherUpgradeWindow:integrationUpgradeWindowPublish", inputPath, &published)
	return published
}

func TestCantonWatcherUpgradeWindow(t *testing.T) {
	rootCtx, cancel := context.WithTimeout(context.Background(), 10*time.Minute)
	defer cancel()

	dpm, testDar, port := startSandbox(t, rootCtx)
	ledgerAddr := "localhost:" + port

	cantonDir, err := filepath.Abs("../../../../canton")
	require.NoError(t, err)
	primaryCorePackageID := readDarMainPackageID(t, dpm, coreDarPath(t, cantonDir))
	nextCoreDar, nextTestDar := buildNextCoreVersion(t, dpm, cantonDir)
	nextCorePackageID := readDarMainPackageID(t, dpm, nextCoreDar)
	lookalikeDar := filepath.Join(cantonDir, "test-lookalike", ".daml", "dist", "wormhole-core-lookalike-0.1.0.dar")
	lookalikePackageID := readDarMainPackageID(t, dpm, lookalikeDar)
	require.NotEqual(t, primaryCorePackageID, nextCorePackageID)
	require.NotEqual(t, primaryCorePackageID, lookalikePackageID)
	require.NotEqual(t, nextCorePackageID, lookalikePackageID)

	upgradeWindowPackageIDs, err := cantonclient.ParseAcceptedPackageIDs(primaryCorePackageID, nextCorePackageID)
	require.NoError(t, err)
	primaryOnlyPackageIDs, err := cantonclient.ParseAcceptedPackageIDs(primaryCorePackageID, "")
	require.NoError(t, err)

	nextCoreUnpinnedCountBefore := unpinnedPublishCount(t, nextCorePackageID)
	lookalikeUnpinnedCountBefore := unpinnedPublishCount(t, lookalikePackageID)

	var bridge realBridge
	runDamlScript(t, dpm, testDar, port, "Test.WatcherGovernanceReader:integrationGovernanceReaderSetup", "", &bridge)

	type watcherCase struct {
		name                string
		acceptedPackageIDs  cantonclient.AcceptedPackageIDs
		wantNextCorePublish bool
	}
	cases := []watcherCase{
		{name: "upgrade window accepts the primary and the next core", acceptedPackageIDs: upgradeWindowPackageIDs, wantNextCorePublish: true},
		{name: "primary-only pin drops the next core", acceptedPackageIDs: primaryOnlyPackageIDs},
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

		w := NewWatcher(ledgerAddr, tc.acceptedPackageIDs, bridge.GuardianGovernance, AuthConfig{}, true, msgC, obsvReqC)
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

	// The next core is uploaded only by the second run, so the first run executes under the primary core.
	primaryCorePublish := runUpgradeWindowPublish(t, dpm, testDar, port, bridge, "PrimaryCoreOwner")
	nextCorePublish := runUpgradeWindowPublish(t, dpm, nextTestDar, port, bridge, "NextCoreOwner")
	require.NotEqual(t, primaryCorePublish.Owner, nextCorePublish.Owner)

	lookalikeInputPath := filepath.Join(t.TempDir(), "lookalike-target.json")
	lookalikeJSON, err := json.Marshal(lookalikeTarget{
		Operator:           bridge.Operator,
		GuardianGovernance: bridge.GuardianGovernance,
		VictimOwner:        primaryCorePublish.Owner,
		VictimEmitterID:    primaryCorePublish.EmitterID,
		VictimSequence:     primaryCorePublish.Sequence,
	})
	require.NoError(t, err)
	require.NoError(t, os.WriteFile(lookalikeInputPath, lookalikeJSON, 0o600))
	var forger string
	runDamlScript(t, dpm, lookalikeDar, port, "Lookalike.Publish:integrationLookalikePublish", lookalikeInputPath, &forger)

	toExpectedPublish := func(published upgradeWindowPublish) observedPublish {
		return observedPublish{
			EmitterChain:   vaa.ChainIDCanton,
			EmitterAddress: deriveEmitterAddress(bridge.Operator, published.Owner, parseDamlUint64(t, published.EmitterID)),
			Sequence:       parseDamlUint64(t, published.Sequence),
			Nonce:          upgradeWindowNonce,
			Payload:        decodeHexPayload(t, upgradeWindowPayloadHex),
		}
	}
	wantPrimaryCorePublish := toExpectedPublish(primaryCorePublish)
	wantNextCorePublish := toExpectedPublish(nextCorePublish)
	require.NotEqual(t, wantPrimaryCorePublish.EmitterAddress, wantNextCorePublish.EmitterAddress)

	deadline := time.After(90 * time.Second)
	var nextCorePublishUpdateID cantonclient.UpdateID
	for caseIndex, tc := range cases {
		wantPublishes := []observedPublish{wantPrimaryCorePublish}
		if tc.wantNextCorePublish {
			wantPublishes = append(wantPublishes, wantNextCorePublish)
		}
		collected := collectMessagePublications(t, tc.name, msgChannels[caseIndex], len(wantPublishes), deadline)
		observed := make([]observedPublish, 0, len(collected))
		for _, msg := range collected {
			observed = append(observed, toObservedPublish(msg))
			if msg.EmitterAddress == wantNextCorePublish.EmitterAddress {
				updateID, err := cantonclient.UpdateIDFromTxID(msg.TxID)
				require.NoError(t, err)
				nextCorePublishUpdateID = updateID
			}
		}
		assert.ElementsMatchf(t, wantPublishes, observed, "%s", tc.name)
	}
	require.False(t, nextCorePublishUpdateID.IsZero(), "the upgrade-window watcher never emitted the next-core publish")

	// Both streams received the next-core publish; only the window emitted it.
	for caseIndex, tc := range cases {
		require.Eventuallyf(t, func() bool {
			return streamRecorders[caseIndex].hasDeliveredUpdateID(nextCorePublishUpdateID)
		}, 30*time.Second, 100*time.Millisecond, "%s: stream never delivered update %s", tc.name, nextCorePublishUpdateID)
	}
	require.Eventually(t, func() bool {
		return unpinnedPublishCount(t, lookalikePackageID) == lookalikeUnpinnedCountBefore+float64(len(cases))
	}, 30*time.Second, 100*time.Millisecond, "every watcher must count the look-alike publish once")
	assert.Never(t, func() bool {
		for _, msgC := range msgChannels {
			if len(msgC) > 0 {
				return true
			}
		}
		return false
	}, unexpectedEmissionWindow, 50*time.Millisecond, "a watcher emitted a message after its expected ones")
	assert.Equal(t, nextCoreUnpinnedCountBefore+1, unpinnedPublishCount(t, nextCorePackageID),
		"only the primary-only watcher must count the next-core publish")

	// Reobservation uses the same accepted package ids as the stream.
	for caseIndex, tc := range cases {
		reobserved := reobserveAndCollect(t, obsvReqChannels[caseIndex], msgChannels[caseIndex], nextCorePublishUpdateID.TxID(), reobservationWindow)
		var wantReobserved []observedPublish
		if tc.wantNextCorePublish {
			wantReobserved = []observedPublish{wantNextCorePublish}
		}
		assert.Equalf(t, wantReobserved, toObservedPublishes(reobserved), "%s: next-core publish reobservation", tc.name)
	}
	assert.Equal(t, nextCoreUnpinnedCountBefore+2, unpinnedPublishCount(t, nextCorePackageID),
		"primary-only reobservation must count the next-core publish again")
}
