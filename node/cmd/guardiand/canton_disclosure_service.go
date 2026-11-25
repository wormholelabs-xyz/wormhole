package guardiand

import (
	"context"
	"errors"
	"fmt"
	"net"
	"net/http"
	"os"
	"os/signal"
	"syscall"
	"time"

	"github.com/prometheus/client_golang/prometheus"
	"github.com/prometheus/client_golang/prometheus/promhttp"
	"github.com/spf13/cobra"
	"go.uber.org/zap"
	"go.uber.org/zap/zapcore"
	"google.golang.org/grpc"
	"google.golang.org/grpc/credentials"
	"google.golang.org/grpc/credentials/insecure"

	"github.com/certusone/wormhole/node/pkg/cantonclient"
	"github.com/certusone/wormhole/node/pkg/cantondisclosure"
)

// Shutdown bounds of canton-disclosure-service.
const (
	cantonDisclosureShutdownTimeout     = 10 * time.Second
	cantonDisclosureStatusServerTimeout = 5 * time.Second
)

var (
	cantonDisclosureRPC                *string
	cantonDisclosureReadAsParty        *string
	cantonDisclosureAuthTokenURL       *string
	cantonDisclosureAuthClientID       *string
	cantonDisclosureAuthAudience       *string
	cantonDisclosurePlaintext          *bool
	cantonDisclosureListenAddress      *string
	cantonDisclosureStatusAddress      *string
	cantonDisclosureIndexByteCapBytes  *int64
	cantonDisclosureLedgerLagBound     *time.Duration
	cantonDisclosureResnapshotInterval *time.Duration
	cantonDisclosureLogLevel           *string
)

// CantonDisclosureServiceCmd serves the explicit-disclosure blobs of the core contracts (canton/README.md section 4.7).
var CantonDisclosureServiceCmd = &cobra.Command{
	Use:   "canton-disclosure-service",
	Short: "Serve the explicit-disclosure blobs of the Canton core contracts over HTTP",
	RunE:  runCantonDisclosureService,
	Args:  cobra.NoArgs,
}

func init() {
	flags := CantonDisclosureServiceCmd.Flags()
	cantonDisclosureRPC = flags.String("cantonRPC", "", "Canton Ledger API v2 gRPC endpoint (host:port)")
	cantonDisclosureReadAsParty = flags.String("cantonReadAsParty", "", "guardianGovernance party id to read as (<hint>::1220<64 lowercase hex>)")
	cantonDisclosureAuthTokenURL = flags.String("cantonAuthTokenURL", "", "OAuth2 token endpoint (https only); the client secret is read from GUARDIAND_CANTONAUTHCLIENTSECRET")
	cantonDisclosureAuthClientID = flags.String("cantonAuthClientID", "", "OAuth2 client id")
	cantonDisclosureAuthAudience = flags.String("cantonAuthAudience", "", "Optional OAuth2 audience: the Ledger API target audience of the participant")
	cantonDisclosurePlaintext = flags.Bool("unsafePlaintext", false, "Dial the Ledger API without TLS (local sandbox only)")
	cantonDisclosureListenAddress = flags.String("listenAddress", "", "HTTP listen address of the disclosure routes (host:port). Put an edge proxy for TLS and rate limits in front of it")
	cantonDisclosureStatusAddress = flags.String("statusAddr", "127.0.0.1:6061", "Listen address of /metrics (host:port); empty disables it. Do not expose it through the edge proxy")
	cantonDisclosureIndexByteCapBytes = flags.Int64("indexByteCapBytes", cantondisclosure.ProductionIndexByteCapBytes, "Byte cap of the in-memory index: blobs plus a fixed overhead per contract. At the cap the index drops every ReplayNode and serves the singletons only")
	cantonDisclosureLedgerLagBound = flags.Duration("ledgerLagBound", cantondisclosure.DefaultLedgerLagBound, "Largest ledger lag at which /v1/readyz answers 200, in [10s, 1h]")
	cantonDisclosureResnapshotInterval = flags.Duration("resnapshotInterval", cantondisclosure.ProductionResnapshotInterval, "Interval of the snapshot comparison with the live index, in [1m, 168h]")
	cantonDisclosureLogLevel = flags.String("logLevel", "info", "Log level (debug, info, warn, error)")
	for _, requiredFlag := range []string{"cantonRPC", "cantonReadAsParty", "listenAddress"} {
		if err := CantonDisclosureServiceCmd.MarkFlagRequired(requiredFlag); err != nil {
			panic(err)
		}
	}
}

// cantonDisclosureServiceFlags are the raw inputs of canton-disclosure-service.
type cantonDisclosureServiceFlags struct {
	cantonRPC          string
	readAsParty        string
	oauth              cantonOAuthFlags
	unsafePlaintext    bool
	listenAddress      string
	statusAddress      string
	indexByteCapBytes  int64
	ledgerLagBound     time.Duration
	resnapshotInterval time.Duration
	logLevel           string
}

// cantonDisclosureServiceConfig is the checked configuration. statusAddress is empty when /metrics is off.
type cantonDisclosureServiceConfig struct {
	cantonRPC              string
	readScope              cantonclient.ReadScope
	guardianGovernance     cantonclient.PartyID
	oauthClientCredentials *cantonclient.OAuthClientCredentials
	unsafePlaintext        bool
	listenAddress          string
	statusAddress          string
	indexByteCapBytes      int64
	ledgerLagBound         time.Duration
	resnapshotInterval     time.Duration
	writerTiming           cantondisclosure.IndexWriterTiming
	logLevel               zapcore.Level
}

// checkHostPortAddress requires host:port with a non-empty port.
func checkHostPortAddress(address string) error {
	_, port, err := net.SplitHostPort(address)
	if err != nil {
		return err
	}
	if port == "" {
		return errors.New("missing port")
	}
	return nil
}

// parseCantonDisclosureServiceFlags fails closed on each invalid or inconsistent input.
func parseCantonDisclosureServiceFlags(flags cantonDisclosureServiceFlags) (cantonDisclosureServiceConfig, error) {
	if err := checkHostPortAddress(flags.cantonRPC); err != nil {
		return cantonDisclosureServiceConfig{}, fmt.Errorf("--cantonRPC %q: %w", flags.cantonRPC, err)
	}
	readScope, err := cantonclient.ParseReadScope(flags.readAsParty, false)
	if err != nil {
		return cantonDisclosureServiceConfig{}, fmt.Errorf("--cantonReadAsParty: %w", err)
	}
	guardianGovernance, isGuardianGovernance := readScope.GuardianGovernanceParty()
	if !isGuardianGovernance {
		return cantonDisclosureServiceConfig{}, errors.New("--cantonReadAsParty: the disclosure service reads as the guardianGovernance party only")
	}
	oauthClientCredentials, err := parseCantonOAuthClientCredentials(flags.oauth, flags.unsafePlaintext)
	if err != nil {
		return cantonDisclosureServiceConfig{}, err
	}
	if err := checkHostPortAddress(flags.listenAddress); err != nil {
		return cantonDisclosureServiceConfig{}, fmt.Errorf("--listenAddress %q: %w", flags.listenAddress, err)
	}
	if flags.statusAddress != "" {
		if err := checkHostPortAddress(flags.statusAddress); err != nil {
			return cantonDisclosureServiceConfig{}, fmt.Errorf("--statusAddr %q: %w", flags.statusAddress, err)
		}
		if flags.statusAddress == flags.listenAddress {
			return cantonDisclosureServiceConfig{}, errors.New("--statusAddr must differ from --listenAddress")
		}
	}
	if _, err := cantondisclosure.NewCoreContractIndex(guardianGovernance, flags.indexByteCapBytes); err != nil {
		return cantonDisclosureServiceConfig{}, fmt.Errorf("--indexByteCapBytes: %w", err)
	}
	if err := cantondisclosure.CheckLedgerLagBound(flags.ledgerLagBound); err != nil {
		return cantonDisclosureServiceConfig{}, fmt.Errorf("--ledgerLagBound: %w", err)
	}
	if err := cantondisclosure.CheckProductionResnapshotInterval(flags.resnapshotInterval); err != nil {
		return cantonDisclosureServiceConfig{}, fmt.Errorf("--resnapshotInterval: %w", err)
	}
	logLevel, err := zapcore.ParseLevel(flags.logLevel)
	if err != nil {
		return cantonDisclosureServiceConfig{}, fmt.Errorf("--logLevel: %w", err)
	}
	return cantonDisclosureServiceConfig{
		cantonRPC:              flags.cantonRPC,
		readScope:              readScope,
		guardianGovernance:     guardianGovernance,
		oauthClientCredentials: oauthClientCredentials,
		unsafePlaintext:        flags.unsafePlaintext,
		listenAddress:          flags.listenAddress,
		statusAddress:          flags.statusAddress,
		indexByteCapBytes:      flags.indexByteCapBytes,
		ledgerLagBound:         flags.ledgerLagBound,
		resnapshotInterval:     flags.resnapshotInterval,
		writerTiming:           cantondisclosure.ProductionIndexWriterTiming(flags.resnapshotInterval),
		logLevel:               logLevel,
	}, nil
}

func runCantonDisclosureService(cmd *cobra.Command, _ []string) error {
	config, err := parseCantonDisclosureServiceFlags(cantonDisclosureServiceFlags{
		cantonRPC:   *cantonDisclosureRPC,
		readAsParty: *cantonDisclosureReadAsParty,
		oauth: cantonOAuthFlags{
			tokenURL:     *cantonDisclosureAuthTokenURL,
			clientID:     *cantonDisclosureAuthClientID,
			clientSecret: os.Getenv("GUARDIAND_CANTONAUTHCLIENTSECRET"),
			audience:     *cantonDisclosureAuthAudience,
		},
		unsafePlaintext:    *cantonDisclosurePlaintext,
		listenAddress:      *cantonDisclosureListenAddress,
		statusAddress:      *cantonDisclosureStatusAddress,
		indexByteCapBytes:  *cantonDisclosureIndexByteCapBytes,
		ledgerLagBound:     *cantonDisclosureLedgerLagBound,
		resnapshotInterval: *cantonDisclosureResnapshotInterval,
		logLevel:           *cantonDisclosureLogLevel,
	})
	if err != nil {
		return err
	}
	loggerConfig := zap.NewProductionConfig()
	loggerConfig.Level = zap.NewAtomicLevelAt(config.logLevel)
	logger, err := loggerConfig.Build()
	if err != nil {
		return err
	}
	defer func() { _ = logger.Sync() }()

	ctx, stopOnSignal := signal.NotifyContext(cmd.Context(), syscall.SIGINT, syscall.SIGTERM)
	defer stopOnSignal()

	var transportCreds credentials.TransportCredentials
	if config.unsafePlaintext {
		transportCreds = insecure.NewCredentials()
	}
	var dialOpts []grpc.DialOption
	if config.oauthClientCredentials != nil {
		dialOpts = append(dialOpts, cantonclient.NewOAuthDialOption(ctx, *config.oauthClientCredentials))
	}
	reader, err := cantonclient.NewCoreContractReader(config.cantonRPC, config.readScope, cantonclient.DefaultApprovedCorePackages(), transportCreds, dialOpts...)
	if err != nil {
		return err
	}
	defer reader.Close()

	listener, err := net.Listen("tcp", config.listenAddress)
	if err != nil {
		return fmt.Errorf("--listenAddress: %w", err)
	}
	var statusListener net.Listener
	if config.statusAddress != "" {
		statusListener, err = net.Listen("tcp", config.statusAddress)
		if err != nil {
			_ = listener.Close()
			return fmt.Errorf("--statusAddr: %w", err)
		}
	}
	logger.Info("canton disclosure service starting",
		zap.String("cantonRPC", config.cantonRPC),
		zap.String("guardianGovernance", config.guardianGovernance.String()),
		zap.String("listenAddress", listener.Addr().String()),
		zap.Int64("indexByteCapBytes", config.indexByteCapBytes),
		zap.Duration("ledgerLagBound", config.ledgerLagBound),
		zap.Duration("resnapshotInterval", config.resnapshotInterval),
	)
	return serveCantonDisclosure(ctx, config, reader, listener, statusListener, logger)
}

// cantonDisclosureComponents are the parts of one service run. metricsRegistry holds the disclosure metrics.
type cantonDisclosureComponents struct {
	writer          *cantondisclosure.CoreContractIndexWriter
	handler         *cantondisclosure.DisclosureHTTPHandler
	metricsRegistry *prometheus.Registry
}

func newCantonDisclosureComponents(config cantonDisclosureServiceConfig, reader cantondisclosure.CoreContractLedgerReader, logger *zap.Logger) (cantonDisclosureComponents, error) {
	served, err := cantondisclosure.NewServedIndex(config.ledgerLagBound)
	if err != nil {
		return cantonDisclosureComponents{}, err
	}
	metricsRegistry, err := served.NewMetricsRegistry()
	if err != nil {
		return cantonDisclosureComponents{}, err
	}
	writer, err := cantondisclosure.NewCoreContractIndexWriter(reader, served, config.guardianGovernance, config.indexByteCapBytes, config.writerTiming, logger)
	if err != nil {
		return cantonDisclosureComponents{}, err
	}
	handler, err := cantondisclosure.NewDisclosureHTTPHandler(served, cantondisclosure.DefaultMaxConcurrentLookupRequests, logger)
	if err != nil {
		return cantonDisclosureComponents{}, err
	}
	return cantonDisclosureComponents{writer: writer, handler: handler, metricsRegistry: metricsRegistry}, nil
}

// serveCantonDisclosure runs the index writer, the disclosure server and the status server until ctx
// ends or one of them fails, then shuts the servers down within cantonDisclosureShutdownTimeout.
// statusListener is nil when /metrics is off.
func serveCantonDisclosure(ctx context.Context, config cantonDisclosureServiceConfig, reader cantondisclosure.CoreContractLedgerReader, listener net.Listener, statusListener net.Listener, logger *zap.Logger) error {
	components, err := newCantonDisclosureComponents(config, reader, logger)
	if err != nil {
		_ = listener.Close()
		if statusListener != nil {
			_ = statusListener.Close()
		}
		return err
	}
	writer := components.writer
	disclosureServer := cantondisclosure.NewDisclosureHTTPServer(listener.Addr().String(), components.handler)
	var statusServer *http.Server
	if statusListener != nil {
		statusRouter := http.NewServeMux()
		statusRouter.Handle("/metrics", promhttp.HandlerFor(components.metricsRegistry, promhttp.HandlerOpts{}))
		statusServer = &http.Server{
			Handler:           statusRouter,
			ReadHeaderTimeout: cantonDisclosureStatusServerTimeout,
			ReadTimeout:       cantonDisclosureStatusServerTimeout,
			WriteTimeout:      cantonDisclosureStatusServerTimeout,
		}
	}

	writerCtx, cancelWriter := context.WithCancel(ctx)
	defer cancelWriter()
	const componentCount = 3
	componentFailures := make(chan error, componentCount)
	writerDone := make(chan struct{})
	go func() {
		defer close(writerDone)
		runErr := writer.Run(writerCtx)
		if runErr == nil && writerCtx.Err() == nil {
			runErr = errors.New("index writer stopped")
		}
		if runErr != nil {
			componentFailures <- fmt.Errorf("index writer: %w", runErr)
		}
	}()
	go func() {
		if serveErr := disclosureServer.Serve(listener); !errors.Is(serveErr, http.ErrServerClosed) {
			componentFailures <- fmt.Errorf("disclosure server: %w", serveErr)
		}
	}()
	if statusServer != nil {
		go func() {
			if serveErr := statusServer.Serve(statusListener); !errors.Is(serveErr, http.ErrServerClosed) {
				componentFailures <- fmt.Errorf("status server: %w", serveErr)
			}
		}()
	}

	var componentErr error
	select {
	case <-ctx.Done():
		logger.Info("canton disclosure service shutting down")
	case componentErr = <-componentFailures:
		logger.Error("canton disclosure service component failed", zap.Error(componentErr))
	}
	cancelWriter()
	shutdownCtx, cancelShutdown := context.WithTimeout(context.Background(), cantonDisclosureShutdownTimeout)
	defer cancelShutdown()
	if err := disclosureServer.Shutdown(shutdownCtx); err != nil {
		logger.Warn("disclosure server shutdown", zap.Error(err))
	}
	if statusServer != nil {
		if err := statusServer.Shutdown(shutdownCtx); err != nil {
			logger.Warn("status server shutdown", zap.Error(err))
		}
	}
	select {
	case <-writerDone:
	case <-shutdownCtx.Done():
		logger.Warn("index writer did not stop before the shutdown timeout")
	}
	return componentErr
}
