package aggregator

import (
	"context"
	"fmt"
	_ "net/http/pprof" // #nosec G108 we are using a custom router (`router := mux.NewRouter()`) and thus not automatically expose pprof.
	"os"
	"os/signal"
	"path"
	"runtime"
	"syscall"
	"time"

	"github.com/certusone/wormhole/node/pkg/db"
	"github.com/certusone/wormhole/node/pkg/devnet"
	"github.com/certusone/wormhole/node/pkg/node"
	"github.com/certusone/wormhole/node/pkg/supervisor"
	"github.com/certusone/wormhole/node/pkg/telemetry"
	"github.com/certusone/wormhole/node/pkg/version"

	"github.com/certusone/wormhole/node/pkg/common"
	"github.com/certusone/wormhole/node/pkg/p2p"
	promremotew "github.com/certusone/wormhole/node/pkg/telemetry/prom_remote_write"
	libp2p_crypto "github.com/libp2p/go-libp2p/core/crypto"
	"github.com/libp2p/go-libp2p/core/peer"
	"github.com/spf13/cobra"
	"go.uber.org/zap"
	"go.uber.org/zap/zapcore"

	ipfslog "github.com/ipfs/go-log/v2"
)

var (
	envStr                 *string

	p2pNetworkID *string
	p2pPort      *uint
	p2pBootstrap *string
	nodeKeyPath *string

	publicGRPCSocketPath *string

	dataDir *string

	ethRPC      *string
	ethContract *string

	statusAddr *string

	logLevel                *string
	publicRpcLogDetailStr   *string
	publicRpcLogToTelemetry *bool

	publicRPC *string
	publicWeb *string

	tlsHostname *string
	tlsProdEnv  *bool

	disableTelemetry *bool

	// Loki cloud logging parameters
	telemetryLokiURL  *string
	telemetryNodeName *string

	// Prometheus remote write URL
	promRemoteURL *string

	// This is the externally reachable address advertised over gossip for guardian p2p and ccq p2p.
	gossipAdvertiseAddress *string

	// env is the mode we are running in, Mainnet, Testnet or UnsafeDevnet.
	env common.Environment
)

const DEV_NETWORK_ID = "/wormhole/dev"

func init() {
	envStr = AggregatorCmd.Flags().String("env", "mainnet", "environment (devnet, testnet, mainnet)")

	p2pNetworkID = AggregatorCmd.Flags().String("network", "", "P2P network identifier (optional, overrides default for environment)")
	p2pPort = AggregatorCmd.Flags().Uint("port", p2p.DefaultPort, "P2P UDP listener port")
	p2pBootstrap = AggregatorCmd.Flags().String("bootstrap", "", "P2P bootstrap peers (optional for mainnet or testnet, overrides default, required for unsafeDevMode)")
	nodeKeyPath = AggregatorCmd.Flags().String("nodeKey", "", "Path to node key (will be generated if it doesn't exist)")

	statusAddr = AggregatorCmd.Flags().String("statusAddr", "[::]:6060", "Listen address for status server (disabled if blank)")

	publicGRPCSocketPath = AggregatorCmd.Flags().String("publicGRPCSocket", "", "Public gRPC service UNIX domain socket path")

	dataDir = AggregatorCmd.Flags().String("dataDir", "", "Data directory")

	ethRPC = AggregatorCmd.Flags().String("ethRPC", "https://ethereum-rpc.publicnode.com", "Ethereum RPC URL")
	ethContract = AggregatorCmd.Flags().String("ethContract", "0x98f3c9e6E3fAce36bAAd05FE09d375Ef1464288B", "Ethereum contract address")

	logLevel = AggregatorCmd.Flags().String("logLevel", "info", "Logging level (debug, info, warn, error, dpanic, panic, fatal)")
	publicRpcLogDetailStr = AggregatorCmd.Flags().String("publicRpcLogDetail", "full", "The detail with which public RPC requests shall be logged (none=no logging, minimal=only log gRPC methods, full=log gRPC method, payload (up to 200 bytes) and user agent (up to 200 bytes))")
	publicRpcLogToTelemetry = AggregatorCmd.Flags().Bool("logPublicRpcToTelemetry", true, "whether or not to include publicRpc request logs in telemetry")

	publicRPC = AggregatorCmd.Flags().String("publicRPC", "", "Listen address for public gRPC interface")
	publicWeb = AggregatorCmd.Flags().String("publicWeb", "", "Listen address for public REST and gRPC Web interface")

	tlsHostname = AggregatorCmd.Flags().String("tlsHostname", "", "If set, serve publicWeb as TLS with this hostname using Let's Encrypt")
	tlsProdEnv = AggregatorCmd.Flags().Bool("tlsProdEnv", false,
		"Use the production Let's Encrypt environment instead of staging")

	disableTelemetry = AggregatorCmd.Flags().Bool("disableTelemetry", false,
		"Disable telemetry")

	telemetryLokiURL = AggregatorCmd.Flags().String("telemetryLokiURL", "", "Loki cloud logging URL")
	telemetryNodeName = AggregatorCmd.Flags().String("telemetryNodeName", "", "Node name used in telemetry")

	promRemoteURL = AggregatorCmd.Flags().String("promRemoteURL", "", "Prometheus remote write URL (Grafana)")
	gossipAdvertiseAddress = AggregatorCmd.Flags().String("gossipAdvertiseAddress", "", "External IP to advertize on Guardian and CCQ p2p (use if behind a NAT or running in k8s)")
}

var (
	rootCtx       context.Context
	rootCtxCancel context.CancelFunc
)

// AggregatorCmd represents the aggregator command
var AggregatorCmd = &cobra.Command{
	Use:               "aggregator",
	Short:             "Run the message aggregation service",
	Run:               runAggregator,
}

// This variable may be overridden by the -X linker flag to "dev" in which case
// we enforce the --unsafeDevMode flag. Only development binaries/docker images
// are distributed. Production binaries are required to be built from source by
// guardians to reduce risk from a compromised builder.

func runAggregator(cmd *cobra.Command, args []string) {
	common.SetRestrictiveUmask()

	// Set up logging. The go-log zap wrapper that libp2p uses is compatible with our
	// usage of zap in supervisor, which is nice.
	lvl, err := ipfslog.LevelFromString(*logLevel)
	if err != nil {
		fmt.Println("Invalid log level")
		os.Exit(1)
	}

	logger := zap.New(zapcore.NewCore(
		consoleEncoder{zapcore.NewConsoleEncoder(
			zap.NewDevelopmentEncoderConfig())},
		zapcore.AddSync(zapcore.Lock(os.Stderr)),
		zap.NewAtomicLevelAt(zapcore.Level(lvl))))

	// Override the default go-log config, which uses a magic environment variable.
	ipfslog.SetAllLoggers(lvl)
	
	env, err = common.ParseEnvironment(*envStr)
	if err != nil || (env != common.UnsafeDevNet && env != common.TestNet && env != common.MainNet) {
		if *envStr == "" {
			logger.Fatal("Please specify --env")
		}
		logger.Fatal("Invalid value for --env, should be devnet, testnet or mainnet", zap.String("val", *envStr))
	}

	// In devnet mode, we automatically set a number of flags that rely on deterministic keys.
	if env == common.UnsafeDevNet {
		g0key, err := peer.IDFromPrivateKey(devnet.DeterministicP2PPrivKeyByIndex(0))
		if err != nil {
			panic(err)
		}

		// Use the first guardian node as bootstrap
		if *p2pBootstrap == "" {
			*p2pBootstrap = fmt.Sprintf("/dns4/guardian-0.guardian/udp/%d/quic/p2p/%s", *p2pPort, g0key.String())
		}
		if *p2pNetworkID == "" {
			*p2pNetworkID = p2p.GetNetworkId(env)
		}
	} else { // Mainnet or Testnet.
		// If the network parameters are not specified, use the defaults. Log a warning if they are specified since we want to discourage this.
		// Note that we don't want to prevent it, to allow for network upgrade testing.
		if *p2pNetworkID == "" {
			*p2pNetworkID = p2p.GetNetworkId(env)
		} else {
			logger.Warn("overriding default p2p network ID", zap.String("p2pNetworkID", *p2pNetworkID))
		}
		if *p2pBootstrap == "" {
			*p2pBootstrap, err = p2p.GetBootstrapPeers(env)
			if err != nil {
				logger.Fatal("failed to determine p2p bootstrap peers", zap.String("env", string(env)), zap.Error(err))
			}
		} else {
			logger.Warn("overriding default p2p bootstrap peers", zap.String("p2pBootstrap", *p2pBootstrap))
		}
	}

	// Verify flags

	if (*publicRPC != "" || *publicWeb != "") && *publicGRPCSocketPath == "" {
		logger.Fatal("If either --publicRPC or --publicWeb is specified, --publicGRPCSocket must also be specified")
	}
	if *dataDir == "" {
		logger.Fatal("Please specify --dataDir")
	}

	var publicRpcLogDetail common.GrpcLogDetail
	switch *publicRpcLogDetailStr {
	case "none":
		publicRpcLogDetail = common.GrpcLogDetailNone
	case "minimal":
		publicRpcLogDetail = common.GrpcLogDetailMinimal
	case "full":
		publicRpcLogDetail = common.GrpcLogDetailFull
	default:
		logger.Fatal("--publicRpcLogDetail should be one of (none, minimal, full)")
	}

	// Database
	db := db.OpenDb(logger, dataDir)
	defer db.Close()

	// Load p2p private key
	var p2pKey libp2p_crypto.PrivKey
	p2pKey, err = common.GetOrCreateNodeKey(logger, *nodeKeyPath)
	if err != nil {
		logger.Fatal("Failed to load node key", zap.Error(err))
	}

	// Node's main lifecycle context.
	rootCtx, rootCtxCancel = context.WithCancel(context.Background())
	defer rootCtxCancel()

	// Handle SIGTERM
	sigterm := make(chan os.Signal, 1)
	signal.Notify(sigterm, syscall.SIGTERM)
	go func() {
		<-sigterm
		logger.Info("Received sigterm. exiting.")
		rootCtxCancel()
	}()

	usingLoki := *telemetryLokiURL != ""

	var hasTelemetryCredential bool = usingLoki

	// Telemetry is enabled by default in mainnet/testnet. In devnet it is disabled by default
	if !*disableTelemetry && (env != common.UnsafeDevNet || (env == common.UnsafeDevNet && hasTelemetryCredential)) {
		if !hasTelemetryCredential {
			logger.Fatal("Please specify --telemetryLokiURL or set --disableTelemetry=false")
		}
		if *telemetryNodeName == "" {
			logger.Fatal("if --telemetryLokiURL is specified --telemetryNodeName must be specified")
		}

		// Get libp2p peer ID from private key
		pk := p2pKey.GetPublic()
		peerID, err := peer.IDFromPublicKey(pk)
		if err != nil {
			logger.Fatal("Failed to get peer ID from private key", zap.Error(err))
		}

		labels := map[string]string{
			"node_name":     *telemetryNodeName,
			"node_key":      peerID.String(),
			"network":       *p2pNetworkID,
			"version":       version.Version(),
		}

		skipPrivateLogs := !*publicRpcLogToTelemetry

		var tm *telemetry.Telemetry
		if usingLoki {
			logger.Info("Using Loki telemetry logger",
				zap.String("publicRpcLogDetail", *publicRpcLogDetailStr),
				zap.Bool("logPublicRpcToTelemetry", *publicRpcLogToTelemetry))

			tm, err = telemetry.NewLokiCloudLogger(context.Background(), logger, *telemetryLokiURL, "wormhole", skipPrivateLogs, labels)
			if err != nil {
				logger.Fatal("Failed to initialize telemetry", zap.Error(err))
			}
		}

		defer tm.Close()
		logger = tm.WrapLogger(logger) // Wrap logger with telemetry logger
	}

	// log golang version
	logger.Info("golang version", zap.String("golang_version", runtime.Version()))

	// Redirect ipfs logs to plain zap
	ipfslog.SetPrimaryCore(logger.Core())

	usingPromRemoteWrite := *promRemoteURL != ""
	if usingPromRemoteWrite {
		var info promremotew.PromTelemetryInfo
		info.PromRemoteURL = *promRemoteURL
		info.Labels = map[string]string{
			"node_name":     *telemetryNodeName,
			"network":       *p2pNetworkID,
			"version":       version.Version(),
			"product":       "aggregator",
		}

		promLogger := logger.With(zap.String("component", "prometheus_scraper"))
		errC := make(chan error)
		common.StartRunnable(rootCtx, errC, false, "prometheus_scraper", func(ctx context.Context) error {
			t := time.NewTicker(15 * time.Second)

			for {
				select {
				case <-ctx.Done():
					return nil
				case <-t.C:
					err := promremotew.ScrapeAndSendLocalMetrics(ctx, info, promLogger)
					if err != nil {
						promLogger.Error("ScrapeAndSendLocalMetrics error", zap.Error(err))
						continue
					}
				}
			}
		})
	}
	
	aggregatorNode := node.NewAggregatorNode(
		env,
	)
	
	aggregatorOptions := []*node.GuardianOption{
		node.GuardianOptionDatabase(db),
		node.AggregatorOptionFetchGuardianSet(*ethRPC, *ethContract),
		node.AggregatorOptionP2P(p2pKey, *p2pNetworkID, *p2pBootstrap, *p2pPort, *gossipAdvertiseAddress),
		node.GuardianOptionStatusServer(*statusAddr),
		node.AggregatorOptionProcessor(),
	}
		
	if shouldStart(publicGRPCSocketPath) {
		aggregatorOptions = append(aggregatorOptions, node.GuardianOptionPublicRpcSocket(*publicGRPCSocketPath, publicRpcLogDetail))

		if shouldStart(publicRPC) {
			aggregatorOptions = append(aggregatorOptions, node.GuardianOptionPublicrpcTcpService(*publicRPC, publicRpcLogDetail))
		}

		if shouldStart(publicWeb) {
			aggregatorOptions = append(aggregatorOptions,
				node.GuardianOptionPublicWeb(*publicWeb, *publicGRPCSocketPath, *tlsHostname, *tlsProdEnv, path.Join(*dataDir, "autocert")),
			)
		}
	}


	// Run supervisor with Guardian Node as root.
	supervisor.New(rootCtx, logger, aggregatorNode.RunAggregator(rootCtxCancel, aggregatorOptions...),
		// It's safer to crash and restart the process in case we encounter a panic,
		// rather than attempting to reschedule the runnable.
		supervisor.WithPropagatePanic)

	<-rootCtx.Done()
	logger.Info("root context cancelled, exiting...")
}

func shouldStart(rpc *string) bool {
	return *rpc != "" && *rpc != "none"
}
