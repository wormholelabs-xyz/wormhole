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
	"regexp"
	"slices"
	"strconv"
	"strings"
	"sync"
	"testing"
	"time"

	"github.com/certusone/wormhole/node/pkg/cantonclient"
	apiv2 "github.com/certusone/wormhole/node/pkg/cantonclient/proto/gen/com/daml/ledger/api/v2"
	"github.com/certusone/wormhole/node/pkg/common"
	gossipv1 "github.com/certusone/wormhole/node/pkg/proto/gossip/v1"
	"github.com/certusone/wormhole/node/pkg/supervisor"
	"github.com/prometheus/client_golang/prometheus/testutil"
	"github.com/stretchr/testify/assert"
	"github.com/stretchr/testify/require"
	"github.com/wormhole-foundation/wormhole/sdk/vaa"
	"go.uber.org/zap"
	"google.golang.org/grpc"
)

// Mirrors Test.WatcherGovernanceReader.
const (
	trustedPublishNonce      = uint32(42)
	trustedPublishPayloadHex = "11223344"
	rogueShadowNonce         = uint32(66)
	rogueShadowPayloadHex    = "deadbeef"
	// An Emitter of the rogue registry with the real guardianGovernance as owner.
	ggOwnedForeignRegistrarNonce      = uint32(77)
	ggOwnedForeignRegistrarPayloadHex = "cafe0077"

	// Bounds the in-process hop from a recorded stream update id to msgC.
	unexpectedEmissionWindow = 2 * time.Second
	// Bounds one GetUpdateById round trip plus the hop to msgC.
	reobservationWindow = 5 * time.Second
)

var getUpdatesFullMethod = "/" + apiv2.UpdateService_ServiceDesc.ServiceName + "/GetUpdates"

// realBridge is the JSON output of Test.WatcherGovernanceReader:integrationGovernanceReaderSetup.
type realBridge struct {
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
	RogueEmitterID      json.RawMessage `json:"rogueEmitterId"`
	RogueShadowSequence json.RawMessage `json:"rogueShadowSequence"`

	GgOwnedForeignRegistrarEmitterID json.RawMessage `json:"ggOwnedForeignRegistrarEmitterId"`
	GgOwnedForeignRegistrarSequence  json.RawMessage `json:"ggOwnedForeignRegistrarSequence"`
}

// observedPublish is the subset of a MessagePublication the cases compare.
type observedPublish struct {
	EmitterChain   vaa.ChainID
	EmitterAddress vaa.Address
	Sequence       uint64
	Nonce          uint32
	Payload        []byte
}

// updateStreamRecorder is a gRPC stream interceptor on the watcher's own
// Ledger API connection. It closes subscribed when GetUpdates opens and records
// the update id of every transaction the stream delivers, and the executing
// package ids of its PublishMessage exercises, before the client-side template
// and package filter.
type updateStreamRecorder struct {
	subscribed    chan struct{}
	subscribeOnce sync.Once

	mutex sync.Mutex
	// publishExecutingPackageIDsByUpdateID maps every delivered update id to the executing package ids of its PublishMessage exercises.
	publishExecutingPackageIDsByUpdateID map[string][]string
}

const maxRecordedUpdateIDs = 4096

func newUpdateStreamRecorder() *updateStreamRecorder {
	return &updateStreamRecorder{subscribed: make(chan struct{}), publishExecutingPackageIDsByUpdateID: make(map[string][]string)}
}

func (r *updateStreamRecorder) interceptGetUpdates(ctx context.Context, desc *grpc.StreamDesc, conn *grpc.ClientConn, method string, streamer grpc.Streamer, opts ...grpc.CallOption) (grpc.ClientStream, error) {
	stream, err := streamer(ctx, desc, conn, method, opts...)
	if err != nil || method != getUpdatesFullMethod {
		return stream, err
	}
	r.subscribeOnce.Do(func() { close(r.subscribed) })
	return &updateIDRecordingStream{ClientStream: stream, recorder: r}, nil
}

func (r *updateStreamRecorder) recordDeliveredTransaction(transaction *apiv2.Transaction) {
	var publishExecutingPackageIDs []string
	for _, event := range transaction.GetEvents() {
		exercised := event.GetExercised()
		if exercised == nil || exercised.GetChoice() != publishMessageChoice {
			continue
		}
		publishExecutingPackageIDs = append(publishExecutingPackageIDs, exercised.GetTemplateId().GetPackageId())
	}
	r.mutex.Lock()
	defer r.mutex.Unlock()
	if len(r.publishExecutingPackageIDsByUpdateID) >= maxRecordedUpdateIDs {
		panic("updateStreamRecorder: update id capacity exceeded")
	}
	r.publishExecutingPackageIDsByUpdateID[transaction.GetUpdateId()] = publishExecutingPackageIDs
}

func (r *updateStreamRecorder) hasDeliveredUpdateID(updateID cantonclient.UpdateID) bool {
	r.mutex.Lock()
	defer r.mutex.Unlock()
	_, delivered := r.publishExecutingPackageIDsByUpdateID[updateID.String()]
	return delivered
}

// publishExecutingPackageIDs returns the executing package ids of the update's PublishMessage exercises, in event order.
func (r *updateStreamRecorder) publishExecutingPackageIDs(updateID cantonclient.UpdateID) []string {
	r.mutex.Lock()
	defer r.mutex.Unlock()
	return slices.Clone(r.publishExecutingPackageIDsByUpdateID[updateID.String()])
}

type updateIDRecordingStream struct {
	grpc.ClientStream
	recorder *updateStreamRecorder
}

func (s *updateIDRecordingStream) RecvMsg(message any) error {
	if err := s.ClientStream.RecvMsg(message); err != nil {
		return err
	}
	if response, isUpdatesResponse := message.(*apiv2.GetUpdatesResponse); isUpdatesResponse {
		if transaction := response.GetTransaction(); transaction != nil {
			s.recorder.recordDeliveredTransaction(transaction)
		}
	}
	return nil
}

// reobserveAndCollect sends a reobservation for txID and returns what the watcher emits within the window.
func reobserveAndCollect(t *testing.T, obsvReqC chan<- *gossipv1.ObservationRequest, msgC <-chan *common.MessagePublication, txID []byte, window time.Duration) []*common.MessagePublication {
	t.Helper()
	obsvReqC <- &gossipv1.ObservationRequest{ChainId: uint32(vaa.ChainIDCanton), TxHash: txID}
	var emitted []*common.MessagePublication
	deadline := time.After(window)
	for {
		select {
		case msg := <-msgC:
			require.True(t, msg.IsReobservation, "reobservation emission must be flagged")
			emitted = append(emitted, msg)
		case <-deadline:
			return emitted
		}
	}
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

var damlProjectVersionPattern = regexp.MustCompile(`(?m)^version: (\S+)$`)

// coreDarPath returns the wormhole-core DAR built from canton/core/daml.yaml's version.
func coreDarPath(t *testing.T, cantonDir string) string {
	t.Helper()
	coreDir := filepath.Join(cantonDir, "core")
	damlYAML, err := os.ReadFile(filepath.Join(coreDir, "daml.yaml"))
	require.NoError(t, err)
	versionMatches := damlProjectVersionPattern.FindAllSubmatch(damlYAML, -1)
	require.Len(t, versionMatches, 1, "canton/core/daml.yaml must declare exactly one version")
	return filepath.Join(coreDir, ".daml", "dist", "wormhole-core-"+string(versionMatches[0][1])+".dar")
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

// acceptedPackageIDsOrAny pins currentPackageID, or matches any package when it is empty.
func acceptedPackageIDsOrAny(t *testing.T, currentPackageID string) cantonclient.AcceptedPackageIDs {
	t.Helper()
	if currentPackageID == "" {
		return cantonclient.AcceptAnyPackageID()
	}
	accepted, err := cantonclient.ParseAcceptedPackageIDs(cantonclient.CurrentPackageConfig{PackageIDHex: currentPackageID}, cantonclient.RetiringPackageConfig{}, time.Now())
	require.NoError(t, err)
	return accepted
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

func toObservedPublish(msg *common.MessagePublication) observedPublish {
	return observedPublish{
		EmitterChain:   msg.EmitterChain,
		EmitterAddress: msg.EmitterAddress,
		Sequence:       msg.Sequence,
		Nonce:          msg.Nonce,
		Payload:        msg.Payload,
	}
}

func collectMessagePublications(t *testing.T, caseName string, msgC <-chan *common.MessagePublication, wantCount int, deadline <-chan time.Time) []*common.MessagePublication {
	t.Helper()
	collected := make([]*common.MessagePublication, 0, wantCount)
	for len(collected) < wantCount {
		select {
		case msg := <-msgC:
			require.Falsef(t, msg.IsReobservation, "%s: live observation flagged as reobservation", caseName)
			collected = append(collected, msg)
		case <-deadline:
			t.Fatalf("%s: received %d of %d expected messages before the deadline", caseName, len(collected), wantCount)
		}
	}
	return collected
}

func TestCantonWatcherGovernanceReaderIntegration(t *testing.T) {
	rootCtx, cancel := context.WithTimeout(context.Background(), 8*time.Minute)
	defer cancel()

	dpm, testDar, port := startSandbox(t, rootCtx)
	ledgerAddr := "localhost:" + port

	cantonDir, err := filepath.Abs("../../../../canton")
	require.NoError(t, err)
	corePackageID := readDarMainPackageID(t, dpm, coreDarPath(t, cantonDir))
	testPackageID := readDarMainPackageID(t, dpm, testDar)
	require.NotEqual(t, corePackageID, testPackageID)

	var bridge realBridge
	runDamlScript(t, dpm, testDar, port, "Test.WatcherGovernanceReader:integrationGovernanceReaderSetup", "", &bridge)
	require.Contains(t, bridge.GuardianGovernance, "::", "guardianGovernance must be a full party id")

	type watcherCase struct {
		name                        string
		readScope                   cantonclient.ReadScope
		packageID                   string
		wantRogueShadow             bool
		wantGgOwnedForeignRegistrar bool
		wantTrustedPublish          bool
	}
	cases := []watcherCase{
		{
			// The gg-owned foreign-registrar publish passes the signatory check; the registrar pin drops it.
			name:               "governance reader with core package emits only the trusted publish",
			readScope:          readScopeOfParty(t, bridge.GuardianGovernance),
			packageID:          corePackageID,
			wantTrustedPublish: true,
		},
		{
			name:      "governance reader with wrong package emits nothing",
			readScope: readScopeOfParty(t, bridge.GuardianGovernance),
			packageID: testPackageID,
		},
		{
			// Control: proves the rogue shadow and gg-owned foreign-registrar publishes are on the ledger stream.
			name:                        "wildcard reader with core package also emits the rogue shadow and gg-owned foreign-registrar publishes",
			readScope:                   anyPartyReadScope(t),
			packageID:                   corePackageID,
			wantRogueShadow:             true,
			wantGgOwnedForeignRegistrar: true,
			wantTrustedPublish:          true,
		},
	}
	const governanceReaderCoreCaseIndex = 0

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

		// Run builds the gRPC client from NewWatcher's readScope.
		w, err := NewWatcher(ledgerAddr, acceptedPackageIDsOrAny(t, tc.packageID), tc.readScope, AuthConfig{}, true, msgC, obsvReqC)
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

	bridgeInputPath := filepath.Join(t.TempDir(), "bridge.json")
	bridgeJSON, err := json.Marshal(bridge)
	require.NoError(t, err)
	require.NoError(t, os.WriteFile(bridgeInputPath, bridgeJSON, 0o600))
	foreignRegistrarDropsBeforePublish := testutil.ToFloat64(cantonForeignRegistrarMessagesDropped)
	var publishes governanceReaderPublishes
	runDamlScript(t, dpm, testDar, port, "Test.WatcherGovernanceReader:integrationGovernanceReaderPublish", bridgeInputPath, &publishes)
	require.NotEqual(t, bridge.GuardianGovernance, publishes.TrustedOwner)
	require.NotEqual(t, bridge.GuardianGovernance, publishes.RogueGovernance)

	trustedPublish := observedPublish{
		EmitterChain:   vaa.ChainIDCanton,
		EmitterAddress: DeriveEmitterAddress(bridge.GuardianGovernance, publishes.TrustedOwner, parseDamlUint64(t, publishes.TrustedEmitterID)),
		Sequence:       parseDamlUint64(t, publishes.TrustedSequence),
		Nonce:          trustedPublishNonce,
		Payload:        decodeHexPayload(t, trustedPublishPayloadHex),
	}
	rogueShadowPublish := observedPublish{
		EmitterChain:   vaa.ChainIDCanton,
		EmitterAddress: DeriveEmitterAddress(publishes.RogueGovernance, publishes.RogueOwner, parseDamlUint64(t, publishes.RogueEmitterID)),
		Sequence:       parseDamlUint64(t, publishes.RogueShadowSequence),
		Nonce:          rogueShadowNonce,
		Payload:        decodeHexPayload(t, rogueShadowPayloadHex),
	}
	ggOwnedForeignRegistrarPublish := observedPublish{
		EmitterChain:   vaa.ChainIDCanton,
		EmitterAddress: DeriveEmitterAddress(publishes.RogueGovernance, bridge.GuardianGovernance, parseDamlUint64(t, publishes.GgOwnedForeignRegistrarEmitterID)),
		Sequence:       parseDamlUint64(t, publishes.GgOwnedForeignRegistrarSequence),
		Nonce:          ggOwnedForeignRegistrarNonce,
		Payload:        decodeHexPayload(t, ggOwnedForeignRegistrarPayloadHex),
	}
	require.NotEqual(t, trustedPublish.EmitterAddress, rogueShadowPublish.EmitterAddress)
	require.NotEqual(t, trustedPublish.EmitterAddress, ggOwnedForeignRegistrarPublish.EmitterAddress)
	require.NotEqual(t, rogueShadowPublish.EmitterAddress, ggOwnedForeignRegistrarPublish.EmitterAddress)

	deadline := time.After(90 * time.Second)
	var trustedPublishUpdateID, rogueShadowUpdateID, ggOwnedForeignRegistrarUpdateID cantonclient.UpdateID
	for caseIndex, tc := range cases {
		var wantPublishes []observedPublish
		if tc.wantRogueShadow {
			wantPublishes = append(wantPublishes, rogueShadowPublish)
		}
		if tc.wantGgOwnedForeignRegistrar {
			wantPublishes = append(wantPublishes, ggOwnedForeignRegistrarPublish)
		}
		if tc.wantTrustedPublish {
			wantPublishes = append(wantPublishes, trustedPublish)
		}
		collected := collectMessagePublications(t, tc.name, msgChannels[caseIndex], len(wantPublishes), deadline)
		observed := make([]observedPublish, 0, len(collected))
		for _, msg := range collected {
			observed = append(observed, toObservedPublish(msg))
			updateID, err := cantonclient.UpdateIDFromTxID(msg.TxID)
			require.NoError(t, err)
			switch msg.Nonce {
			case trustedPublishNonce:
				trustedPublishUpdateID = updateID
			case rogueShadowNonce:
				rogueShadowUpdateID = updateID
			case ggOwnedForeignRegistrarNonce:
				ggOwnedForeignRegistrarUpdateID = updateID
			}
		}
		assert.ElementsMatchf(t, wantPublishes, observed, "%s", tc.name)
	}
	require.False(t, trustedPublishUpdateID.IsZero(), "no case emitted the trusted publish")
	require.False(t, rogueShadowUpdateID.IsZero(), "the wildcard control never emitted the rogue shadow publish")
	require.False(t, ggOwnedForeignRegistrarUpdateID.IsZero(), "the wildcard control never emitted the gg-owned foreign-registrar publish")
	require.NotEqual(t, trustedPublishUpdateID, rogueShadowUpdateID)
	require.NotEqual(t, trustedPublishUpdateID, ggOwnedForeignRegistrarUpdateID)
	// The governance reader stream carried the gg-owned publish, so only the registrar pin dropped it.
	require.True(t, streamRecorders[governanceReaderCoreCaseIndex].hasDeliveredUpdateID(ggOwnedForeignRegistrarUpdateID))
	assert.Equal(t, 1.0, testutil.ToFloat64(cantonForeignRegistrarMessagesDropped)-foreignRegistrarDropsBeforePublish,
		"only the governance reader with the core package reaches the registrar pin")

	// Every stream, including the wrong-package one, received the trusted publish.
	for caseIndex, tc := range cases {
		require.Eventuallyf(t, func() bool {
			return streamRecorders[caseIndex].hasDeliveredUpdateID(trustedPublishUpdateID)
		}, 30*time.Second, 100*time.Millisecond, "%s: stream never delivered update %s", tc.name, trustedPublishUpdateID)
	}
	assert.Never(t, func() bool {
		for _, msgC := range msgChannels {
			if len(msgC) > 0 {
				return true
			}
		}
		return false
	}, unexpectedEmissionWindow, 50*time.Millisecond, "a watcher emitted a message after its expected ones")
	for caseIndex, tc := range cases {
		select {
		case extra := <-msgChannels[caseIndex]:
			t.Errorf("%s: unexpected extra message: emitter %s sequence %d nonce %d",
				tc.name, extra.EmitterAddress, extra.Sequence, extra.Nonce)
		default:
		}
	}

	// Reobservation by update id applies the same party filter and package pin as the stream.
	for caseIndex, tc := range cases {
		reobservedTrusted := reobserveAndCollect(t, obsvReqChannels[caseIndex], msgChannels[caseIndex], trustedPublishUpdateID.TxID(), reobservationWindow)
		reobservedRogue := reobserveAndCollect(t, obsvReqChannels[caseIndex], msgChannels[caseIndex], rogueShadowUpdateID.TxID(), reobservationWindow)
		foreignRegistrarDropsBeforeReobservation := testutil.ToFloat64(cantonForeignRegistrarMessagesDropped)
		reobservedGgOwned := reobserveAndCollect(t, obsvReqChannels[caseIndex], msgChannels[caseIndex], ggOwnedForeignRegistrarUpdateID.TxID(), reobservationWindow)
		var wantTrusted, wantRogue, wantGgOwned []observedPublish
		if tc.wantTrustedPublish {
			wantTrusted = []observedPublish{trustedPublish}
		}
		if tc.wantRogueShadow {
			wantRogue = []observedPublish{rogueShadowPublish}
		}
		if tc.wantGgOwnedForeignRegistrar {
			wantGgOwned = []observedPublish{ggOwnedForeignRegistrarPublish}
		}
		wantReobservationDrops := 0.0
		if caseIndex == governanceReaderCoreCaseIndex {
			wantReobservationDrops = 1
		}
		assert.Equalf(t, wantTrusted, toObservedPublishes(reobservedTrusted), "%s: trusted publish reobservation", tc.name)
		assert.Equalf(t, wantRogue, toObservedPublishes(reobservedRogue), "%s: rogue shadow reobservation", tc.name)
		assert.Equalf(t, wantGgOwned, toObservedPublishes(reobservedGgOwned), "%s: gg-owned foreign-registrar reobservation", tc.name)
		assert.Equalf(t, wantReobservationDrops, testutil.ToFloat64(cantonForeignRegistrarMessagesDropped)-foreignRegistrarDropsBeforeReobservation,
			"%s: registrar pin drops on reobservation", tc.name)
		for _, msg := range append(append(reobservedTrusted, reobservedRogue...), reobservedGgOwned...) {
			assert.Len(t, msg.TxID, cantonclient.UpdateIDTxIDLen)
		}
		if len(reobservedTrusted) == 1 {
			assert.Equal(t, trustedPublishUpdateID.TxID(), reobservedTrusted[0].TxID)
		}
	}
}

func toObservedPublishes(msgs []*common.MessagePublication) []observedPublish {
	var observed []observedPublish
	for _, msg := range msgs {
		observed = append(observed, toObservedPublish(msg))
	}
	return observed
}
