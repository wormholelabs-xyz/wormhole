//go:build localnet

// LocalNet harness for the Canton watcher. It builds the watcher with WatcherConfig.Create in the TestNet
// environment, so the mandatory package-id and read-as-party checks run, and supervises the returned
// Runnable against a Splice LocalNet participant. The Ledger API user of the token holds only
// CanReadAs(guardianGovernance) (canton/localnet/watcher_harness_lib.sh creates it). LocalNet serves
// plaintext gRPC with a static HS256 JWT; the harness supplies that transport through
// WatcherConfig.ledgerTransportForTest, which only package tests can set.
//
//	CANTON_LOCALNET_GRPC=localhost:3901 CANTON_LOCALNET_TOKEN_FILE=<jwt> \
//	CANTON_LOCALNET_READ_AS_PARTY=<guardianGovernance> CANTON_LOCALNET_CORE_PACKAGE_ID=<core package id> \
//	CANTON_LOCALNET_CORE_PACKAGE_VERSION=<core package version> \
//	CANTON_LOCALNET_OBSERVATIONS_FILE=<out.jsonl> CANTON_LOCALNET_READY_FILE=<path> CANTON_LOCALNET_STOP_FILE=<path> \
//	CANTON_LOCALNET_METRICS_FILE=<out.json> CANTON_LOCALNET_REOBSERVE_UPDATE_IDS=<update id,...> \
//	go test -tags localnet -run TestCantonWatcherLocalNet ./pkg/watchers/canton -v -timeout 15m
//
// The harness writes one JSON line per observation (live and reobserved). It stops when the stop
// file exists and each requested update id was reobserved, after it writes the queued observations.
// Every 500 ms and at the stop, it replaces the metrics file with the
// wormhole_canton_unpinned_publish_messages_total counts by executing package id.
// It fails at CANTON_LOCALNET_MAX_SECONDS. The id and version must be in approved_core_packages.json.
// canton/localnet/watcher_harness_lib.sh drives it.
package canton

import (
	"context"
	"encoding/hex"
	"encoding/json"
	"fmt"
	"os"
	"strconv"
	"strings"
	"testing"
	"time"

	"github.com/certusone/wormhole/node/pkg/cantonclient"
	"github.com/certusone/wormhole/node/pkg/common"
	gossipv1 "github.com/certusone/wormhole/node/pkg/proto/gossip/v1"
	"github.com/certusone/wormhole/node/pkg/supervisor"
	"github.com/prometheus/client_golang/prometheus"
	"github.com/prometheus/client_golang/prometheus/testutil"
	"github.com/stretchr/testify/require"
	"github.com/wormhole-foundation/wormhole/sdk/vaa"
	"go.uber.org/zap"
	"google.golang.org/grpc"
	"google.golang.org/grpc/credentials/insecure"
)

const (
	localNetDefaultMaxSeconds = 300
	localNetStopPollInterval  = 500 * time.Millisecond
	unpinnedPublishMetricName = "wormhole_canton_unpinned_publish_messages_total"
)

// localNetBearerToken sends a static LocalNet JWT on every RPC. LocalNet serves plaintext gRPC.
type localNetBearerToken struct{ jwt string }

func (token localNetBearerToken) GetRequestMetadata(context.Context, ...string) (map[string]string, error) {
	return map[string]string{"authorization": "Bearer " + token.jwt}, nil
}

func (localNetBearerToken) RequireTransportSecurity() bool { return false }

type localNetObservation struct {
	EmitterChain     uint16 `json:"emitterChain"`
	EmitterAddress   string `json:"emitterAddress"`
	Sequence         uint64 `json:"sequence"`
	Nonce            uint32 `json:"nonce"`
	ConsistencyLevel uint8  `json:"consistencyLevel"`
	PayloadHex       string `json:"payloadHex"`
	UpdateID         string `json:"updateId"`
	IsReobservation  bool   `json:"isReobservation"`
	Timestamp        string `json:"timestamp"`
}

func requiredEnv(t *testing.T, name string) string {
	t.Helper()
	value := os.Getenv(name)
	if value == "" {
		t.Skipf("%s is not set; skipping the LocalNet watcher harness", name)
	}
	return value
}

func TestCantonWatcherLocalNet(t *testing.T) {
	rpcAddress := requiredEnv(t, "CANTON_LOCALNET_GRPC")
	jwtBytes, err := os.ReadFile(requiredEnv(t, "CANTON_LOCALNET_TOKEN_FILE"))
	require.NoError(t, err)
	readAsParty := requiredEnv(t, "CANTON_LOCALNET_READ_AS_PARTY")
	corePackageID := requiredEnv(t, "CANTON_LOCALNET_CORE_PACKAGE_ID")
	require.Len(t, corePackageID, 64)
	corePackageVersion := requiredEnv(t, "CANTON_LOCALNET_CORE_PACKAGE_VERSION")
	observationsFile := requiredEnv(t, "CANTON_LOCALNET_OBSERVATIONS_FILE")
	metricsFile := requiredEnv(t, "CANTON_LOCALNET_METRICS_FILE")
	readyFile := requiredEnv(t, "CANTON_LOCALNET_READY_FILE")
	stopFile := requiredEnv(t, "CANTON_LOCALNET_STOP_FILE")
	reobserveUpdateIDs, err := parseReobserveUpdateIDs(os.Getenv("CANTON_LOCALNET_REOBSERVE_UPDATE_IDS"))
	require.NoError(t, err)
	maxSeconds := localNetDefaultMaxSeconds
	if value := os.Getenv("CANTON_LOCALNET_MAX_SECONDS"); value != "" {
		maxSeconds, err = strconv.Atoi(value)
		require.NoError(t, err)
		require.Positive(t, maxSeconds)
	}

	rootCtx, cancel := context.WithTimeout(context.Background(), time.Duration(maxSeconds)*time.Second)
	defer cancel()
	logger, err := zap.NewDevelopment()
	require.NoError(t, err)

	msgC := make(chan *common.MessagePublication, 64)
	obsvReqC := make(chan *gossipv1.ObservationRequest, localNetMaxReobservations)
	watcherConfig := WatcherConfig{
		NetworkID:      "canton",
		ChainID:        vaa.ChainIDCanton,
		Rpc:            rpcAddress,
		PackageID:      corePackageID,
		PackageVersion: corePackageVersion,
		ReadAsParty:    readAsParty,
		ledgerTransportForTest: &ledgerTransportOverride{
			transportCreds: insecure.NewCredentials(),
			dialOpts:       []grpc.DialOption{grpc.WithPerRPCCredentials(localNetBearerToken{jwt: strings.TrimSpace(string(jwtBytes))})},
		},
	}
	checkCreateRejectsAnUnpinnedConfig(t, watcherConfig, msgC, obsvReqC)
	runWatcher, reobserver, err := watcherConfig.Create(msgC, obsvReqC, nil, nil, nil, common.TestNet)
	require.NoError(t, err)
	require.NotNil(t, runWatcher)
	require.Nil(t, reobserver)

	supervisor.New(rootCtx, logger, func(ctx context.Context) error {
		if err := supervisor.Run(ctx, "canton", runWatcher); err != nil {
			return err
		}
		<-ctx.Done()
		return nil
	}, supervisor.WithPropagatePanic)

	require.Eventually(t, func() bool { return testutil.ToFloat64(currentCantonHeight) > 0 }, 60*time.Second, time.Second,
		"watcher did not read the ledger end")
	require.NoError(t, os.WriteFile(readyFile, []byte("ready\n"), 0o600))
	reobservedUpdates := newReobservedUpdateTracker(reobserveUpdateIDs)
	for _, updateID := range reobserveUpdateIDs {
		obsvReqC <- &gossipv1.ObservationRequest{ChainId: uint32(vaa.ChainIDCanton), TxHash: updateID.TxID()}
	}

	output, err := os.Create(observationsFile)
	require.NoError(t, err)
	defer output.Close()
	encoder := json.NewEncoder(output)
	writeObservation := func(observation *common.MessagePublication) error {
		if observation.EmitterChain != vaa.ChainIDCanton {
			return fmt.Errorf("observation of chain %d, expected %d", observation.EmitterChain, vaa.ChainIDCanton)
		}
		updateID, err := cantonclient.UpdateIDFromTxID(observation.TxID)
		if err != nil {
			return err
		}
		if observation.IsReobservation {
			if err := reobservedUpdates.recordReobservation(updateID); err != nil {
				return err
			}
		}
		return encoder.Encode(localNetObservation{
			EmitterChain:     uint16(observation.EmitterChain),
			EmitterAddress:   hex.EncodeToString(observation.EmitterAddress[:]),
			Sequence:         observation.Sequence,
			Nonce:            observation.Nonce,
			ConsistencyLevel: observation.ConsistencyLevel,
			PayloadHex:       hex.EncodeToString(observation.Payload),
			UpdateID:         updateID.String(),
			IsReobservation:  observation.IsReobservation,
			Timestamp:        observation.Timestamp.UTC().Format(time.RFC3339Nano),
		})
	}
	stopTicker := time.NewTicker(localNetStopPollInterval)
	defer stopTicker.Stop()
	for {
		select {
		case <-rootCtx.Done():
			t.Fatalf("harness reached CANTON_LOCALNET_MAX_SECONDS=%d without the stop file", maxSeconds)
		case observation := <-msgC:
			require.NoError(t, writeObservation(observation))
		case <-stopTicker.C:
			writeUnpinnedPublishCounts(t, metricsFile)
			if _, err := os.Stat(stopFile); err == nil && reobservedUpdates.allRequestedUpdatesObserved() {
				_, err := drainPublications(msgC, cap(msgC), writeObservation)
				require.NoError(t, err)
				writeUnpinnedPublishCounts(t, metricsFile)
				return
			}
		}
	}
}

// checkCreateRejectsAnUnpinnedConfig: outside unsafe dev mode, Create refuses an empty package id or read-as party.
func checkCreateRejectsAnUnpinnedConfig(t *testing.T, watcherConfig WatcherConfig,
	msgC chan<- *common.MessagePublication, obsvReqC <-chan *gossipv1.ObservationRequest) {
	t.Helper()
	withoutPackageID := watcherConfig
	withoutPackageID.PackageID = ""
	_, _, err := withoutPackageID.Create(msgC, obsvReqC, nil, nil, nil, common.TestNet)
	require.ErrorContains(t, err, "PackageID must be set")
	withoutReadAsParty := watcherConfig
	withoutReadAsParty.ReadAsParty = ""
	_, _, err = withoutReadAsParty.Create(msgC, obsvReqC, nil, nil, nil, common.TestNet)
	require.ErrorContains(t, err, "ReadAsParty must be set")
}

// writeUnpinnedPublishCounts replaces the metrics file with {executing package id: count} of the
// unpinned-publish counter. The rename keeps each read of the file complete.
func writeUnpinnedPublishCounts(t *testing.T, metricsFile string) {
	t.Helper()
	metricFamilies, err := prometheus.DefaultGatherer.Gather()
	require.NoError(t, err)
	countsByExecutingPackageID := map[string]float64{}
	for _, metricFamily := range metricFamilies {
		if metricFamily.GetName() != unpinnedPublishMetricName {
			continue
		}
		for _, metric := range metricFamily.GetMetric() {
			for _, label := range metric.GetLabel() {
				if label.GetName() == "executing_package_id" {
					countsByExecutingPackageID[label.GetValue()] = metric.GetCounter().GetValue()
				}
			}
		}
	}
	encodedCounts, err := json.Marshal(countsByExecutingPackageID)
	require.NoError(t, err)
	partialFile := metricsFile + ".partial"
	require.NoError(t, os.WriteFile(partialFile, append(encodedCounts, '\n'), 0o600))
	require.NoError(t, os.Rename(partialFile, metricsFile))
}
