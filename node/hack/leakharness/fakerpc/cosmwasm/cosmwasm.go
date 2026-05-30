// Package cosmwasm is a minimal fake Tendermint RPC server and a
// polling worker for the leak harness. The cosmwasm watcher polls
// `/abci_info` and `/block_results` over HTTP; this fake answers both
// with a tiny canned payload.
package cosmwasm

import (
	"bytes"
	"context"
	"io"
	"net"
	"net/http"
	"net/http/httptest"
	"strings"
	"sync"
	"sync/atomic"
	"time"

	"github.com/certusone/wormhole/node/hack/leakharness/fakerpc/common"
)

type Server struct {
	mu            sync.Mutex
	fault         common.Fault
	srv           *httptest.Server
	requestCount  int64
	hijackedConns []net.Conn
}

var Family = common.Family{
	Name: "cosmwasm",
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

const abciInfoResponse = `{"jsonrpc":"2.0","id":1,"result":{"response":{"last_block_height":"42","last_block_app_hash":""}}}`
const blockResultsResponse = `{"jsonrpc":"2.0","id":1,"result":{"height":"42","txs_results":[],"end_block_events":[]}}`

func (s *Server) handle(w http.ResponseWriter, r *http.Request) {
	defer r.Body.Close()
	atomic.AddInt64(&s.requestCount, 1)
	_, _ = io.Copy(io.Discard, r.Body)

	s.mu.Lock()
	fault := s.fault
	s.mu.Unlock()

	switch fault {
	case common.FaultCloseAllConnections:
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
		select {
		case <-r.Context().Done():
			return
		case <-time.After(2 * time.Second):
		}
		fallthrough
	default:
		w.Header().Set("Content-Type", "application/json")
		switch {
		case strings.Contains(r.URL.Path, "block_results"):
			_, _ = w.Write([]byte(blockResultsResponse))
		default:
			_, _ = w.Write([]byte(abciInfoResponse))
		}
	}
}

type Worker struct{}

func (w *Worker) Run(ctx context.Context, url string) {
	client := &http.Client{Timeout: 5 * time.Second}
	body := []byte(`{}`)
	for {
		if ctx.Err() != nil {
			return
		}
		req, _ := http.NewRequestWithContext(ctx, "POST", url+"/abci_info", bytes.NewReader(body))
		resp, err := client.Do(req)
		if err == nil {
			_, _ = io.Copy(io.Discard, resp.Body)
			_ = resp.Body.Close()
		}
		select {
		case <-ctx.Done():
			return
		case <-time.After(100 * time.Millisecond):
		}
	}
}
