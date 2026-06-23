// Package canton implements the Wormhole guardian watcher for the Canton
// Network. It observes the Canton Ledger API v2 (via node/pkg/cantonclient) for
// the result of the Daml `PublishMessage` choice on the core bridge's `Emitter`
// template and turns each into a common.MessagePublication.
//
// See canton/README.md for the full design. The watcher mirrors the structure
// of node/pkg/watchers/sui.
package canton

import (
	"context"
	"fmt"
	"math"
	"time"

	"github.com/certusone/wormhole/node/pkg/cantonclient"
	"github.com/certusone/wormhole/node/pkg/common"
	"github.com/certusone/wormhole/node/pkg/p2p"
	gossipv1 "github.com/certusone/wormhole/node/pkg/proto/gossip/v1"
	"github.com/certusone/wormhole/node/pkg/readiness"
	"github.com/certusone/wormhole/node/pkg/supervisor"
	"github.com/certusone/wormhole/node/pkg/watchers"

	"github.com/prometheus/client_golang/prometheus"
	"github.com/prometheus/client_golang/prometheus/promauto"
	"github.com/wormhole-foundation/wormhole/sdk/vaa"
	"go.uber.org/zap"
	"google.golang.org/grpc"
	"google.golang.org/grpc/credentials/insecure"
)

// The Daml template/choice the watcher observes. PublishMessage is a choice on
// the Emitter template in module Wormhole.Core.State; its exercise_result is the
// WormholeMessage. (See canton/core/daml/Wormhole/Core/State.daml.)
const (
	publishMessageModule = "Wormhole.Core.State"
	publishMessageEntity = "Emitter"
	publishMessageChoice = "PublishMessage"
)

// cantonDialOpts returns the gRPC dial options for connecting to the Canton
// Ledger API. In unsafe dev mode the local node serves plaintext gRPC, so TLS is
// disabled; otherwise cantonclient.NewCantonGrpcClient applies TLS by default.
func cantonDialOpts(unsafeDevMode bool) []grpc.DialOption {
	if unsafeDevMode {
		return []grpc.DialOption{grpc.WithTransportCredentials(insecure.NewCredentials())}
	}
	return nil
}

type Watcher struct {
	cantonRPC   string
	packageID   string
	readAsParty string

	unsafeDevMode bool

	msgChan       chan<- *common.MessagePublication
	obsvReqC      <-chan *gossipv1.ObservationRequest
	readinessSync readiness.Component

	// cantonClient is an interface; a nil check is fine for "not injected".
	// Tests inject a fake; in production Run creates a gRPC client.
	cantonClient cantonclient.CantonClient
}

var (
	cantonMessagesConfirmed = promauto.NewCounter(
		prometheus.CounterOpts{
			Name: "wormhole_canton_observations_confirmed_total",
			Help: "Total number of verified Canton observations found",
		})
	currentCantonHeight = promauto.NewGauge(
		prometheus.GaugeOpts{
			Name: "wormhole_canton_current_height",
			Help: "Current Canton ledger offset",
		})
)

// NewWatcher creates a new Canton watcher.
func NewWatcher(
	cantonRPC string,
	packageID string,
	readAsParty string,
	unsafeDevMode bool,
	msgC chan<- *common.MessagePublication,
	obsvReqC <-chan *gossipv1.ObservationRequest,
) *Watcher {
	return &Watcher{
		cantonRPC:     cantonRPC,
		packageID:     packageID,
		readAsParty:   readAsParty,
		unsafeDevMode: unsafeDevMode,
		msgChan:       msgC,
		obsvReqC:      obsvReqC,
		readinessSync: common.MustConvertChainIdToReadinessSyncing(vaa.ChainIDCanton),
	}
}

// templateID is the Ledger API filter for the core bridge's Emitter template.
func (e *Watcher) templateID() cantonclient.TemplateID {
	return cantonclient.TemplateID{
		PackageID:  e.packageID,
		ModuleName: publishMessageModule,
		EntityName: publishMessageEntity,
	}
}

// processMessage turns a decoded Canton message event into a
// common.MessagePublication and publishes it to the message channel.
//
// The VAA timestamp is taken from the transaction's ledger effective time
// (whitepaper 0001/0004: the timestamp is block-derived). The TxID is the
// participant offset encoded as 32 big-endian bytes, so reobservation can decode
// it back into an offset.
func (e *Watcher) processMessage(logger *zap.Logger, ev cantonclient.CantonMessageEvent, isReobservation bool) {
	if len(ev.Message.Sender) != 32 {
		logger.Error("dropping Canton message with malformed sender",
			zap.Int("senderLen", len(ev.Message.Sender)),
			zap.Int64("offset", ev.Offset))
		p2p.DefaultRegistry.AddErrorCount(vaa.ChainIDCanton, 1)
		return
	}

	var emitter vaa.Address
	copy(emitter[:], ev.Message.Sender)

	timestamp := ev.EffectiveAt
	if timestamp.IsZero() {
		// Should not happen for a committed transaction, but never publish a
		// zero timestamp.
		logger.Error("dropping Canton message with zero effective time",
			zap.Int64("offset", ev.Offset))
		p2p.DefaultRegistry.AddErrorCount(vaa.ChainIDCanton, 1)
		return
	}

	observation := &common.MessagePublication{
		TxID:             cantonclient.OffsetToTxID(ev.Offset),
		Timestamp:        timestamp,
		Nonce:            ev.Message.Nonce,
		Sequence:         ev.Message.Sequence,
		EmitterChain:     vaa.ChainIDCanton,
		EmitterAddress:   emitter,
		Payload:          ev.Message.Payload,
		ConsistencyLevel: ev.Message.ConsistencyLevel,
		IsReobservation:  isReobservation,
		Unreliable:       false,
	}

	e.msgChan <- observation //nolint:channelcheck // The channel to the processor is buffered and shared across chains, if it backs up we should stop processing new observations

	cantonMessagesConfirmed.Inc()
	if isReobservation {
		watchers.ReobservationsByChain.WithLabelValues("canton", "std").Inc()
	}

	logger.Info("message observed", observation.ZapFields(zap.String("updateId", ev.UpdateID))...)
}

// handleReobservation fetches the transaction identified by a re-observation
// request (the TxID encodes a participant offset) and re-processes each Wormhole
// message it produced.
func (e *Watcher) handleReobservation(ctx context.Context, logger *zap.Logger, client cantonclient.CantonClient, r *gossipv1.ObservationRequest) {
	// node/pkg/node/reobserve.go enforces a valid uint16 chain id and only
	// writes to this chain's channel; anything else is a programming error.
	if r.ChainId > math.MaxUint16 || vaa.ChainID(r.ChainId) != vaa.ChainIDCanton {
		panic("invalid chain ID")
	}

	offset, err := cantonclient.TxIDToOffset(r.TxHash)
	if err != nil {
		logger.Error("canton_fetch_obvs_req invalid txID", zap.Error(err))
		p2p.DefaultRegistry.AddErrorCount(vaa.ChainIDCanton, 1)
		return
	}

	txn, err := client.GetUpdateByOffset(ctx, offset, e.templateID(), publishMessageChoice)
	if err != nil {
		logger.Error("canton_fetch_obvs_req failed", zap.Int64("offset", offset), zap.Error(err))
		p2p.DefaultRegistry.AddErrorCount(vaa.ChainIDCanton, 1)
		return
	}

	for _, msg := range txn.Messages {
		e.processMessage(logger, cantonclient.CantonMessageEvent{
			Offset:      txn.Offset,
			UpdateID:    txn.UpdateID,
			EffectiveAt: txn.EffectiveAt,
			Message:     msg,
		}, true)
	}
}

func (e *Watcher) Run(ctx context.Context) error {
	p2p.DefaultRegistry.SetNetworkStats(vaa.ChainIDCanton, &gossipv1.Heartbeat_Network{
		ContractAddress: e.packageID,
	})

	logger := supervisor.Logger(ctx)

	logger.Info("Starting watcher",
		zap.String("watcher_name", "canton"),
		zap.String("cantonRPC", e.cantonRPC),
		zap.String("packageID", e.packageID),
		zap.String("readAsParty", e.readAsParty),
		zap.Bool("unsafeDevMode", e.unsafeDevMode),
	)

	// Use an injected client (e.g. from tests) if present, otherwise create one
	// for the lifetime of this Run. The client is kept local rather than stored
	// on the Watcher so each supervisor restart establishes a fresh connection
	// and the goroutines below cannot observe a closed/nil client at shutdown.
	client := e.cantonClient
	if client == nil {
		grpcClient, err := cantonclient.NewCantonGrpcClient(e.cantonRPC, e.readAsParty, logger, cantonDialOpts(e.unsafeDevMode)...)
		if err != nil {
			return fmt.Errorf("failed to create Canton gRPC client: %w", err)
		}
		client = grpcClient
		defer func() {
			if cerr := client.Close(); cerr != nil {
				logger.Error("failed to close Canton gRPC client", zap.Error(cerr))
			}
		}()
	}

	// Confirm connectivity and establish the stream start offset before
	// reporting healthy.
	ledgerEnd, err := client.GetLedgerEnd(ctx)
	if err != nil {
		return fmt.Errorf("failed to get ledger end: %w", err)
	}
	currentCantonHeight.Set(float64(ledgerEnd))

	timer := time.NewTicker(time.Second * 5)
	defer timer.Stop()

	errC := make(chan error)

	supervisor.Signal(ctx, supervisor.SignalHealthy)
	readiness.SetReady(e.readinessSync)

	// Data pump: stream updates from the current ledger end onward.
	common.RunWithScissors(ctx, errC, "canton_data_pump", func(ctx context.Context) error {
		eventChan := make(chan cantonclient.CantonMessageEvent, 64)

		subscription, err := client.SubscribeUpdates(ctx, ledgerEnd, e.templateID(), publishMessageChoice, eventChan)
		if err != nil {
			return fmt.Errorf("canton_data_pump failed to subscribe to updates: %w", err)
		}
		defer subscription.Unsubscribe()

		for {
			select {
			case <-ctx.Done():
				logger.Error("canton_data_pump context done")
				return ctx.Err()
			case subErr := <-subscription.Err():
				return fmt.Errorf("canton_data_pump subscription error: %w", subErr)
			case ev := <-eventChan:
				e.processMessage(logger, ev, false)
			}
		}
	})

	// Ledger-offset poll: report height + readiness every 5s.
	common.RunWithScissors(ctx, errC, "canton_block_height", func(ctx context.Context) error {
		for {
			select {
			case <-ctx.Done():
				logger.Error("canton_block_height context done")
				return ctx.Err()
			case <-timer.C:
				offset, err := client.GetLedgerEnd(ctx)
				if err != nil {
					logger.Error("Failed to get ledger end", zap.Error(err))
				} else {
					currentCantonHeight.Set(float64(offset))
					logger.Debug("canton_getLedgerEnd", zap.Int64("result", offset))
					p2p.DefaultRegistry.SetNetworkStats(vaa.ChainIDCanton, &gossipv1.Heartbeat_Network{
						Height:          offset,
						ContractAddress: e.packageID,
					})
				}
				readiness.SetReady(e.readinessSync)
			}
		}
	})

	// Reobservation handler.
	common.RunWithScissors(ctx, errC, "canton_fetch_obvs_req", func(ctx context.Context) error {
		for {
			select {
			case <-ctx.Done():
				logger.Error("canton_fetch_obvs_req context done")
				return ctx.Err()
			case r := <-e.obsvReqC:
				e.handleReobservation(ctx, logger, client, r)
			}
		}
	})

	select {
	case <-ctx.Done():
		return ctx.Err()
	case err := <-errC:
		return err
	}
}
