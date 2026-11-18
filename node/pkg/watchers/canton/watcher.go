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
	"encoding/binary"
	"errors"
	"fmt"
	"math"
	"slices"
	"sync/atomic"
	"time"

	"github.com/certusone/wormhole/node/pkg/cantonclient"
	"github.com/certusone/wormhole/node/pkg/common"
	"github.com/certusone/wormhole/node/pkg/p2p"
	gossipv1 "github.com/certusone/wormhole/node/pkg/proto/gossip/v1"
	"github.com/certusone/wormhole/node/pkg/readiness"
	"github.com/certusone/wormhole/node/pkg/supervisor"
	"github.com/certusone/wormhole/node/pkg/watchers"

	ethcrypto "github.com/ethereum/go-ethereum/crypto"
	"github.com/prometheus/client_golang/prometheus"
	"github.com/prometheus/client_golang/prometheus/promauto"
	"github.com/wormhole-foundation/wormhole/sdk/vaa"
	"go.uber.org/zap"
	"google.golang.org/grpc"
	"google.golang.org/grpc/credentials"
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

// The Canton TxID must fit MessagePublication's TxID bounds (common.TxIDLenMin..255).
const (
	_ uint  = cantonclient.UpdateIDTxIDLen - common.TxIDLenMin
	_ uint8 = cantonclient.UpdateIDTxIDLen
)

// emitterAddressTag is the domain-separation tag for emitter addresses. It MUST
// match Wormhole.Core.State.emitterAddressTag; the encoding is pinned across
// both languages by the shared vector in
// node/pkg/cantonclient/vectorgen_test.go (TestCantonDamlFixtures),
// Test.TestCore:testAddressVector, and TestDeriveEmitterAddressMatchesVector.
const emitterAddressTag = "wormhole:emitter:v1"

// DeriveEmitterAddress computes the canonical 32-byte Wormhole emitter address
// from an emitter's contract-key components:
//
//	keccak256( utf8(tag) ‖ lp(registrar) ‖ lp(owner) ‖ uint64be(emitterId) )
//
// where lp is a uint32 big-endian byte-length prefix. The two length prefixes
// make the preimage injective across the two variable-length party ids; owner is
// part of the preimage so a compromised operator cannot forge an existing
// emitter's address (see Wormhole.Core.State).
func DeriveEmitterAddress(registrar string, owner string, emitterID uint64) vaa.Address {
	buf := make([]byte, 0, len(emitterAddressTag)+8+len(registrar)+len(owner)+8)
	buf = append(buf, emitterAddressTag...)
	var l [4]byte
	binary.BigEndian.PutUint32(l[:], uint32(len(registrar))) //nolint:gosec // party ids are short
	buf = append(buf, l[:]...)
	buf = append(buf, registrar...)
	binary.BigEndian.PutUint32(l[:], uint32(len(owner))) //nolint:gosec // party ids are short
	buf = append(buf, l[:]...)
	buf = append(buf, owner...)
	var idb [8]byte
	binary.BigEndian.PutUint64(idb[:], emitterID)
	buf = append(buf, idb[:]...)
	var a vaa.Address
	copy(a[:], ethcrypto.Keccak256(buf))
	return a
}

// AuthConfig carries OAuth2 client-credentials parameters for a
// Keycloak-fronted Ledger API. All fields set enables per-RPC bearer tokens;
// all empty disables authentication. Partial configs are rejected by
// WatcherConfig.Create.
type AuthConfig struct {
	// TokenURL is the full OAuth2 token endpoint
	// (…/realms/<realm>/protocol/openid-connect/token for Keycloak).
	TokenURL     string
	ClientID     string
	ClientSecret string
}

func (a AuthConfig) enabled() bool {
	return a.TokenURL != "" || a.ClientID != "" || a.ClientSecret != ""
}

func (a AuthConfig) complete() bool {
	return a.TokenURL != "" && a.ClientID != "" && a.ClientSecret != ""
}

// cantonTransportCreds selects the gRPC transport. The unsafe dev-mode
// participant serves plaintext; everything else is TLS (nil defers to
// cantonclient's TLS default).
func (e *Watcher) cantonTransportCreds() credentials.TransportCredentials {
	if e.unsafeDevMode {
		return insecure.NewCredentials()
	}
	return nil
}

// cantonDialOpts returns the non-transport dial options: extraLedgerDialOpts,
// then OAuth per-RPC credentials when the Ledger API is behind an identity
// provider. Bearer tokens require transport security, so OAuth is omitted in
// dev mode (plaintext). The ctx bounds token fetches and must outlive the
// connection (Run's ctx).
func (e *Watcher) cantonDialOpts(ctx context.Context) []grpc.DialOption {
	dialOpts := slices.Clone(e.extraLedgerDialOpts)
	if !e.unsafeDevMode && e.auth.complete() {
		dialOpts = append(dialOpts, cantonclient.NewOAuthDialOption(ctx, e.auth.TokenURL, e.auth.ClientID, e.auth.ClientSecret))
	}
	return dialOpts
}

type Watcher struct {
	cantonRPC          string
	acceptedPackageIDs cantonclient.AcceptedPackageIDs
	readAsParty        string
	auth               AuthConfig

	unsafeDevMode bool

	msgChan       chan<- *common.MessagePublication
	obsvReqC      <-chan *gossipv1.ObservationRequest
	readinessSync readiness.Component

	// lastOffset is the offset of the newest stream event processed, kept
	// across supervisor restarts of Run (the Watcher instance persists). Canton
	// ends streams whose access token expired, so Run resubscribes from here
	// rather than the current ledger end to avoid dropping the reconnect
	// window. Written only by the data-pump goroutine.
	lastOffset atomic.Int64

	// packageMetadata holds which pinned roles passed the vetting metadata check.
	packageMetadata packageMetadataVerification

	// packageMetadataRecheckInterval is the period of the vetting metadata recheck.
	packageMetadataRecheckInterval time.Duration

	// approvedCorePackages is the reviewed (id, version) list that outside
	// unsafe dev mode every pinned core package must match.
	approvedCorePackages cantonclient.ApprovedCorePackages

	// cantonClient is an interface; a nil check is fine for "not injected".
	// Tests inject a fake; in production Run creates a gRPC client.
	cantonClient cantonclient.CantonClient

	// extraLedgerDialOpts are passed to the gRPC client Run creates. Integration
	// tests set a stream interceptor here to observe the production client.
	extraLedgerDialOpts []grpc.DialOption
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
	// At most two series: role current and, during an upgrade window, role retiring.
	acceptedPackageIDsGauge = promauto.NewGaugeVec(
		prometheus.GaugeOpts{
			Name: "wormhole_canton_accepted_package_ids",
			Help: "Accepted wormhole-core package ids (value 1). A role=retiring series means the upgrade window is open",
		}, []string{"role", "package_id"})
	// Two series: role current and role retiring.
	acceptedPublishMessages = promauto.NewCounterVec(
		prometheus.CounterOpts{
			Name: "wormhole_canton_accepted_publish_messages_total",
			Help: "PublishMessage exercises the watcher emitted from the stream, by the role of the executing core package id",
		}, []string{"role"})
	retiringPackageAcceptedUntilGauge = promauto.NewGauge(
		prometheus.GaugeOpts{
			Name: "wormhole_canton_retiring_package_accepted_until_seconds",
			Help: "Exclusive record-time deadline of the retiring wormhole-core package id (Unix seconds); 0 outside an upgrade window",
		})
)

// Warning texts for an open upgrade window and a retiring id past its deadline.
const (
	upgradeWindowOpenMessage      = "canton core upgrade window is open: the retiring core package id is accepted before its record-time deadline; remove it at a later restart"
	retiringDeadlinePassedMessage = "canton retiring core package deadline has passed: only pre-deadline messages of the retiring id are accepted; remove it at a later restart"
)

// maxHeartbeatContractAddressLen is "current,retiring": two package ids and a comma.
const maxHeartbeatContractAddressLen = 2*cantonclient.PackageIDHexLen + 1

// reportAcceptedPackageIDs replaces the accepted-package-id gauge series and
// warns when an upgrade window is open, so a window left open stays visible.
// wallClock only selects the past-deadline warning.
func reportAcceptedPackageIDs(logger *zap.Logger, acceptedPackageIDs cantonclient.AcceptedPackageIDs, wallClock time.Time) {
	acceptedPackageIDsGauge.Reset()
	retiringPackageAcceptedUntilGauge.Set(0)
	if acceptedPackageIDs.MatchesAnyPackage() {
		acceptedPackageIDsGauge.WithLabelValues("current", "*").Set(1)
		return
	}
	currentPackageID, _ := acceptedPackageIDs.CurrentPackageID()
	acceptedPackageIDsGauge.WithLabelValues("current", currentPackageID.String()).Set(1)
	retiringPackageID, hasRetiring := acceptedPackageIDs.RetiringPackageID()
	if !hasRetiring {
		return
	}
	acceptedBeforeRecordTime := retiringPackageID.AcceptedBeforeRecordTime()
	acceptedPackageIDsGauge.WithLabelValues("retiring", retiringPackageID.PackageID().String()).Set(1)
	retiringPackageAcceptedUntilGauge.Set(float64(acceptedBeforeRecordTime.Unix()))
	windowFields := []zap.Field{
		zap.Stringer("currentPackageId", currentPackageID),
		zap.Stringer("retiringPackageId", retiringPackageID.PackageID()),
		zap.Time("retiringPackageAcceptedBeforeRecordTime", acceptedBeforeRecordTime),
	}
	logger.Warn(upgradeWindowOpenMessage, windowFields...)
	if !wallClock.Before(acceptedBeforeRecordTime) {
		logger.Warn(retiringDeadlinePassedMessage, windowFields...)
	}
}

// checkAcceptedPackageIDsAllowed rejects an empty set, and the any-package
// wildcard outside unsafe dev mode (a look-alike package could forge messages).
func checkAcceptedPackageIDsAllowed(acceptedPackageIDs cantonclient.AcceptedPackageIDs, unsafeDevMode bool) error {
	if acceptedPackageIDs.MatchesAnyPackage() {
		if !unsafeDevMode {
			return fmt.Errorf("canton: the any-package wildcard is unsafe dev mode only")
		}
		return nil
	}
	if acceptedPackageIDs.IsEmpty() {
		return fmt.Errorf("canton: no accepted core package ids")
	}
	if unsafeDevMode {
		return nil
	}
	for _, pinned := range acceptedPackageIDs.PinnedCorePackages() {
		if pinned.ExpectedVersion.IsZero() {
			return fmt.Errorf("canton: the %s core package id needs an expected version outside unsafe dev mode", pinned.Role)
		}
	}
	return nil
}

// heartbeatContractAddress is "current", "current,retiring", or "*" for the wildcard.
func (e *Watcher) heartbeatContractAddress() string {
	contractAddress := e.acceptedPackageIDs.String()
	if len(contractAddress) > maxHeartbeatContractAddressLen {
		panic("canton heartbeat contract address exceeds two package ids")
	}
	return contractAddress
}

// defaultPackageMetadataRecheckInterval is the vetting metadata recheck period in production.
const defaultPackageMetadataRecheckInterval = 60 * time.Second

// NewWatcher creates a new Canton watcher. Outside unsafe dev mode every
// pinned core package must be in the approved list compiled into guardiand.
func NewWatcher(
	cantonRPC string,
	acceptedPackageIDs cantonclient.AcceptedPackageIDs,
	readAsParty string,
	auth AuthConfig,
	unsafeDevMode bool,
	msgC chan<- *common.MessagePublication,
	obsvReqC <-chan *gossipv1.ObservationRequest,
) (*Watcher, error) {
	return newWatcherWithApprovedList(cantonclient.DefaultApprovedCorePackages(), cantonRPC, acceptedPackageIDs, readAsParty, auth, unsafeDevMode, msgC, obsvReqC)
}

func newWatcherWithApprovedList(
	approvedCorePackages cantonclient.ApprovedCorePackages,
	cantonRPC string,
	acceptedPackageIDs cantonclient.AcceptedPackageIDs,
	readAsParty string,
	auth AuthConfig,
	unsafeDevMode bool,
	msgC chan<- *common.MessagePublication,
	obsvReqC <-chan *gossipv1.ObservationRequest,
) (*Watcher, error) {
	if err := checkAcceptedPackageIDsAllowed(acceptedPackageIDs, unsafeDevMode); err != nil {
		return nil, err
	}
	return &Watcher{
		cantonRPC:                      cantonRPC,
		acceptedPackageIDs:             acceptedPackageIDs,
		readAsParty:                    readAsParty,
		auth:                           auth,
		unsafeDevMode:                  unsafeDevMode,
		msgChan:                        msgC,
		obsvReqC:                       obsvReqC,
		readinessSync:                  common.MustConvertChainIdToReadinessSyncing(vaa.ChainIDCanton),
		packageMetadataRecheckInterval: defaultPackageMetadataRecheckInterval,
		approvedCorePackages:           approvedCorePackages,
	}, nil
}

// PublishMessageChoice is the Emitter choice whose result the watcher observes.
const PublishMessageChoice = publishMessageChoice

// PublishMessageTemplateID is the filter for the core bridge's Emitter template.
func PublishMessageTemplateID(acceptedPackageIDs cantonclient.AcceptedPackageIDs) cantonclient.TemplateID {
	return cantonclient.TemplateID{
		AcceptedPackageIDs: acceptedPackageIDs,
		ModuleName:         publishMessageModule,
		EntityName:         publishMessageEntity,
	}
}

// templateID is the filter shared by the stream and reobservation.
func (e *Watcher) templateID() cantonclient.TemplateID {
	return PublishMessageTemplateID(e.acceptedPackageIDs)
}

// processMessage turns a decoded Canton message event into a
// common.MessagePublication and publishes it to the message channel.
//
// The 32-byte emitter address is derived from the emitter's contract-key
// components (registrar, owner, emitterId) via DeriveEmitterAddress: the guardian
// is the sole source of truth for the address (it is not stored on-ledger), and
// because the owner is part of the preimage a compromised operator cannot forge
// an existing emitter's address. See canton/README.md §4.2.
//
// The VAA timestamp is taken from the transaction's ledger effective time
// (whitepaper 0001/0004: the timestamp is block-derived). The TxID is the
// hex-decoded update id (cantonclient.UpdateIDTxIDLen bytes), identical on
// every participant, so guardians agree on it and reobservation can look it up
// on any participant.
func (e *Watcher) processMessage(logger *zap.Logger, ev cantonclient.CantonMessageEvent, isReobservation bool) {
	if !ev.Message.AcceptedPackageRole.IsKnown() {
		logger.Error("dropping Canton message with an unknown accepted package role",
			zap.Stringer("updateId", ev.UpdateID))
		p2p.DefaultRegistry.AddErrorCount(vaa.ChainIDCanton, 1)
		return
	}

	// Both key parties must be present to derive the address; a missing one means
	// a malformed record we must not turn into an observation.
	if ev.Message.Registrar == "" || ev.Message.Owner == "" {
		logger.Error("dropping Canton message with missing emitter key party",
			zap.String("registrar", ev.Message.Registrar),
			zap.String("owner", ev.Message.Owner),
			zap.Stringer("updateId", ev.UpdateID))
		p2p.DefaultRegistry.AddErrorCount(vaa.ChainIDCanton, 1)
		return
	}

	emitter := DeriveEmitterAddress(ev.Message.Registrar, ev.Message.Owner, ev.Message.EmitterID)

	timestamp := ev.EffectiveAt
	if timestamp.IsZero() {
		// Should not happen for a committed transaction, but never publish a
		// zero timestamp.
		logger.Error("dropping Canton message with zero effective time",
			zap.Stringer("updateId", ev.UpdateID))
		p2p.DefaultRegistry.AddErrorCount(vaa.ChainIDCanton, 1)
		return
	}

	if ev.UpdateID.IsZero() {
		logger.Error("dropping Canton message with zero update id",
			zap.Int64("participantOffset", ev.ParticipantOffset))
		p2p.DefaultRegistry.AddErrorCount(vaa.ChainIDCanton, 1)
		return
	}
	txID := ev.UpdateID.TxID()
	if len(txID) != cantonclient.UpdateIDTxIDLen {
		panic("canton TxID length differs from UpdateIDTxIDLen")
	}

	observation := &common.MessagePublication{
		TxID:             txID,
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

	// Record the resume point before publishing so a restart can never skip an
	// offset the processor already saw. Reobservations replay old offsets and
	// must not move the stream's resume point backward.
	if !isReobservation && ev.ParticipantOffset > e.lastOffset.Load() {
		e.lastOffset.Store(ev.ParticipantOffset)
	}

	e.msgChan <- observation //nolint:channelcheck // The channel to the processor is buffered and shared across chains, if it backs up we should stop processing new observations

	cantonMessagesConfirmed.Inc()
	if isReobservation {
		watchers.ReobservationsByChain.WithLabelValues("canton", "std").Inc()
	} else {
		acceptedPublishMessages.WithLabelValues(ev.Message.AcceptedPackageRole.String()).Inc()
	}

	logger.Info("message observed", observation.ZapFields(
		zap.Stringer("updateId", ev.UpdateID),
		zap.Int64("participantOffset", ev.ParticipantOffset),
		zap.Stringer("acceptedPackageRole", ev.Message.AcceptedPackageRole))...)
}

// handleReobservation fetches the transaction whose update id the
// re-observation request's TxID encodes and re-processes each Wormhole message
// it produced.
func (e *Watcher) handleReobservation(ctx context.Context, logger *zap.Logger, client cantonclient.CantonClient, r *gossipv1.ObservationRequest) {
	// node/pkg/node/reobserve.go enforces a valid uint16 chain id and only
	// writes to this chain's channel; anything else is a programming error.
	if r.ChainId > math.MaxUint16 || vaa.ChainID(r.ChainId) != vaa.ChainIDCanton {
		panic("invalid chain ID")
	}

	updateID, err := cantonclient.UpdateIDFromTxID(r.TxHash)
	if err != nil {
		logger.Error("canton_fetch_obvs_req invalid txID", zap.Error(err))
		p2p.DefaultRegistry.AddErrorCount(vaa.ChainIDCanton, 1)
		return
	}

	txn, err := client.GetTransactionByUpdateID(ctx, updateID, e.templateID(), publishMessageChoice)
	if err != nil {
		logger.Error("canton_fetch_obvs_req failed", zap.Stringer("updateId", updateID), zap.Error(err))
		p2p.DefaultRegistry.AddErrorCount(vaa.ChainIDCanton, 1)
		return
	}
	if txn.UpdateID != updateID {
		logger.Error("canton_fetch_obvs_req returned a different update",
			zap.Stringer("requestedUpdateId", updateID), zap.Stringer("returnedUpdateId", txn.UpdateID))
		p2p.DefaultRegistry.AddErrorCount(vaa.ChainIDCanton, 1)
		return
	}

	for _, msg := range txn.Messages {
		if err := e.checkPublishPackageVerified(ctx, logger, client, msg.AcceptedPackageRole); err != nil {
			logger.Error("canton_fetch_obvs_req dropped a message from an unverified core package",
				zap.Stringer("updateId", updateID), zap.Error(err))
			p2p.DefaultRegistry.AddErrorCount(vaa.ChainIDCanton, 1)
			continue
		}
		e.processMessage(logger, cantonclient.CantonMessageEvent{
			ParticipantOffset: txn.ParticipantOffset,
			UpdateID:          txn.UpdateID,
			EffectiveAt:       txn.EffectiveAt,
			RecordTime:        txn.RecordTime,
			Message:           msg,
		}, true)
	}
}

func (e *Watcher) Run(ctx context.Context) error {
	// Redundant with NewWatcher; covers a Watcher literal.
	if err := checkAcceptedPackageIDsAllowed(e.acceptedPackageIDs, e.unsafeDevMode); err != nil {
		return err
	}
	if e.packageMetadataRecheckInterval <= 0 {
		return fmt.Errorf("canton: the package metadata recheck interval must be positive")
	}

	p2p.DefaultRegistry.SetNetworkStats(vaa.ChainIDCanton, &gossipv1.Heartbeat_Network{
		ContractAddress: e.heartbeatContractAddress(),
	})

	logger := supervisor.Logger(ctx)

	logger.Info("Starting watcher",
		zap.String("watcher_name", "canton"),
		zap.String("cantonRPC", e.cantonRPC),
		zap.Stringer("acceptedPackageIDs", e.acceptedPackageIDs),
		zap.String("readAsParty", e.readAsParty),
		zap.Bool("unsafeDevMode", e.unsafeDevMode),
	)
	reportAcceptedPackageIDs(logger, e.acceptedPackageIDs, time.Now())

	// Use an injected client (e.g. from tests) if present, otherwise create one
	// for the lifetime of this Run. The client is kept local rather than stored
	// on the Watcher so each supervisor restart establishes a fresh connection
	// and the goroutines below cannot observe a closed/nil client at shutdown.
	client := e.cantonClient
	if client == nil {
		grpcClient, err := cantonclient.NewCantonGrpcClient(e.cantonRPC, e.readAsParty, logger, e.cantonTransportCreds(), e.cantonDialOpts(ctx)...)
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

	if err := e.refreshPackageMetadata(ctx, logger, client); err != nil {
		return err
	}

	// First run streams from the current ledger end. Restarted runs resume from
	// the last processed offset: Canton ends streams whose access token expired
	// (~minutes with Keycloak), and resuming from ledger end would silently
	// drop anything published during the reconnect window.
	beginExclusive := ledgerEnd
	if last := e.lastOffset.Load(); last > 0 {
		beginExclusive = last
		logger.Info("resuming update stream from last processed offset",
			zap.Int64("lastOffset", last), zap.Int64("ledgerEnd", ledgerEnd))
	}

	timer := time.NewTicker(time.Second * 5)
	defer timer.Stop()
	packageMetadataRecheckTicker := time.NewTicker(e.packageMetadataRecheckInterval)
	defer packageMetadataRecheckTicker.Stop()

	errC := make(chan error)

	supervisor.Signal(ctx, supervisor.SignalHealthy)
	readiness.SetReady(e.readinessSync)

	// Data pump: stream updates from the chosen resume point onward.
	common.RunWithScissors(ctx, errC, "canton_data_pump", func(ctx context.Context) error {
		eventChan := make(chan cantonclient.CantonMessageEvent, 64)

		subscription, err := client.SubscribeUpdates(ctx, beginExclusive, e.templateID(), publishMessageChoice, eventChan)
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
				// SECURITY: fail closed; the next Run resubscribes before this event.
				if err := e.checkPublishPackageVerified(ctx, logger, client, ev.Message.AcceptedPackageRole); err != nil {
					e.rewindResumeOffsetBefore(ev.ParticipantOffset)
					return fmt.Errorf("canton_data_pump stopped at offset %d: %w", ev.ParticipantOffset, err)
				}
				e.processMessage(logger, ev, false)
			}
		}
	})

	// Ledger-offset poll: report height + readiness every 5s; recheck vetting metadata.
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
						ContractAddress: e.heartbeatContractAddress(),
					})
				}
				readiness.SetReady(e.readinessSync)
			case <-packageMetadataRecheckTicker.C:
				if err := e.refreshPackageMetadata(ctx, logger, client); err != nil {
					if errors.Is(err, errPackageMetadataMismatched) {
						return err
					}
					logger.Error("canton package metadata recheck failed", zap.Error(err))
				}
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
