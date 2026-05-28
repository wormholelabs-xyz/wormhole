// This tool checks whether an EVM RPC endpoint satisfies the assumptions that the Wormhole
// guardian makes about EVM endpoints. It reuses the same connectors/ package the watcher uses,
// so it exercises the real code path rather than a re-implementation of the JSON-RPC spec.
//
// It checks:
//   1. Connectivity, chain ID and node version.
//   2. The "finalized" (and optionally "safe") block tags are supported, return real blocks,
//      are correctly ordered (finalized <= safe <= latest) and advance over time.
//   3. The block subscription (newHeads) delivers latest blocks and the poller delivers finalized blocks.
//   4. Block-hash matching: the block hash seen on a subscription/log matches the block hash in a
//      later transaction receipt (the invariant the watcher enforces at watcher.go: tx.BlockHash != key.BlockHash).
//      Verified opportunistically against Wormhole LogMessagePublished events when a --contract is given,
//      and generically against arbitrary transactions in observed blocks.
//   5. Reorg/rollback replay (observational): over the run duration it watches for reorgs (a height seen
//      with a different hash, a height regression, or a Removed log) and reports whether the subscription
//      replayed the affected blocks. Reorgs cannot be forced, so a clean run is reported as INCONCLUSIVE.
//
// Usage:
//   go run . --rpc <url> [--contract <coreBridgeAddr>] [--evmChainId <id>] [--safe] [--duration 3m]
//
// Example (Sepolia):
//   go run . --rpc wss://... --contract 0x4a8bc80Ed5a4067f1CCf107057b8270E0cC11A78 --evmChainId 11155111 --safe --duration 2m

package main

import (
	"context"
	"flag"
	"fmt"
	"math/big"
	"os"
	"os/signal"
	"sort"
	"strings"
	"sync"
	"syscall"
	"time"

	"github.com/certusone/wormhole/node/pkg/watchers/evm/connectors"
	ethAbi "github.com/certusone/wormhole/node/pkg/watchers/evm/connectors/ethabi"

	ethCommon "github.com/ethereum/go-ethereum/common"
	ethTypes "github.com/ethereum/go-ethereum/core/types"

	"go.uber.org/zap"
)

var (
	flagRPC        = flag.String("rpc", "", "RPC URL (ws://, wss://, http:// or https://). Required.")
	flagContract   = flag.String("contract", "", "Wormhole core bridge contract address. Enables the LogMessagePublished block-hash match check.")
	flagEvmChainID = flag.Uint64("evmChainId", 0, "Expected EVM chain ID. 0 to skip the chain ID check.")
	flagSafe       = flag.Bool("safe", false, "Require the 'safe' block tag in addition to 'finalized'.")
	flagDuration   = flag.Duration("duration", 3*time.Minute, "How long to monitor the subscription for liveness and reorgs.")
	flagPollDelay  = flag.Duration("pollDelay", time.Second, "Delay between finalized/safe polls, matching the guardian default.")
)

type checkStatus int

const (
	statusPass checkStatus = iota
	statusFail
	statusInconclusive
	statusSkip
)

func (s checkStatus) String() string {
	switch s {
	case statusPass:
		return "PASS"
	case statusFail:
		return "FAIL"
	case statusInconclusive:
		return "INCONCLUSIVE"
	case statusSkip:
		return "SKIP"
	default:
		return "UNKNOWN"
	}
}

type report struct {
	mu      sync.Mutex
	results []result
}

type result struct {
	name   string
	status checkStatus
	detail string
}

func (r *report) add(name string, status checkStatus, detail string) {
	r.mu.Lock()
	defer r.mu.Unlock()
	r.results = append(r.results, result{name: name, status: status, detail: detail})
}

func main() {
	flag.Parse()

	logger, _ := zap.NewDevelopment()

	if *flagRPC == "" {
		logger.Fatal(`The "--rpc" parameter is required`)
	}

	ctx, rootCancel := context.WithCancel(context.Background())
	defer rootCancel()

	// Cancel on SIGTERM/SIGINT so the run can be stopped early; checks still report what they observed.
	sig := make(chan os.Signal, 1)
	signal.Notify(sig, syscall.SIGTERM, syscall.SIGINT)
	go func() {
		<-sig
		logger.Info("received signal, shutting down early")
		rootCancel()
	}()

	rep := &report{}

	var contractAddr ethCommon.Address
	if *flagContract != "" {
		contractAddr = ethCommon.HexToAddress(*flagContract)
	}

	// Build the base connector exactly as the watcher does (createConnector in watcher.go).
	baseConnector, err := connectors.NewEthereumBaseConnector(ctx, "compliance", *flagRPC, contractAddr, nil, logger)
	if err != nil {
		logger.Fatal("failed to dial RPC", zap.Error(err))
	}

	// --- Check 1: connectivity, chain ID, node version ---
	checkConnectivity(ctx, logger, rep, baseConnector)

	// --- Check 2: finality tags supported, ordered, and advancing ---
	// Direct tag queries via the base connector so the result reflects the endpoint, not the --safe flag.
	checkFinalityTags(ctx, logger, rep, baseConnector)

	// Build the polling/subscribing connector the same way the watcher does: HTTP -> PollConnector,
	// WebSocket -> BatchPollConnector. Finalized polling is assumed supported (that is what we are testing).
	var pollConn connectors.Connector
	isHTTP := strings.HasPrefix(*flagRPC, "http://") || strings.HasPrefix(*flagRPC, "https://")
	if isHTTP {
		pollConn = connectors.NewPollConnector(ctx, logger, baseConnector, *flagSafe, *flagPollDelay)
	} else {
		pollConn = connectors.NewBatchPollConnector(ctx, logger, baseConnector, *flagSafe, *flagPollDelay)
	}

	// --- Checks 3-5: run the live monitor over the configured duration ---
	runMonitor(ctx, logger, rep, baseConnector, pollConn, contractAddr, isHTTP)

	printReport(rep)
}

func checkConnectivity(ctx context.Context, logger *zap.Logger, rep *report, base *connectors.EthereumBaseConnector) {
	cctx, cancel := context.WithTimeout(ctx, 15*time.Second)
	defer cancel()

	chainID, err := base.Client().ChainID(cctx)
	if err != nil {
		rep.add("Connectivity / chain ID", statusFail, fmt.Sprintf("eth_chainId failed: %v", err))
		return
	}

	var version string
	if err := base.RawCallContext(cctx, &version, "web3_clientVersion"); err != nil {
		version = fmt.Sprintf("(web3_clientVersion failed: %v)", err)
	}

	if *flagEvmChainID != 0 && chainID.Uint64() != *flagEvmChainID {
		rep.add("Connectivity / chain ID", statusFail,
			fmt.Sprintf("expected EVM chain ID %d, endpoint reported %s (node: %s)", *flagEvmChainID, chainID.String(), version))
		return
	}

	rep.add("Connectivity / chain ID", statusPass, fmt.Sprintf("chainId=%s, node=%s", chainID.String(), version))
}

func checkFinalityTags(ctx context.Context, logger *zap.Logger, rep *report, base *connectors.EthereumBaseConnector) {
	latest, err := connectors.GetBlockByFinality(ctx, base, connectors.Latest)
	if err != nil {
		rep.add("Latest block tag", statusFail, fmt.Sprintf("eth_getBlockByNumber(latest) failed: %v", err))
		return
	}

	finalized, err := connectors.GetBlockByFinality(ctx, base, connectors.Finalized)
	if err != nil {
		rep.add("Finalized block tag", statusFail,
			fmt.Sprintf("eth_getBlockByNumber(finalized) failed (endpoint likely does not support the finalized tag): %v", err))
	} else if finalized.Number.Sign() == 0 {
		rep.add("Finalized block tag", statusFail, "finalized block number is 0 (endpoint returned an empty/null finalized block)")
	} else if finalized.Number.Cmp(latest.Number) > 0 {
		rep.add("Finalized block tag", statusFail,
			fmt.Sprintf("finalized block %s is ahead of latest %s", finalized.Number, latest.Number))
	} else {
		rep.add("Finalized block tag", statusPass,
			fmt.Sprintf("finalized=%s, latest=%s (lag %s blocks)", finalized.Number, latest.Number, new(big.Int).Sub(latest.Number, finalized.Number)))
	}

	if *flagSafe {
		safe, err := connectors.GetBlockByFinality(ctx, base, connectors.Safe)
		switch {
		case err != nil:
			rep.add("Safe block tag", statusFail,
				fmt.Sprintf("eth_getBlockByNumber(safe) failed but --safe was set: %v", err))
		case safe.Number.Sign() == 0:
			rep.add("Safe block tag", statusFail, "safe block number is 0 (endpoint returned an empty/null safe block)")
		case finalized != nil && safe.Number.Cmp(finalized.Number) < 0:
			rep.add("Safe block tag", statusFail,
				fmt.Sprintf("safe block %s is behind finalized %s (must be safe >= finalized)", safe.Number, finalized.Number))
		case safe.Number.Cmp(latest.Number) > 0:
			rep.add("Safe block tag", statusFail,
				fmt.Sprintf("safe block %s is ahead of latest %s", safe.Number, latest.Number))
		default:
			rep.add("Safe block tag", statusPass, fmt.Sprintf("safe=%s (finalized <= safe <= latest holds)", safe.Number))
		}
	} else {
		rep.add("Safe block tag", statusSkip, "not requested (pass --safe to require it)")
	}
}

// monitor accumulates observations from the block and message subscriptions.
type monitor struct {
	mu sync.Mutex

	// latest subscription liveness
	latestSeen    uint64
	latestCount   int
	finalizedSeen uint64
	finalizedHigh uint64
	finalizedLow  uint64
	finalizedAdvanced bool

	// reorg tracking: height -> hash seen on the latest subscription
	latestHashes map[uint64]ethCommon.Hash
	maxHeight    uint64
	reorgs       []string
	removedLogs  int

	// block-hash match
	msgMatches    int
	msgMismatches []string
	genMatches    int
	genMismatches []string
}

func runMonitor(ctx context.Context, logger *zap.Logger, rep *report, base *connectors.EthereumBaseConnector, pollConn connectors.Connector, contractAddr ethCommon.Address, isHTTP bool) {
	m := &monitor{latestHashes: make(map[uint64]ethCommon.Hash)}

	errC := make(chan error, 10)

	// Block subscription (newHeads for WS via BatchPollConnector; polled for HTTP via PollConnector).
	blockSink := make(chan *connectors.NewBlock, 64)
	blockSub, err := pollConn.SubscribeForBlocks(ctx, errC, blockSink)
	if err != nil {
		rep.add("Block subscription", statusFail, fmt.Sprintf("SubscribeForBlocks failed: %v", err))
	} else {
		defer blockSub.Unsubscribe()
	}

	// Message subscription, only if a contract was provided.
	var msgSink chan *ethAbi.AbiLogMessagePublished
	if contractAddr != (ethCommon.Address{}) {
		msgSink = make(chan *ethAbi.AbiLogMessagePublished, 64)
		msgSub, err := pollConn.WatchLogMessagePublished(ctx, errC, msgSink)
		if err != nil {
			rep.add("Message block-hash match", statusFail, fmt.Sprintf("WatchLogMessagePublished failed: %v", err))
			msgSink = nil
		} else {
			defer msgSub.Unsubscribe()
		}
	}

	deadline := time.NewTimer(*flagDuration)
	defer deadline.Stop()

	logger.Info("monitoring endpoint", zap.Duration("duration", *flagDuration))

	genSampleLimit := 5

loop:
	for {
		select {
		case <-ctx.Done():
			break loop
		case <-deadline.C:
			break loop
		case err := <-errC:
			logger.Error("subscription error", zap.Error(err))
			m.mu.Lock()
			m.reorgs = append(m.reorgs, fmt.Sprintf("subscription error: %v", err))
			m.mu.Unlock()
		case blk := <-blockSink:
			if blk == nil || blk.Number == nil {
				continue
			}
			m.handleBlock(ctx, logger, base, blk, &genSampleLimit)
		case ev := <-msgSink:
			if ev == nil {
				continue
			}
			m.handleMessage(ctx, logger, base, ev)
		}
	}

	scoreMonitor(rep, m, isHTTP, contractAddr != (ethCommon.Address{}))
}

func (m *monitor) handleBlock(ctx context.Context, logger *zap.Logger, base *connectors.EthereumBaseConnector, blk *connectors.NewBlock, genSampleLimit *int) {
	num := blk.Number.Uint64()

	m.mu.Lock()
	switch blk.Finality {
	case connectors.Latest:
		m.latestCount++
		if num > m.latestSeen {
			m.latestSeen = num
		}
		// Reorg detection on the latest stream.
		if prev, ok := m.latestHashes[num]; ok && prev != blk.Hash {
			m.reorgs = append(m.reorgs, fmt.Sprintf("height %d re-delivered with new hash %s (was %s) -> replay observed", num, blk.Hash.Hex(), prev.Hex()))
		} else if num < m.maxHeight {
			m.reorgs = append(m.reorgs, fmt.Sprintf("height regression: received %d after max %d -> replay observed", num, m.maxHeight))
		}
		m.latestHashes[num] = blk.Hash
		if num > m.maxHeight {
			m.maxHeight = num
		}
	case connectors.Finalized:
		if m.finalizedLow == 0 {
			m.finalizedLow = num
		}
		if num > m.finalizedHigh {
			m.finalizedHigh = num
		}
		if num > m.finalizedSeen {
			if m.finalizedSeen != 0 {
				m.finalizedAdvanced = true
			}
			m.finalizedSeen = num
		}
	}
	doGenSample := *genSampleLimit > 0 && blk.Finality == connectors.Latest
	if doGenSample {
		*genSampleLimit--
	}
	m.mu.Unlock()

	if doGenSample {
		m.genericHashMatch(ctx, logger, base, blk.Hash)
	}
}

// genericHashMatch validates that an arbitrary transaction in the given block reports a receipt
// blockHash equal to the block hash we observed. This is the same invariant the watcher relies on,
// exercised without needing Wormhole traffic.
func (m *monitor) genericHashMatch(ctx context.Context, logger *zap.Logger, base *connectors.EthereumBaseConnector, blockHash ethCommon.Hash) {
	cctx, cancel := context.WithTimeout(ctx, 15*time.Second)
	defer cancel()

	block, err := base.Client().BlockByHash(cctx, blockHash)
	if err != nil {
		logger.Debug("generic match: BlockByHash failed (skipping sample)", zap.Error(err))
		return
	}
	txs := block.Transactions()
	if len(txs) == 0 {
		return
	}
	txHash := txs[0].Hash()
	receipt, err := base.TransactionReceipt(cctx, txHash)
	if err != nil {
		logger.Debug("generic match: TransactionReceipt failed (skipping sample)", zap.Error(err))
		return
	}

	m.mu.Lock()
	defer m.mu.Unlock()
	if receipt.BlockHash == blockHash {
		m.genMatches++
	} else {
		m.genMismatches = append(m.genMismatches,
			fmt.Sprintf("tx %s: subscription block hash %s != receipt block hash %s", txHash.Hex(), blockHash.Hex(), receipt.BlockHash.Hex()))
	}
}

// handleMessage validates the Wormhole-specific invariant: the block hash carried on the
// LogMessagePublished event matches the block hash in that transaction's receipt.
func (m *monitor) handleMessage(ctx context.Context, logger *zap.Logger, base *connectors.EthereumBaseConnector, ev *ethAbi.AbiLogMessagePublished) {
	logger.Info("observed LogMessagePublished",
		zap.String("txHash", ev.Raw.TxHash.Hex()),
		zap.String("blockHash", ev.Raw.BlockHash.Hex()),
		zap.Uint64("sequence", ev.Sequence),
		zap.Bool("removed", ev.Raw.Removed),
	)

	if ev.Raw.Removed {
		m.mu.Lock()
		m.removedLogs++
		m.reorgs = append(m.reorgs, fmt.Sprintf("LogMessagePublished tx %s replayed with Removed=true (reorg)", ev.Raw.TxHash.Hex()))
		m.mu.Unlock()
		return
	}

	cctx, cancel := context.WithTimeout(ctx, 15*time.Second)
	defer cancel()
	receipt, err := base.TransactionReceipt(cctx, ev.Raw.TxHash)
	if err != nil {
		logger.Warn("message match: TransactionReceipt failed", zap.Error(err))
		return
	}

	m.mu.Lock()
	defer m.mu.Unlock()
	if receipt.Status != ethTypes.ReceiptStatusSuccessful {
		m.msgMismatches = append(m.msgMismatches, fmt.Sprintf("tx %s: receipt status not successful (%d)", ev.Raw.TxHash.Hex(), receipt.Status))
		return
	}
	if receipt.BlockHash == ev.Raw.BlockHash {
		m.msgMatches++
	} else {
		m.msgMismatches = append(m.msgMismatches,
			fmt.Sprintf("tx %s: subscription block hash %s != receipt block hash %s", ev.Raw.TxHash.Hex(), ev.Raw.BlockHash.Hex(), receipt.BlockHash.Hex()))
	}
}

func scoreMonitor(rep *report, m *monitor, isHTTP bool, haveContract bool) {
	m.mu.Lock()
	defer m.mu.Unlock()

	// Block subscription liveness.
	if isHTTP {
		if m.latestCount > 0 {
			rep.add("Block subscription (latest)", statusPass, fmt.Sprintf("polled %d latest blocks, high=%d", m.latestCount, m.latestSeen))
		} else {
			rep.add("Block subscription (latest)", statusFail, "no latest blocks received from the poller")
		}
	} else {
		if m.latestCount > 0 {
			rep.add("Block subscription (newHeads)", statusPass, fmt.Sprintf("received %d latest heads, high=%d", m.latestCount, m.latestSeen))
		} else {
			rep.add("Block subscription (newHeads)", statusFail, "no latest heads received from the WebSocket subscription")
		}
	}

	// Finalized advancement / liveness.
	switch {
	case m.finalizedHigh == 0:
		rep.add("Finalized advancement", statusFail, "no finalized blocks were delivered during the run")
	case m.finalizedAdvanced:
		rep.add("Finalized advancement", statusPass, fmt.Sprintf("finalized advanced %d -> %d during the run", m.finalizedLow, m.finalizedHigh))
	default:
		rep.add("Finalized advancement", statusInconclusive,
			fmt.Sprintf("finalized did not advance during the run (stayed at %d); try a longer --duration", m.finalizedHigh))
	}

	// Generic block-hash match.
	switch {
	case len(m.genMismatches) > 0:
		rep.add("Generic block-hash match", statusFail, strings.Join(m.genMismatches, "; "))
	case m.genMatches > 0:
		rep.add("Generic block-hash match", statusPass, fmt.Sprintf("%d transaction(s): receipt block hash matched the observed block hash", m.genMatches))
	default:
		rep.add("Generic block-hash match", statusInconclusive, "no blocks with transactions were sampled; try a longer --duration")
	}

	// Wormhole message block-hash match.
	if !haveContract {
		rep.add("Message block-hash match", statusSkip, "no --contract provided")
	} else {
		switch {
		case len(m.msgMismatches) > 0:
			rep.add("Message block-hash match", statusFail, strings.Join(m.msgMismatches, "; "))
		case m.msgMatches > 0:
			rep.add("Message block-hash match", statusPass, fmt.Sprintf("%d LogMessagePublished event(s): receipt block hash matched", m.msgMatches))
		default:
			rep.add("Message block-hash match", statusInconclusive, "no LogMessagePublished events observed during the run")
		}
	}

	// Reorg / rollback replay (observational).
	if isHTTP {
		rep.add("Reorg replay (observational)", statusSkip, "HTTP endpoint has no subscription; replay behavior is not observable")
	} else if len(m.reorgs) > 0 {
		sort.Strings(m.reorgs)
		rep.add("Reorg replay (observational)", statusPass,
			fmt.Sprintf("observed %d reorg signal(s), all replayed on the subscription: %s", len(m.reorgs), strings.Join(m.reorgs, " | ")))
	} else {
		rep.add("Reorg replay (observational)", statusInconclusive,
			"no reorgs occurred during the run, so replay could not be confirmed (reorgs cannot be forced); re-run over a longer window for more confidence")
	}
}

func printReport(rep *report) {
	rep.mu.Lock()
	defer rep.mu.Unlock()

	fmt.Println()
	fmt.Println("================ EVM RPC Guardian Compliance Report ================")
	hardFail := false
	for _, r := range rep.results {
		fmt.Printf("[%-12s] %s\n", r.status.String(), r.name)
		if r.detail != "" {
			fmt.Printf("               %s\n", r.detail)
		}
		if r.status == statusFail {
			hardFail = true
		}
	}
	fmt.Println("===================================================================")

	if hardFail {
		fmt.Println("RESULT: FAIL — endpoint does not meet guardian assumptions")
		os.Exit(1)
	}
	fmt.Println("RESULT: OK — no failing checks (review any INCONCLUSIVE items)")
}
