//go:build integration

// Upgrade window after the unvet (canton/README.md section 7.5, steps 17 to
// 21): the participant vets the current and the retiring core, then unvets the
// retiring core. After one metadata recheck, reobservation still emits a
// retiring-core publish from before the deadline and drops one from after it,
// and the stream still emits current-core publishes.
//
//	go test -tags integration -run TestCantonWatcherUpgradeWindowAfterUnvet ./pkg/watchers/canton -v
package canton

import (
	"context"
	"path/filepath"
	"testing"
	"time"

	"github.com/certusone/wormhole/node/pkg/cantonclient"
	"github.com/certusone/wormhole/node/pkg/common"
	gossipv1 "github.com/certusone/wormhole/node/pkg/proto/gossip/v1"
	"github.com/certusone/wormhole/node/pkg/supervisor"
	"github.com/prometheus/client_golang/prometheus/testutil"
	"github.com/stretchr/testify/assert"
	"github.com/stretchr/testify/require"
	"github.com/wormhole-foundation/wormhole/sdk/vaa"
	"go.uber.org/zap"
	"google.golang.org/grpc"
	"google.golang.org/grpc/credentials/insecure"
	"google.golang.org/protobuf/encoding/protowire"
)

const (
	updateVettedPackagesFullMethod = "/com.daml.ledger.api.v2.admin.PackageManagementService/UpdateVettedPackages"
	// upgradeWindowAfterUnvetLength must cover the two publishes before the deadline.
	upgradeWindowAfterUnvetLength = 75 * time.Second
	recheckIntervalForUnvetTest   = time.Second
)

// rawProtoBytesCodec sends and receives already-encoded protobuf bytes, for the
// admin PackageManagementService outside the vendored Ledger API subset.
type rawProtoBytesCodec struct{}

func (rawProtoBytesCodec) Marshal(message any) ([]byte, error) { return *(message.(*[]byte)), nil }
func (rawProtoBytesCodec) Unmarshal(encoded []byte, message any) error {
	*(message.(*[]byte)) = append([]byte(nil), encoded...)
	return nil
}
func (rawProtoBytesCodec) Name() string { return "proto" }

// encodeUnvetRequest encodes UpdateVettedPackagesRequest{changes: [{unvet: {packages: [{package_id}]}}]}.
func encodeUnvetRequest(packageIDHex string) []byte {
	vettedPackagesRef := protowire.AppendString(protowire.AppendTag(nil, 1, protowire.BytesType), packageIDHex)
	unvet := protowire.AppendBytes(protowire.AppendTag(nil, 1, protowire.BytesType), vettedPackagesRef)
	vettedPackagesChange := protowire.AppendBytes(protowire.AppendTag(nil, 2, protowire.BytesType), unvet)
	return protowire.AppendBytes(protowire.AppendTag(nil, 1, protowire.BytesType), vettedPackagesChange)
}

func unvetPackageOnSandbox(t *testing.T, ctx context.Context, ledgerAddr, packageIDHex string) {
	t.Helper()
	conn, err := grpc.NewClient(ledgerAddr, grpc.WithTransportCredentials(insecure.NewCredentials()))
	require.NoError(t, err)
	defer conn.Close()
	request := encodeUnvetRequest(packageIDHex)
	var response []byte
	require.NoError(t, conn.Invoke(ctx, updateVettedPackagesFullMethod, &request, &response, grpc.ForceCodec(rawProtoBytesCodec{})))
}

// updateIDsWithPublishExecutedBy returns the delivered update ids with a PublishMessage executed by packageIDHex.
func (r *updateStreamRecorder) updateIDsWithPublishExecutedBy(packageIDHex string) map[string]bool {
	r.mutex.Lock()
	defer r.mutex.Unlock()
	updateIDs := make(map[string]bool)
	for updateID, executingPackageIDs := range r.publishExecutingPackageIDsByUpdateID {
		for _, executingPackageID := range executingPackageIDs {
			if executingPackageID == packageIDHex {
				updateIDs[updateID] = true
			}
		}
	}
	return updateIDs
}

func packageMetadataVerifiedGaugeValue(role cantonclient.AcceptedPackageRole) float64 {
	return testutil.ToFloat64(packageMetadataVerifiedGauge.WithLabelValues(role.String()))
}

func TestCantonWatcherUpgradeWindowAfterUnvet(t *testing.T) {
	rootCtx, cancel := context.WithTimeout(context.Background(), 10*time.Minute)
	defer cancel()

	dpm, testDar, port := startSandbox(t, rootCtx)
	ledgerAddr := "localhost:" + port
	cantonDir, err := filepath.Abs("../../../../canton")
	require.NoError(t, err)
	retiringCorePackageID := readDarMainPackageID(t, dpm, coreDarPath(t, cantonDir))
	retiringCoreVersion, currentCoreVersion, currentCoreDar, currentTestDar := buildCurrentCoreVersion(t, dpm, cantonDir)
	currentCorePackageID := readDarMainPackageID(t, dpm, currentCoreDar)
	require.NotEqual(t, retiringCorePackageID, currentCorePackageID)

	var bridge realBridge
	runDamlScript(t, dpm, testDar, port, "Test.WatcherGovernanceReader:integrationGovernanceReaderSetup", "", &bridge)

	startupWallClock := time.Now().UTC()
	acceptedPackageIDs, err := cantonclient.ParseAcceptedPackageIDs(cantonclient.CurrentPackageConfig{PackageIDHex: currentCorePackageID, Version: currentCoreVersion}, cantonclient.RetiringPackageConfig{
		PackageIDHex:     retiringCorePackageID,
		AcceptedUntilUTC: startupWallClock.Add(upgradeWindowAfterUnvetLength).Format(cantonclient.RetiringPackageDeadlineLayout),
		Version:          retiringCoreVersion,
	}, startupWallClock)
	require.NoError(t, err)
	retiringPackageID, hasRetiring := acceptedPackageIDs.RetiringPackageID()
	require.True(t, hasRetiring)
	retiringDeadline := retiringPackageID.AcceptedBeforeRecordTime()

	msgC := make(chan *common.MessagePublication, 16)
	obsvReqC := make(chan *gossipv1.ObservationRequest, 1)
	streamRecorder := newUpdateStreamRecorder()
	w, err := NewWatcher(ledgerAddr, acceptedPackageIDs, bridge.GuardianGovernance, AuthConfig{}, true, msgC, obsvReqC)
	require.NoError(t, err)
	w.packageMetadataRecheckInterval = recheckIntervalForUnvetTest
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
		t.Fatal("watcher did not subscribe within 60s")
	}

	toExpectedPublish := func(published upgradeWindowPublish) observedPublish {
		return observedPublish{
			EmitterChain:   vaa.ChainIDCanton,
			EmitterAddress: DeriveEmitterAddress(bridge.GuardianGovernance, published.Owner, parseDamlUint64(t, published.EmitterID)),
			Sequence:       parseDamlUint64(t, published.Sequence),
			Nonce:          upgradeWindowNonce,
			Payload:        decodeHexPayload(t, upgradeWindowPayloadHex),
		}
	}

	// Before the deadline: one publish under each core, so the participant vets both.
	retiringCorePublish := runUpgradeWindowPublish(t, dpm, testDar, port, bridge, "UnvetRetiringOwner")
	currentCorePublish := runUpgradeWindowPublish(t, dpm, currentTestDar, port, bridge, "UnvetCurrentOwner")
	collected := collectMessagePublications(t, "publishes before the deadline", msgC, 2, time.After(90*time.Second))
	require.Truef(t, time.Now().Before(retiringDeadline), "the publishes took longer than %s; raise upgradeWindowAfterUnvetLength", upgradeWindowAfterUnvetLength)
	assert.ElementsMatch(t, []observedPublish{toExpectedPublish(retiringCorePublish), toExpectedPublish(currentCorePublish)}, toObservedPublishes(collected))
	var retiringCorePublishTxID []byte
	for _, msg := range collected {
		if toObservedPublish(msg).EmitterAddress == toExpectedPublish(retiringCorePublish).EmitterAddress {
			retiringCorePublishTxID = msg.TxID
		}
	}
	require.NotNil(t, retiringCorePublishTxID)

	// After the deadline: the stream drops a retiring-core publish.
	time.Sleep(time.Until(retiringDeadline) + time.Second)
	retiringAfterDeadlineCountBefore := retiringPackagePublishAfterDeadlineCount(t)
	retiringUpdateIDsBefore := streamRecorder.updateIDsWithPublishExecutedBy(retiringCorePackageID)
	runPublishOnExistingEmitter(t, dpm, testDar, port, bridge, retiringCorePublish.Owner, &retiringCorePackageID)
	var retiringCoreAfterDeadlineUpdateID cantonclient.UpdateID
	require.Eventually(t, func() bool {
		for updateIDHex := range streamRecorder.updateIDsWithPublishExecutedBy(retiringCorePackageID) {
			if !retiringUpdateIDsBefore[updateIDHex] {
				retiringCoreAfterDeadlineUpdateID, err = cantonclient.ParseUpdateID(updateIDHex)
				require.NoError(t, err)
				return true
			}
		}
		return false
	}, 30*time.Second, 100*time.Millisecond, "the stream never delivered the retiring-core publish after the deadline")
	require.Eventually(t, func() bool {
		return retiringPackagePublishAfterDeadlineCount(t) == retiringAfterDeadlineCountBefore+1
	}, 30*time.Second, 100*time.Millisecond, "the stream must drop and count the retiring-core publish after the deadline")

	// Unvet the retiring core and wait for one recheck to see it.
	require.Equal(t, float64(1), packageMetadataVerifiedGaugeValue(cantonclient.AcceptedPackageRoleRetiring))
	unvetPackageOnSandbox(t, rootCtx, ledgerAddr, retiringCorePackageID)
	require.Eventually(t, func() bool {
		return packageMetadataVerifiedGaugeValue(cantonclient.AcceptedPackageRoleRetiring) == 0
	}, 30*recheckIntervalForUnvetTest, 100*time.Millisecond, "the recheck never reported the unvet")
	require.Equal(t, float64(1), packageMetadataVerifiedGaugeValue(cantonclient.AcceptedPackageRoleCurrent))

	reobservedBeforeDeadline := reobserveAndCollect(t, obsvReqC, msgC, retiringCorePublishTxID, reobservationWindow)
	assert.Equal(t, []observedPublish{toExpectedPublish(retiringCorePublish)}, toObservedPublishes(reobservedBeforeDeadline),
		"a retiring-core publish from before the deadline must stay recoverable after the unvet")

	reobservedAfterDeadline := reobserveAndCollect(t, obsvReqC, msgC, retiringCoreAfterDeadlineUpdateID.TxID(), reobservationWindow)
	assert.Empty(t, reobservedAfterDeadline, "a retiring-core publish from after the deadline must stay dropped")
	assert.Equal(t, retiringAfterDeadlineCountBefore+2, retiringPackagePublishAfterDeadlineCount(t))

	// The stream stays live: a current-core publish after the unvet is emitted once.
	currentCoreAfterUnvetPublish := runUpgradeWindowPublish(t, dpm, currentTestDar, port, bridge, "UnvetLaterOwner")
	collectedAfterUnvet := collectMessagePublications(t, "current-core publish after the unvet", msgC, 1, time.After(60*time.Second))
	assert.Equal(t, []observedPublish{toExpectedPublish(currentCoreAfterUnvetPublish)}, toObservedPublishes(collectedAfterUnvet))
	assert.Never(t, func() bool { return len(msgC) > 0 }, unexpectedEmissionWindow, 50*time.Millisecond, "the watcher emitted a message after its expected ones")
}
