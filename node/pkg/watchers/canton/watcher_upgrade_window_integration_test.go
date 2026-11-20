//go:build integration

// Upgrade window (canton/README.md section 7.5, proposal U3): a watcher with a
// current and a retiring wormhole-core package id emits publishes executed
// under either version while the retiring deadline is in the future, including
// a current-core publish on an emitter the retiring core created and a
// retiring-core publish on the emitter the current core wrote last, and drops
// a look-alike package. After the deadline it drops and counts retiring-core
// publishes. Single-id watchers drop and count the other version.
//
//	go test -tags integration -run TestCantonWatcherUpgradeWindow ./pkg/watchers/canton -v
package canton

import (
	"context"
	"encoding/json"
	"os"
	"path/filepath"
	"regexp"
	"strconv"
	"strings"
	"testing"
	"time"

	"github.com/certusone/wormhole/node/pkg/cantonclient"
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

// Mirrors Test.WatcherUpgradeWindow.
const (
	upgradeWindowNonce      = uint32(43)
	upgradeWindowPayloadHex = "55667788"
	currentTestPackageName  = "wormhole-core-test-upgrade-window"
)

// upgradeWindowPublishInput is the input of Test.WatcherUpgradeWindow:integrationUpgradeWindowPublish.
type upgradeWindowPublishInput struct {
	Bridge    realBridge `json:"bridge"`
	OwnerHint string     `json:"ownerHint"`
}

// existingEmitterPublishInput is the input of Test.WatcherUpgradeWindow:integrationPublishOnExistingEmitter.
type existingEmitterPublishInput struct {
	Bridge                 realBridge `json:"bridge"`
	Owner                  string     `json:"owner"`
	PreferredCorePackageID *string    `json:"preferredCorePackageId"`
}

// upgradeWindowPublish is the JSON output of both fixtures.
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

// nextPatchVersion increments PATCH of a MAJOR.MINOR.PATCH version.
func nextPatchVersion(t *testing.T, version string) string {
	t.Helper()
	versionParts := strings.Split(version, ".")
	require.Lenf(t, versionParts, 3, "core version %q must be MAJOR.MINOR.PATCH", version)
	patch, err := strconv.ParseUint(versionParts[2], 10, 32)
	require.NoError(t, err)
	return versionParts[0] + "." + versionParts[1] + "." + strconv.FormatUint(patch+1, 10)
}

// buildCurrentCoreVersion builds an upgrade-compatible copy of wormhole-core with
// a higher version, and a copy of the test package against it. It returns the
// current core DAR and the current test DAR.
func buildCurrentCoreVersion(t *testing.T, dpm, cantonDir string) (retiringCoreVersion, currentCoreVersion, currentCoreDar, currentTestDar string) {
	t.Helper()
	coreDar := coreDarPath(t, cantonDir)
	retiringCoreVersion = strings.TrimSuffix(strings.TrimPrefix(filepath.Base(coreDar), "wormhole-core-"), ".dar")
	currentCoreVersion = nextPatchVersion(t, retiringCoreVersion)
	vendoredDarsDir := filepath.Join(cantonDir, "dars") + string(filepath.Separator)

	currentCoreDir := filepath.Join(t.TempDir(), "core")
	copyDamlProject(t, filepath.Join(cantonDir, "core"), currentCoreDir, func(damlYAML string) string {
		damlYAML = regexp.MustCompile(`(?m)^upgrades: \S+\n`).ReplaceAllString(damlYAML, "")
		damlYAML = replaceExactlyOnce(t, damlYAML, regexp.MustCompile(`(?m)^version: \S+$`),
			"version: "+currentCoreVersion+"\nupgrades: "+coreDar)
		return strings.ReplaceAll(damlYAML, "../dars/", vendoredDarsDir)
	})
	runCmd(t, currentCoreDir, dpm, "build")
	currentCoreDar = filepath.Join(currentCoreDir, ".daml", "dist", "wormhole-core-"+currentCoreVersion+".dar")

	nextTestDir := filepath.Join(t.TempDir(), "test")
	copyDamlProject(t, filepath.Join(cantonDir, "test"), nextTestDir, func(damlYAML string) string {
		damlYAML = replaceExactlyOnce(t, damlYAML, regexp.MustCompile(`(?m)^name: wormhole-core-test$`), "name: "+currentTestPackageName)
		damlYAML = replaceExactlyOnce(t, damlYAML, regexp.MustCompile(`(?m)^  - \.\./core/\.daml/dist/\S+\.dar$`), "  - "+currentCoreDar)
		return strings.ReplaceAll(damlYAML, "../dars/", vendoredDarsDir)
	})
	runCmd(t, nextTestDir, dpm, "build")
	currentTestDar = filepath.Join(nextTestDir, ".daml", "dist", currentTestPackageName+"-0.1.0.dar")
	return retiringCoreVersion, currentCoreVersion, currentCoreDar, currentTestDar
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

// runPublishOnExistingEmitter publishes on the owner's existing emitter, executed by the core version dar depends on.
// A nil preferredCorePackageID lets Daml Script pick the newest vetted core.
func runPublishOnExistingEmitter(t *testing.T, dpm, dar, port string, bridge realBridge, owner string, preferredCorePackageID *string) upgradeWindowPublish {
	t.Helper()
	inputPath := filepath.Join(t.TempDir(), "existing-emitter-input.json")
	inputJSON, err := json.Marshal(existingEmitterPublishInput{Bridge: bridge, Owner: owner, PreferredCorePackageID: preferredCorePackageID})
	require.NoError(t, err)
	require.NoError(t, os.WriteFile(inputPath, inputJSON, 0o600))
	var published upgradeWindowPublish
	runDamlScript(t, dpm, dar, port, "Test.WatcherUpgradeWindow:integrationPublishOnExistingEmitter", inputPath, &published)
	return published
}

func TestCantonWatcherUpgradeWindow(t *testing.T) {
	rootCtx, cancel := context.WithTimeout(context.Background(), 10*time.Minute)
	defer cancel()

	dpm, testDar, port := startSandbox(t, rootCtx)
	ledgerAddr := "localhost:" + port

	cantonDir, err := filepath.Abs("../../../../canton")
	require.NoError(t, err)
	retiringCorePackageID := readDarMainPackageID(t, dpm, coreDarPath(t, cantonDir))
	retiringCoreVersion, currentCoreVersion, currentCoreDar, currentTestDar := buildCurrentCoreVersion(t, dpm, cantonDir)
	currentCorePackageID := readDarMainPackageID(t, dpm, currentCoreDar)
	lookalikeDar := filepath.Join(cantonDir, "test-lookalike", ".daml", "dist", "wormhole-core-lookalike-0.1.0.dar")
	lookalikePackageID := readDarMainPackageID(t, dpm, lookalikeDar)
	require.NotEqual(t, retiringCorePackageID, currentCorePackageID)
	// The core built from this tree is the one guardiand approves.
	approvedCorePackages := cantonclient.DefaultApprovedCorePackages()
	builtCoreAccepted, err := cantonclient.ParseAcceptedPackageIDs(cantonclient.CurrentPackageConfig{PackageIDHex: retiringCorePackageID, Version: retiringCoreVersion}, cantonclient.RetiringPackageConfig{}, time.Now())
	require.NoError(t, err)
	require.NoError(t, approvedCorePackages.CheckAcceptedPackageIDsApproved(builtCoreAccepted),
		"update node/pkg/cantonclient/approved_core_packages.json for the built core")
	require.NotEqual(t, retiringCorePackageID, lookalikePackageID)
	require.NotEqual(t, currentCorePackageID, lookalikePackageID)

	startupWallClock := time.Now().UTC()
	upgradeWindowPackageIDs, err := cantonclient.ParseAcceptedPackageIDs(cantonclient.CurrentPackageConfig{PackageIDHex: currentCorePackageID, Version: currentCoreVersion}, cantonclient.RetiringPackageConfig{
		PackageIDHex:     retiringCorePackageID,
		AcceptedUntilUTC: startupWallClock.Add(time.Hour).Format(cantonclient.RetiringPackageDeadlineLayout),
		Version:          retiringCoreVersion,
	}, startupWallClock)
	require.NoError(t, err)
	expiredWindowPackageIDs, err := cantonclient.ParseAcceptedPackageIDs(cantonclient.CurrentPackageConfig{PackageIDHex: currentCorePackageID, Version: currentCoreVersion}, cantonclient.RetiringPackageConfig{
		PackageIDHex:     retiringCorePackageID,
		AcceptedUntilUTC: startupWallClock.Add(-time.Minute).Format(cantonclient.RetiringPackageDeadlineLayout),
		Version:          retiringCoreVersion,
	}, startupWallClock)
	require.NoError(t, err)
	retiringOnlyPackageIDs, err := cantonclient.ParseAcceptedPackageIDs(cantonclient.CurrentPackageConfig{PackageIDHex: retiringCorePackageID, Version: retiringCoreVersion}, cantonclient.RetiringPackageConfig{}, startupWallClock)
	require.NoError(t, err)
	currentOnlyPackageIDs, err := cantonclient.ParseAcceptedPackageIDs(cantonclient.CurrentPackageConfig{PackageIDHex: currentCorePackageID, Version: currentCoreVersion}, cantonclient.RetiringPackageConfig{}, startupWallClock)
	require.NoError(t, err)

	retiringAfterDeadlineCountBefore := retiringPackagePublishAfterDeadlineCount(t)
	retiringCoreUnpinnedCountBefore := unpinnedPublishCount(t, retiringCorePackageID)
	currentCoreUnpinnedCountBefore := unpinnedPublishCount(t, currentCorePackageID)
	lookalikeUnpinnedCountBefore := unpinnedPublishCount(t, lookalikePackageID)

	var bridge realBridge
	runDamlScript(t, dpm, testDar, port, "Test.WatcherGovernanceReader:integrationGovernanceReaderSetup", "", &bridge)

	type watcherCase struct {
		name                      string
		acceptedPackageIDs        cantonclient.AcceptedPackageIDs
		wantRetiringCorePublishes bool
		wantCurrentCorePublishes  bool
	}
	const upgradeWindowCaseIndex = 0
	cases := []watcherCase{
		{name: "upgrade window accepts the current and the retiring core", acceptedPackageIDs: upgradeWindowPackageIDs, wantRetiringCorePublishes: true, wantCurrentCorePublishes: true},
		{name: "retiring-only pin drops the current core", acceptedPackageIDs: retiringOnlyPackageIDs, wantRetiringCorePublishes: true},
		{name: "current-only pin drops the retiring core", acceptedPackageIDs: currentOnlyPackageIDs, wantCurrentCorePublishes: true},
		{name: "window after the deadline drops the retiring core", acceptedPackageIDs: expiredWindowPackageIDs, wantCurrentCorePublishes: true},
	}
	require.Equal(t, upgradeWindowPackageIDs, cases[upgradeWindowCaseIndex].acceptedPackageIDs)

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

		w, err := NewWatcher(ledgerAddr, tc.acceptedPackageIDs, readScopeOfParty(t, bridge.GuardianGovernance), AuthConfig{}, true, msgC, obsvReqC)
		require.NoError(t, err)
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

	// The current core is uploaded only by the second run, so the first run executes under the retiring core.
	retiringCorePublish := runUpgradeWindowPublish(t, dpm, testDar, port, bridge, "RetiringCoreOwner")
	currentCorePublish := runUpgradeWindowPublish(t, dpm, currentTestDar, port, bridge, "CurrentCoreOwner")
	require.NotEqual(t, retiringCorePublish.Owner, currentCorePublish.Owner)
	// Production case: once the current core is vetted, existing emitters publish under it.
	currentCoreOnRetiringEmitterPublish := runPublishOnExistingEmitter(t, dpm, currentTestDar, port, bridge, retiringCorePublish.Owner, nil)
	require.Equal(t, retiringCorePublish.Owner, currentCoreOnRetiringEmitterPublish.Owner)
	// The retiring core publishes again on the emitter the current core wrote last.
	retiringCoreOnCurrentWrittenEmitterPublish := runPublishOnExistingEmitter(t, dpm, testDar, port, bridge, retiringCorePublish.Owner, &retiringCorePackageID)
	require.Equal(t, retiringCorePublish.Owner, retiringCoreOnCurrentWrittenEmitterPublish.Owner)

	lookalikeInputPath := filepath.Join(t.TempDir(), "lookalike-target.json")
	lookalikeJSON, err := json.Marshal(lookalikeTarget{
		GuardianGovernance: bridge.GuardianGovernance,
		VictimOwner:        retiringCorePublish.Owner,
		VictimEmitterID:    retiringCorePublish.EmitterID,
		VictimSequence:     retiringCorePublish.Sequence,
	})
	require.NoError(t, err)
	require.NoError(t, os.WriteFile(lookalikeInputPath, lookalikeJSON, 0o600))
	var forger string
	runDamlScript(t, dpm, lookalikeDar, port, "Lookalike.Publish:integrationLookalikePublish", lookalikeInputPath, &forger)

	toExpectedPublish := func(published upgradeWindowPublish) observedPublish {
		return observedPublish{
			EmitterChain:   vaa.ChainIDCanton,
			EmitterAddress: DeriveEmitterAddress(bridge.GuardianGovernance, published.Owner, parseDamlUint64(t, published.EmitterID)),
			Sequence:       parseDamlUint64(t, published.Sequence),
			Nonce:          upgradeWindowNonce,
			Payload:        decodeHexPayload(t, upgradeWindowPayloadHex),
		}
	}
	wantRetiringCorePublish := toExpectedPublish(retiringCorePublish)
	wantCurrentCorePublish := toExpectedPublish(currentCorePublish)
	wantCurrentCoreOnRetiringEmitterPublish := toExpectedPublish(currentCoreOnRetiringEmitterPublish)
	require.NotEqual(t, wantRetiringCorePublish.EmitterAddress, wantCurrentCorePublish.EmitterAddress)
	require.Equal(t, wantRetiringCorePublish.EmitterAddress, wantCurrentCoreOnRetiringEmitterPublish.EmitterAddress)
	require.Equal(t, wantRetiringCorePublish.Sequence+1, wantCurrentCoreOnRetiringEmitterPublish.Sequence)
	wantRetiringCoreOnCurrentWrittenEmitterPublish := toExpectedPublish(retiringCoreOnCurrentWrittenEmitterPublish)
	require.Equal(t, wantRetiringCorePublish.EmitterAddress, wantRetiringCoreOnCurrentWrittenEmitterPublish.EmitterAddress)
	require.Equal(t, wantRetiringCorePublish.Sequence+2, wantRetiringCoreOnCurrentWrittenEmitterPublish.Sequence)

	deadline := time.After(90 * time.Second)
	type emitterSequence struct {
		emitterAddress vaa.Address
		sequence       uint64
	}
	updateIDsByEmitterSequence := make(map[emitterSequence]cantonclient.UpdateID, 4)
	updateIDOf := func(publish observedPublish) cantonclient.UpdateID {
		return updateIDsByEmitterSequence[emitterSequence{emitterAddress: publish.EmitterAddress, sequence: publish.Sequence}]
	}
	for caseIndex, tc := range cases {
		var wantPublishes []observedPublish
		if tc.wantRetiringCorePublishes {
			wantPublishes = append(wantPublishes, wantRetiringCorePublish, wantRetiringCoreOnCurrentWrittenEmitterPublish)
		}
		if tc.wantCurrentCorePublishes {
			wantPublishes = append(wantPublishes, wantCurrentCorePublish, wantCurrentCoreOnRetiringEmitterPublish)
		}
		collected := collectMessagePublications(t, tc.name, msgChannels[caseIndex], len(wantPublishes), deadline)
		observed := make([]observedPublish, 0, len(collected))
		for _, msg := range collected {
			observedMessage := toObservedPublish(msg)
			observed = append(observed, observedMessage)
			if caseIndex == upgradeWindowCaseIndex {
				updateID, err := cantonclient.UpdateIDFromTxID(msg.TxID)
				require.NoError(t, err)
				updateIDsByEmitterSequence[emitterSequence{emitterAddress: msg.EmitterAddress, sequence: msg.Sequence}] = updateID
			}
		}
		assert.ElementsMatchf(t, wantPublishes, observed, "%s", tc.name)
	}
	retiringCorePublishUpdateID := updateIDOf(wantRetiringCorePublish)
	currentCorePublishUpdateID := updateIDOf(wantCurrentCorePublish)
	currentCoreOnRetiringEmitterUpdateID := updateIDOf(wantCurrentCoreOnRetiringEmitterPublish)
	retiringCoreOnCurrentWrittenEmitterUpdateID := updateIDOf(wantRetiringCoreOnCurrentWrittenEmitterPublish)
	require.False(t, retiringCorePublishUpdateID.IsZero(), "the upgrade-window watcher never emitted the retiring-core publish")
	require.False(t, currentCorePublishUpdateID.IsZero(), "the upgrade-window watcher never emitted the current-core publish")
	require.False(t, currentCoreOnRetiringEmitterUpdateID.IsZero(), "the upgrade-window watcher never emitted the current-core publish on the retiring-core emitter")
	require.False(t, retiringCoreOnCurrentWrittenEmitterUpdateID.IsZero(), "the upgrade-window watcher never emitted the retiring-core publish on the emitter the current core wrote last")

	// The upgrade-window watcher emitted each publish under the package id that executed it.
	executingPackageIDByUpdateID := map[cantonclient.UpdateID]string{
		retiringCorePublishUpdateID:                 retiringCorePackageID,
		currentCorePublishUpdateID:                  currentCorePackageID,
		currentCoreOnRetiringEmitterUpdateID:        currentCorePackageID,
		retiringCoreOnCurrentWrittenEmitterUpdateID: retiringCorePackageID,
	}
	for updateID, wantExecutingPackageID := range executingPackageIDByUpdateID {
		assert.Equalf(t, []string{wantExecutingPackageID}, streamRecorders[upgradeWindowCaseIndex].publishExecutingPackageIDs(updateID),
			"executing package id of update %s", updateID)
	}

	// Every stream received every publish; the package filter decides what is emitted.
	for caseIndex, tc := range cases {
		for updateID := range executingPackageIDByUpdateID {
			require.Eventuallyf(t, func() bool {
				return streamRecorders[caseIndex].hasDeliveredUpdateID(updateID)
			}, 30*time.Second, 100*time.Millisecond, "%s: stream never delivered update %s", tc.name, updateID)
		}
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
	assert.Equal(t, currentCoreUnpinnedCountBefore+2, unpinnedPublishCount(t, currentCorePackageID),
		"only the retiring-only watcher must count the two current-core publishes")
	assert.Equal(t, retiringCoreUnpinnedCountBefore+2, unpinnedPublishCount(t, retiringCorePackageID),
		"only the current-only watcher must count the two retiring-core publishes")
	assert.Equal(t, retiringAfterDeadlineCountBefore+2, retiringPackagePublishAfterDeadlineCount(t),
		"only the expired-window watcher must count the two retiring-core publishes after the deadline")

	// Reobservation uses the same accepted package ids as the stream.
	for caseIndex, tc := range cases {
		reobserved := reobserveAndCollect(t, obsvReqChannels[caseIndex], msgChannels[caseIndex], currentCoreOnRetiringEmitterUpdateID.TxID(), reobservationWindow)
		var wantReobserved []observedPublish
		if tc.wantCurrentCorePublishes {
			wantReobserved = []observedPublish{wantCurrentCoreOnRetiringEmitterPublish}
		}
		assert.Equalf(t, wantReobserved, toObservedPublishes(reobserved), "%s: current-core publish on the retiring-core emitter reobservation", tc.name)
	}
	assert.Equal(t, currentCoreUnpinnedCountBefore+3, unpinnedPublishCount(t, currentCorePackageID),
		"retiring-only reobservation must count the current-core publish again")
	assert.Equal(t, retiringCoreUnpinnedCountBefore+2, unpinnedPublishCount(t, retiringCorePackageID),
		"reobserving a current-core publish must not count the retiring core")

	// After the deadline, reobservation of a retiring-core publish follows its record time.
	for caseIndex, tc := range cases {
		reobserved := reobserveAndCollect(t, obsvReqChannels[caseIndex], msgChannels[caseIndex], retiringCorePublishUpdateID.TxID(), reobservationWindow)
		var wantReobserved []observedPublish
		if tc.wantRetiringCorePublishes {
			wantReobserved = []observedPublish{wantRetiringCorePublish}
		}
		assert.Equalf(t, wantReobserved, toObservedPublishes(reobserved), "%s: retiring-core publish reobservation", tc.name)
	}
	assert.Equal(t, retiringAfterDeadlineCountBefore+3, retiringPackagePublishAfterDeadlineCount(t),
		"the expired-window watcher must count the reobserved retiring-core publish again")
	assert.Equal(t, retiringCoreUnpinnedCountBefore+3, unpinnedPublishCount(t, retiringCorePackageID),
		"the current-only watcher must count the reobserved retiring-core publish again")
}

func retiringPackagePublishAfterDeadlineCount(t *testing.T) float64 {
	t.Helper()
	metricFamilies, err := prometheus.DefaultGatherer.Gather()
	require.NoError(t, err)
	for _, metricFamily := range metricFamilies {
		if metricFamily.GetName() == "wormhole_canton_retiring_package_publish_after_deadline_total" {
			return metricFamily.GetMetric()[0].GetCounter().GetValue()
		}
	}
	return 0
}
