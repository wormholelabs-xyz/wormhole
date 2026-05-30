// Package sui implements a fake Sui JSON-RPC server and a worker that
// exercises the HTTP-poll lifecycle used by the Sui watcher.
//
// Unlike the EVM family, which imports node/pkg/watchers/evm/connectors
// directly and so detects regressions in real watcher code, this Sui
// worker is a *generic* HTTP poll loop. It exercises Go's net/http
// transport lifecycle (which is also a leak surface) but it does NOT
// import or exercise node/pkg/watchers/sui itself.
//
// Known gap: a re-introduction of the historical hot-retry bug
// (commit 11f66039) in the real sui watcher would NOT be caught here.
// Promoting this worker to drive `(*sui.Watcher).Run` against the
// fake is a deliberate follow-up — it requires either pulling the
// watcher's wiring inside-out (chain config, msg channel, observation
// channel, signer, …) or building a thin shim. Tracked as a future
// improvement on the harness backlog.
package sui

import (
	"bytes"
	"context"
	"encoding/json"
	"io"
	"net"
	"net/http"
	"net/http/httptest"
	"sync"
	"sync/atomic"
	"time"

	"github.com/certusone/wormhole/node/hack/leakharness/fakerpc/common"
)

type Server struct {
	mu               sync.Mutex
	fault            common.Fault
	srv              *httptest.Server
	requestCount     int64
	hijackedConns    []net.Conn
}

var Family = common.Family{
	Name: "sui",
	NewServer: func(_ uint64) common.FaultableServer {
		return New()
	},
	NewWorker: func() common.Worker {
		return &Worker{}
	},
}

func New() *Server {
	s := &Server{}
	s.srv = httptest.NewServer(http.HandlerFunc(s.handle))
	return s
}

func (s *Server) URL() string { return s.srv.URL }

func (s *Server) Set(f common.Fault) {
	s.mu.Lock()
	s.fault = f
	s.mu.Unlock()
}

func (s *Server) Stop() {
	s.mu.Lock()
	for _, c := range s.hijackedConns {
		_ = c.Close()
	}
	s.hijackedConns = nil
	s.mu.Unlock()
	s.srv.Close()
}

func (s *Server) RequestCount() int64 { return atomic.LoadInt64(&s.requestCount) }

func (s *Server) handle(w http.ResponseWriter, r *http.Request) {
	defer r.Body.Close()
	atomic.AddInt64(&s.requestCount, 1)
	_, _ = io.Copy(io.Discard, r.Body)

	s.mu.Lock()
	fault := s.fault
	s.mu.Unlock()

	switch fault {
	case common.FaultCloseAllConnections:
		// Hijack the connection to close it without sending a response.
		// Track the hijacked conn so Stop() can guarantee closure even
		// if the client never noticed the half-close.
		hj, ok := w.(http.Hijacker)
		if ok {
			c, _, err := hj.Hijack()
			if err == nil {
				s.mu.Lock()
				s.hijackedConns = append(s.hijackedConns, c)
				s.mu.Unlock()
				_ = c.Close()
				return
			}
		}
		http.Error(w, "disconnected", http.StatusBadGateway)
	case common.FaultMalformed:
		w.Header().Set("Content-Type", "application/json")
		_, _ = w.Write([]byte("not json"))
	case common.FaultSlow:
		// Interruptible delay so an in-flight slow request does not
		// pin the server goroutine past r.Context().Done.
		select {
		case <-r.Context().Done():
			return
		case <-time.After(2 * time.Second):
		}
		fallthrough
	default:
		w.Header().Set("Content-Type", "application/json")
		// Minimal "result": empty events payload, matching what Sui
		// returns when no events match the filter.
		_ = json.NewEncoder(w).Encode(map[string]any{
			"jsonrpc": "2.0",
			"id":      1,
			"result": map[string]any{
				"data":      []any{},
				"hasNextPage": false,
			},
		})
	}
}

// Worker polls the fake at 50 ms intervals and ignores all
// responses. The tight loop is intentional: it is the shape that
// previously leaked.
type Worker struct{}

func (w *Worker) Run(ctx context.Context, url string) {
	client := &http.Client{Timeout: 5 * time.Second}
	body := []byte(`{"jsonrpc":"2.0","id":1,"method":"suix_queryEvents","params":[]}`)
	for {
		if ctx.Err() != nil {
			return
		}
		req, _ := http.NewRequestWithContext(ctx, "POST", url, bytes.NewReader(body))
		req.Header.Set("Content-Type", "application/json")
		resp, err := client.Do(req)
		if err == nil {
			_, _ = io.Copy(io.Discard, resp.Body)
			_ = resp.Body.Close()
		}
		select {
		case <-ctx.Done():
			return
		case <-time.After(50 * time.Millisecond):
		}
	}
}

