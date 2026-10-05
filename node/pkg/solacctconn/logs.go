// logsSubscribe over a raw websocket: one hand-rolled JSON-RPC subscription frame, its
// acknowledgement, then notification frames, following node/pkg/watchers/solana/client.go.
// The SDK's rpc/ws client requires modules outside this binary's dependency set.

package solacctconn

import (
	"context"
	"encoding/json"
	"errors"
	"fmt"
	"sync"
	"time"

	"github.com/coder/websocket"
	"github.com/gagliardetto/solana-go"
	"github.com/gagliardetto/solana-go/rpc/jsonrpc"
)

const (
	logsAckTimeout = 30 * time.Second

	// A mentions filter is silent while the program is idle. Thus pings, not notification
	// traffic, show that the connection is live.
	logsPingInterval = 30 * time.Second
	logsPongTimeout  = 30 * time.Second

	logEventBufferLen = 256

	// Agave truncates the log buffer at 10 KiB per transaction. The JSON envelope uses the rest.
	maxLogsFrameBytes = 1 << 20

	// One subscription per connection.
	logsSubscribeRequestID = 1

	logsNotificationMethod = "logsNotification"
)

// logsTimeouts bound each wait of a subscription.
type logsTimeouts struct {
	ackTimeout   time.Duration
	pingInterval time.Duration
	pongTimeout  time.Duration
}

var productionLogsTimeouts = logsTimeouts{
	ackTimeout:   logsAckTimeout,
	pingInterval: logsPingInterval,
	pongTimeout:  logsPongTimeout,
}

type logsSubscribeAck struct {
	ID             *uint64           `json:"id"`
	SubscriptionID *uint64           `json:"result"`
	Error          *jsonrpc.RPCError `json:"error"`
}

type logsNotificationFrame struct {
	Method string                 `json:"method"`
	Error  *jsonrpc.RPCError      `json:"error"`
	Params *logsNotificationParam `json:"params"`
}

type logsNotificationParam struct {
	Result         logsNotificationResult `json:"result"`
	SubscriptionID uint64                 `json:"subscription"`
}

type logsNotificationResult struct {
	Value logsNotificationValue `json:"value"`
}

type logsNotificationValue struct {
	Signature string          `json:"signature"`
	Err       json.RawMessage `json:"err"`
	Logs      []string        `json:"logs"`
}

// SubscribeLogs opens a logsSubscribe subscription on the transactions that mention
// program, at finalized commitment. The channel closes on any read, decode, keepalive or
// context error, and the supervisor restarts the watcher.
func (c *ClientConn) SubscribeLogs(ctx context.Context, program solana.PublicKey) (<-chan LogEvent, error) {
	return c.subscribeLogs(ctx, program, productionLogsTimeouts)
}

func (c *ClientConn) subscribeLogs(ctx context.Context, program solana.PublicKey, timeouts logsTimeouts) (<-chan LogEvent, error) {
	if program.IsZero() {
		return nil, errors.New("logsSubscribe: no program id")
	}

	conn, resp, err := websocket.Dial(ctx, c.wsURL, nil)
	if resp != nil && resp.Body != nil {
		resp.Body.Close()
	}
	if err != nil {
		return nil, fmt.Errorf("logsSubscribe: dial %s: %w", c.wsURL, err)
	}
	conn.SetReadLimit(maxLogsFrameBytes)

	req := fmt.Sprintf(
		`{"jsonrpc":"2.0","id":%d,"method":"logsSubscribe","params":[{"mentions":["%s"]},{"commitment":"finalized"}]}`,
		logsSubscribeRequestID, program,
	)
	if err := conn.Write(ctx, websocket.MessageText, []byte(req)); err != nil {
		conn.Close(websocket.StatusInternalError, "subscribe failed")
		return nil, fmt.Errorf("logsSubscribe: write subscription: %w", err)
	}

	events := make(chan LogEvent, logEventBufferLen)
	go pumpLogs(ctx, conn, events, timeouts)
	return events, nil
}

// pumpLogs reads the acknowledgement, then forwards notifications until a read or decode
// fails. It owns the connection and the channel. Thus it is the only writer and the only
// closer of the channel.
func pumpLogs(ctx context.Context, conn *websocket.Conn, events chan<- LogEvent, timeouts logsTimeouts) {
	pumpCtx, cancel := context.WithCancel(ctx)
	var pinger sync.WaitGroup
	pinger.Add(1)
	go func() {
		defer pinger.Done()
		pingUntilUnresponsive(pumpCtx, conn, timeouts)
	}()

	// Close the channel first. A close handshake with an unresponsive peer blocks. The
	// watcher must see the end of the subscription immediately.
	defer conn.Close(websocket.StatusNormalClosure, "")
	defer pinger.Wait()
	defer cancel()
	defer close(events)

	subscriptionID, err := readLogsSubscribeAck(pumpCtx, conn, timeouts.ackTimeout)
	if err != nil {
		return
	}

	for {
		_, raw, err := conn.Read(pumpCtx)
		if err != nil {
			return
		}

		evt, err := decodeLogsNotification(raw, subscriptionID)
		if err != nil {
			return
		}

		select {
		case events <- evt:
		case <-pumpCtx.Done():
			return
		}
	}
}

// pingUntilUnresponsive closes conn when a pong misses its deadline, which fails the
// pump's pending read. Runs until ctx ends.
func pingUntilUnresponsive(ctx context.Context, conn *websocket.Conn, timeouts logsTimeouts) {
	ticker := time.NewTicker(timeouts.pingInterval)
	defer ticker.Stop()

	for {
		select {
		case <-ctx.Done():
			return
		case <-ticker.C:
		}

		pingCtx, cancel := context.WithTimeout(ctx, timeouts.pongTimeout)
		err := conn.Ping(pingCtx)
		cancel()
		if err != nil {
			_ = conn.CloseNow()
			return
		}
	}
}

// readLogsSubscribeAck returns the subscription id the server assigned.
//
// SECURITY: the acknowledgement must be the first frame. It must arrive within ackTimeout.
// Thus a subscription that the server did not set up ends. Live pings cannot hide it.
func readLogsSubscribeAck(ctx context.Context, conn *websocket.Conn, ackTimeout time.Duration) (uint64, error) {
	ackCtx, cancel := context.WithTimeout(ctx, ackTimeout)
	defer cancel()
	_, raw, err := conn.Read(ackCtx)
	if err != nil {
		return 0, fmt.Errorf("logsSubscribe: read acknowledgement: %w", err)
	}
	return decodeLogsSubscribeAck(raw)
}

func decodeLogsSubscribeAck(raw []byte) (uint64, error) {
	var ack logsSubscribeAck
	if err := json.Unmarshal(raw, &ack); err != nil {
		return 0, fmt.Errorf("logsSubscribe: decode acknowledgement: %w", err)
	}
	if ack.Error != nil {
		return 0, fmt.Errorf("logsSubscribe: server error %d: %s", ack.Error.Code, ack.Error.Message)
	}
	if ack.ID == nil {
		return 0, errors.New("logsSubscribe: first frame is not an acknowledgement")
	}
	if *ack.ID != logsSubscribeRequestID {
		return 0, fmt.Errorf("logsSubscribe: acknowledgement for request %d, want %d", *ack.ID, logsSubscribeRequestID)
	}
	if ack.SubscriptionID == nil {
		return 0, errors.New("logsSubscribe: acknowledgement without a subscription id")
	}
	return *ack.SubscriptionID, nil
}

// decodeLogsNotification turns one frame of subscription subscriptionID into an event.
//
// SECURITY: a frame that the guardian cannot parse ends the subscription. The guardian must
// not skip it, because a skipped commit stalls a transfer until the next audit cycle.
func decodeLogsNotification(raw []byte, subscriptionID uint64) (LogEvent, error) {
	var frame logsNotificationFrame
	if err := json.Unmarshal(raw, &frame); err != nil {
		return LogEvent{}, fmt.Errorf("logsSubscribe: decode frame: %w", err)
	}
	if frame.Error != nil {
		return LogEvent{}, fmt.Errorf("logsSubscribe: server error %d: %s", frame.Error.Code, frame.Error.Message)
	}
	if frame.Method != logsNotificationMethod {
		return LogEvent{}, fmt.Errorf("logsSubscribe: frame method %q, want %q", frame.Method, logsNotificationMethod)
	}
	if frame.Params == nil {
		return LogEvent{}, errors.New("logsSubscribe: notification without params")
	}
	if frame.Params.SubscriptionID != subscriptionID {
		return LogEvent{}, fmt.Errorf("logsSubscribe: notification for subscription %d, want %d", frame.Params.SubscriptionID, subscriptionID)
	}

	value := frame.Params.Result.Value
	if len(value.Logs) > MaxLogLinesPerTx {
		return LogEvent{}, fmt.Errorf("logsSubscribe: %d log lines is past the %d line limit", len(value.Logs), MaxLogLinesPerTx)
	}

	sig, err := solana.SignatureFromBase58(value.Signature)
	if err != nil {
		return LogEvent{}, fmt.Errorf("logsSubscribe: signature %q: %w", value.Signature, err)
	}

	return LogEvent{
		Signature: sig,
		Logs:      value.Logs,
		Failed:    !isJSONNull(value.Err),
	}, nil
}

func isJSONNull(raw json.RawMessage) bool {
	// SECURITY: a missing field counts as not null, so a notification without "err" is failed.
	if len(raw) == 0 {
		return false
	}
	var v any
	if err := json.Unmarshal(raw, &v); err != nil {
		return false
	}
	return v == nil
}
