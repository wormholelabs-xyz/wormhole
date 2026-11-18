//go:build localnet

// LocalNet harness for the Canton watcher: runs watcher.Run with the production settings (pinned core
// package id and version, update stream narrowed to guardianGovernance, unsafeDevMode false) against a Splice
// LocalNet participant. LocalNet authenticates with a static HS256 JWT over plaintext gRPC, so the
// harness injects a gRPC client that carries that token.
//
//	CANTON_LOCALNET_GRPC=localhost:3901 CANTON_LOCALNET_TOKEN_FILE=<jwt> \
//	CANTON_LOCALNET_READ_AS_PARTY=<guardianGovernance> CANTON_LOCALNET_CORE_PACKAGE_ID=<core package id> \
//	CANTON_LOCALNET_CORE_PACKAGE_VERSION=<core package version> \
//	CANTON_LOCALNET_OBSERVATIONS_FILE=<out.jsonl> CANTON_LOCALNET_READY_FILE=<path> CANTON_LOCALNET_STOP_FILE=<path> \
//	CANTON_LOCALNET_REOBSERVE_UPDATE_IDS=<update id,...> \
//	go test -tags localnet -run TestCantonWatcherLocalNet ./pkg/watchers/canton -v -timeout 15m
//
// The harness writes one JSON line per observation (live and reobserved). It stops when the stop
// file exists and each requested update id was reobserved, after it writes the queued observations.
// It fails at CANTON_LOCALNET_MAX_SECONDS. The id and version must be in approved_core_packages.json.
// canton/localnet/ntt_watcher.sh drives it.
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

	client, err := cantonclient.NewCantonGrpcClient(rpcAddress, readAsParty, logger, insecure.NewCredentials(),
		grpc.WithPerRPCCredentials(localNetBearerToken{jwt: strings.TrimSpace(string(jwtBytes))}))
	require.NoError(t, err)
	defer client.Close()

	msgC := make(chan *common.MessagePublication, 64)
	obsvReqC := make(chan *gossipv1.ObservationRequest, localNetMaxReobservations)
	acceptedPackageIDs, err := cantonclient.ParseAcceptedPackageIDs(
		cantonclient.CurrentPackageConfig{PackageIDHex: corePackageID, Version: corePackageVersion},
		cantonclient.RetiringPackageConfig{}, time.Now())
	require.NoError(t, err)
	watcher, err := NewWatcher(rpcAddress, acceptedPackageIDs, readAsParty, AuthConfig{}, false, msgC, obsvReqC)
	require.NoError(t, err)
	watcher.cantonClient = client

	supervisor.New(rootCtx, logger, func(ctx context.Context) error {
		if err := supervisor.Run(ctx, "canton", watcher.Run); err != nil {
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
			if _, err := os.Stat(stopFile); err == nil && reobservedUpdates.allRequestedUpdatesObserved() {
				_, err := drainPublications(msgC, cap(msgC), writeObservation)
				require.NoError(t, err)
				return
			}
		}
	}
}
