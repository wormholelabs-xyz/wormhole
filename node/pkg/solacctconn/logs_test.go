package solacctconn

import (
	"context"
	"encoding/json"
	"fmt"
	"net/http"
	"net/http/httptest"
	"strings"
	"sync"
	"testing"
	"time"

	"github.com/coder/websocket"
	"github.com/gagliardetto/solana-go"
	"github.com/stretchr/testify/assert"
	"github.com/stretchr/testify/require"
)

const (
	testLogsReadTimeout = 5 * time.Second

	testAck = `{"jsonrpc":"2.0","result":7,"id":1}`
)

// testLogsTimeouts end a dead subscription within a second.
var testLogsTimeouts = logsTimeouts{
	ackTimeout:   500 * time.Millisecond,
	pingInterval: 100 * time.Millisecond,
	pongTimeout:  500 * time.Millisecond,
}

var (
	testLogsProgram   = solana.PublicKey{0x11}
	testLogsSignature = solana.Signature{1, 2, 3}
)

// notificationFrame is a logsNotification as the cluster sends it.
// omitErrField as the txErr of notificationFrame leaves the "err" key out of the value.
type omitErrField struct{}

func notificationFrame(sig solana.Signature, logs []string, txErr any) string {
	value := map[string]any{"signature": sig.String(), "err": txErr, "logs": logs}
	if _, omit := txErr.(omitErrField); omit {
		delete(value, "err")
	}
	frame := map[string]any{
		"jsonrpc": "2.0",
		"method":  logsNotificationMethod,
		"params": map[string]any{
			"result":       map[string]any{"context": map[string]any{"slot": 42}, "value": value},
			"subscription": 7,
		},
	}
	raw, err := json.Marshal(frame)
	if err != nil {
		panic(err)
	}
	return string(raw)
}

// wsServer serves one logsSubscribe connection. send runs after the subscription frame
// arrives. It returns when the server must close.
type wsServer struct {
	url string

	mu        sync.Mutex
	subscribe string
}

// newWSServer starts a websocket endpoint that hands each accepted connection to serve.
func newWSServer(t *testing.T, serve func(t *testing.T, conn *websocket.Conn, subscribe string)) *wsServer {
	t.Helper()

	srv := &wsServer{}
	httpSrv := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		conn, err := websocket.Accept(w, r, nil)
		if err != nil {
			return
		}
		defer conn.Close(websocket.StatusNormalClosure, "")

		readCtx, cancel := context.WithTimeout(r.Context(), testLogsReadTimeout)
		defer cancel()
		_, raw, err := conn.Read(readCtx)
		if err != nil {
			return
		}
		srv.mu.Lock()
		srv.subscribe = string(raw)
		srv.mu.Unlock()

		serve(t, conn, string(raw))
	}))
	t.Cleanup(httpSrv.Close)

	srv.url = "ws" + strings.TrimPrefix(httpSrv.URL, "http")
	return srv
}

func (s *wsServer) subscribeFrame() string {
	s.mu.Lock()
	defer s.mu.Unlock()
	return s.subscribe
}

// connTo builds a ClientConn whose websocket endpoint is srv.
func connTo(t *testing.T, srv *wsServer) *ClientConn {
	t.Helper()
	conn, err := NewConn("http://127.0.0.1:1", srv.url)
	require.NoError(t, err)
	t.Cleanup(conn.Close)
	return conn
}

// writeFrames sends each frame then blocks until the context ends.
func writeFrames(frames ...string) func(t *testing.T, conn *websocket.Conn, subscribe string) {
	return func(t *testing.T, conn *websocket.Conn, subscribe string) {
		t.Helper()
		ctx, cancel := context.WithTimeout(context.Background(), testLogsReadTimeout)
		defer cancel()
		for _, frame := range frames {
			if err := conn.Write(ctx, websocket.MessageText, []byte(frame)); err != nil {
				return
			}
		}
		<-ctx.Done()
	}
}

func TestSubscribeLogsHandshake(t *testing.T) {
	srv := newWSServer(t, writeFrames(testAck))
	conn := connTo(t, srv)

	ctx, cancel := context.WithCancel(context.Background())
	defer cancel()

	events, err := conn.SubscribeLogs(ctx, testLogsProgram)
	require.NoError(t, err)
	require.NotNil(t, events)

	require.Eventually(t, func() bool { return srv.subscribeFrame() != "" }, testLogsReadTimeout, 10*time.Millisecond)

	want := fmt.Sprintf(`{"jsonrpc":"2.0","id":%d,"method":"logsSubscribe","params":[{"mentions":[%q]},{"commitment":"finalized"}]}`,
		logsSubscribeRequestID, testLogsProgram.String())
	assert.JSONEq(t, want, srv.subscribeFrame())
}

func TestSubscribeLogsFails(t *testing.T) {
	tests := []struct {
		name    string
		live    bool
		program solana.PublicKey
	}{
		{name: "zero program", live: true},
		{name: "dial failure", program: testLogsProgram},
	}

	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			wsURL := "ws://127.0.0.1:1"
			if tt.live {
				wsURL = newWSServer(t, writeFrames()).url
			}
			conn, err := NewConn("http://127.0.0.1:1", wsURL)
			require.NoError(t, err)
			t.Cleanup(conn.Close)

			events, err := conn.SubscribeLogs(context.Background(), tt.program)
			require.Error(t, err)
			assert.Nil(t, events)
		})
	}
}

func TestSubscribeLogsEvents(t *testing.T) {
	sig := testLogsSignature
	logs := []string{"Program " + testLogsProgram.String() + " invoke [1]", "Program data: QUNDREdTVA=="}

	tests := []struct {
		name       string
		txErr      any
		wantFailed bool
	}{
		{name: "one notification after the acknowledgement"},
		{name: "failed transaction is flagged", txErr: map[string]any{"InstructionError": []any{0, map[string]any{"Custom": 7}}}, wantFailed: true},
		{name: "missing err field is flagged", txErr: omitErrField{}, wantFailed: true},
	}

	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			srv := newWSServer(t, writeFrames(testAck, notificationFrame(sig, logs, tt.txErr)))
			conn := connTo(t, srv)

			ctx, cancel := context.WithCancel(context.Background())
			defer cancel()

			events, err := conn.SubscribeLogs(ctx, testLogsProgram)
			require.NoError(t, err)

			select {
			case evt, ok := <-events:
				require.True(t, ok, "channel closed before the event arrived")
				assert.Equal(t, sig, evt.Signature)
				assert.Equal(t, logs, evt.Logs)
				assert.Equal(t, tt.wantFailed, evt.Failed)
			case <-time.After(testLogsReadTimeout):
				t.Fatal("timed out waiting for a log event")
			}
		})
	}
}

// TestSubscribeLogsChannelCloses covers every path that ends the subscription. A failure
// before the acknowledgement is an error from subscribeLogs.
func TestSubscribeLogsChannelCloses(t *testing.T) {
	sig := testLogsSignature

	tests := []struct {
		name      string
		serve     func(t *testing.T, conn *websocket.Conn, subscribe string)
		cancelCtx bool
		// wantErr fails the subscribe call before any channel exists.
		wantErr bool
	}{
		{
			name:    "server closes the connection",
			serve:   func(t *testing.T, conn *websocket.Conn, subscribe string) {},
			wantErr: true,
		},
		{
			name:      "context cancelled",
			serve:     answerPingsAfter(0, testAck),
			cancelCtx: true,
		},
		{
			name:    "malformed frame",
			serve:   writeFrames("not json"),
			wantErr: true,
		},
		{
			name:    "server error reply",
			serve:   writeFrames(`{"jsonrpc":"2.0","error":{"code":-32602,"message":"bad params"},"id":1}`),
			wantErr: true,
		},
		{
			name:  "notification without params",
			serve: writeFrames(testAck, fmt.Sprintf(`{"jsonrpc":"2.0","method":%q}`, logsNotificationMethod)),
		},
		{
			name:  "signature is not base58",
			serve: writeFrames(testAck, strings.Replace(notificationFrame(sig, nil, nil), sig.String(), "0OIl", 1)),
		},
		{
			name:  "log line count past the limit",
			serve: writeFrames(testAck, notificationFrame(sig, overLogLimit(), nil)),
		},
		{
			name:    "no acknowledgement",
			serve:   answerPingsAfter(0),
			wantErr: true,
		},
		{
			name:    "notification before the acknowledgement",
			serve:   writeFrames(notificationFrame(sig, nil, nil)),
			wantErr: true,
		},
		{
			name:    "acknowledgement for another request id",
			serve:   writeFrames(`{"jsonrpc":"2.0","result":7,"id":2}`),
			wantErr: true,
		},
		{
			name:    "acknowledgement without a subscription id",
			serve:   writeFrames(`{"jsonrpc":"2.0","id":1}`),
			wantErr: true,
		},
		{
			name:  "notification for another subscription",
			serve: writeFrames(strings.Replace(testAck, "7", "8", 1), notificationFrame(sig, nil, nil)),
		},
		{
			name:  "frame other than a notification",
			serve: writeFrames(testAck, `{"jsonrpc":"2.0","method":"slotNotification","params":{"result":{},"subscription":7}}`),
		},
		{
			name:  "peer stops answering pings",
			serve: writeFrames(testAck),
		},
	}

	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			srv := newWSServer(t, tt.serve)
			conn := connTo(t, srv)

			ctx, cancel := context.WithCancel(context.Background())
			defer cancel()

			events, err := conn.subscribeLogs(ctx, testLogsProgram, testLogsTimeouts)
			if tt.wantErr {
				require.Error(t, err)
				assert.Nil(t, events)
				return
			}
			require.NoError(t, err)

			if tt.cancelCtx {
				require.Eventually(t, func() bool { return srv.subscribeFrame() != "" }, testLogsReadTimeout, 10*time.Millisecond)
				cancel()
			}

			select {
			case evt, ok := <-events:
				assert.False(t, ok, "expected a closed channel, got event %v", evt)
			case <-time.After(testLogsReadTimeout):
				t.Fatal("timed out waiting for the channel to close")
			}
		})
	}
}

// answerPingsAfter answers pings, sends frames with delay before the second, then blocks until the test ends.
func answerPingsAfter(delay time.Duration, frames ...string) func(t *testing.T, conn *websocket.Conn, subscribe string) {
	return func(t *testing.T, conn *websocket.Conn, subscribe string) {
		t.Helper()
		ctx, cancel := context.WithTimeout(context.Background(), testLogsReadTimeout)
		defer cancel()
		closed := conn.CloseRead(ctx)
		for idx, frame := range frames {
			if idx == 1 {
				time.Sleep(delay)
			}
			if err := conn.Write(ctx, websocket.MessageText, []byte(frame)); err != nil {
				return
			}
		}
		<-closed.Done()
	}
}

// TestSubscribeLogsSurvivesIdle holds a quiet subscription open past many ping intervals.
func TestSubscribeLogsSurvivesIdle(t *testing.T) {
	sig := testLogsSignature
	idle := 3 * (testLogsTimeouts.pingInterval + testLogsTimeouts.pongTimeout)
	require.Less(t, idle, testLogsReadTimeout)

	srv := newWSServer(t, answerPingsAfter(idle, testAck, notificationFrame(sig, nil, nil)))
	conn := connTo(t, srv)

	ctx, cancel := context.WithCancel(context.Background())
	defer cancel()

	events, err := conn.subscribeLogs(ctx, testLogsProgram, testLogsTimeouts)
	require.NoError(t, err)

	select {
	case evt, ok := <-events:
		require.True(t, ok, "subscription closed while idle")
		assert.Equal(t, sig, evt.Signature)
	case <-time.After(testLogsReadTimeout):
		t.Fatal("timed out waiting for the log event")
	}
}
