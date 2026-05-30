// Package xrpl is a minimal fake XRPL WebSocket server and a worker
// that exercises the WS subscribe/read loop. XRPL leaks would surface
// here similarly to EVM (orphaned WS dispatchers across reconnect
// storms).
package xrpl

import (
	"context"
	"net/http"
	"net/http/httptest"
	"strings"
	"sync"
	"sync/atomic"
	"time"

	"github.com/gorilla/websocket"

	"github.com/certusone/wormhole/node/hack/leakharness/fakerpc/common"
)

type Server struct {
	mu           sync.Mutex
	fault        common.Fault
	upgrader     websocket.Upgrader
	srv          *httptest.Server
	openWS       sync.Map
	requestCount int64
}

var Family = common.Family{
	Name: "xrpl",
	NewServer: func(_ uint64) common.FaultableServer {
		return New()
	},
	NewWorker: func() common.Worker {
		return &Worker{}
	},
}

func New() *Server {
	s := &Server{
		upgrader: websocket.Upgrader{
			CheckOrigin: func(r *http.Request) bool { return true },
		},
	}
	s.srv = httptest.NewServer(http.HandlerFunc(s.handle))
	return s
}

func (s *Server) URL() string { return "ws" + strings.TrimPrefix(s.srv.URL, "http") }

func (s *Server) Set(f common.Fault) {
	s.mu.Lock()
	s.fault = f
	s.mu.Unlock()
	if f == common.FaultCloseAllConnections {
		s.closeAllWS()
	}
}

func (s *Server) Stop() {
	s.closeAllWS()
	s.srv.Close()
}

func (s *Server) RequestCount() int64 { return atomic.LoadInt64(&s.requestCount) }

func (s *Server) closeAllWS() {
	s.openWS.Range(func(k, _ any) bool {
		if c, ok := k.(*websocket.Conn); ok {
			_ = c.Close()
		}
		s.openWS.Delete(k)
		return true
	})
}

func (s *Server) handle(w http.ResponseWriter, r *http.Request) {
	conn, err := s.upgrader.Upgrade(w, r, nil)
	if err != nil {
		return
	}
	s.openWS.Store(conn, struct{}{})
	defer func() {
		s.openWS.Delete(conn)
		_ = conn.Close()
	}()
	for {
		_, msg, err := conn.ReadMessage()
		if err != nil {
			return
		}
		atomic.AddInt64(&s.requestCount, 1)

		s.mu.Lock()
		fault := s.fault
		s.mu.Unlock()
		if fault == common.FaultCloseAllConnections {
			return
		}
		_ = msg // ignore content; the worker just needs roundtrip semantics
		// Minimal response indicating subscription accepted.
		_ = conn.WriteMessage(websocket.TextMessage, []byte(`{"type":"response","status":"success","result":{}}`))
	}
}

type Worker struct{}

func (w *Worker) Run(ctx context.Context, url string) {
	dialer := websocket.DefaultDialer
	for {
		if ctx.Err() != nil {
			return
		}
		conn, _, err := dialer.DialContext(ctx, url, nil)
		if err != nil {
			select {
			case <-ctx.Done():
				return
			case <-time.After(50 * time.Millisecond):
			}
			continue
		}
		// Send a subscribe ping, read response, then close — exercises the
		// dial/write/read/close cycle that XRPL watchers run.
		_ = conn.WriteMessage(websocket.TextMessage, []byte(`{"command":"subscribe","streams":["transactions"]}`))
		_, _, _ = conn.ReadMessage()
		_ = conn.Close()
		select {
		case <-ctx.Done():
			return
		case <-time.After(100 * time.Millisecond):
		}
	}
}
