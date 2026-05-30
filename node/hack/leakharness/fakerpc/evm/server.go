// Package evm provides a minimal in-process Ethereum-compatible
// JSON-RPC server for the leak harness. It speaks just enough of the
// Ethereum JSON-RPC protocol to let a guardian EVM watcher complete
// `verifyEvmChainID` and stay sufficiently happy across a few seconds
// of activity.
//
// The server is *not* a faithful emulator: it does not produce realistic
// chain state, gossip blocks, or honour finality semantics. Its purpose
// is purely to exercise the connector lifecycle — dial, request, error,
// close — which is where the leak class we are guarding against lives.
//
// Fault injection is exposed via `Set(fault FaultMode)`; Phase 2 wires
// those to behaviour changes (close all websockets, freeze the height,
// return malformed JSON, etc.).
package evm

import (
	"encoding/json"
	"fmt"
	"net/http"
	"net/http/httptest"
	"strings"
	"sync"
	"sync/atomic"
	"testing"
	"time"

	"github.com/gorilla/websocket"

	"github.com/certusone/wormhole/node/hack/leakharness/fakerpc/common"
)

// FaultMode controls how the server responds to requests.
type FaultMode int

const (
	ModeHealthy FaultMode = iota
	// ModeCloseAllWS instructs newly accepted WebSocket connections to
	// be closed immediately after the handshake, and currently-active
	// connections to be dropped on the next request. Use this to simulate
	// the mezo testnet flap.
	ModeCloseAllWS
	// ModeMalformed returns valid HTTP 200 but garbage JSON-RPC bodies.
	ModeMalformed
	// ModeSlow holds responses for `slowDelay` before answering. Set
	// `SetSlowDelay` for the actual duration.
	ModeSlow
	// ModeStuck answers `eth_blockNumber` with the same value forever.
	ModeStuck
)

// Server is a controllable JSON-RPC endpoint. The zero value is unusable;
// construct with New.
type Server struct {
	chainID uint64

	mu        sync.Mutex
	mode      FaultMode
	slowDelay int64 // ns, atomic
	stuckHt   uint64

	upgrader websocket.Upgrader

	openWS sync.Map // *websocket.Conn -> struct{}

	srv *httptest.Server

	requestCount int64
}

// New constructs an EVM fake RPC for the given chain ID and starts
// listening. Closure (via Stop or t.Cleanup) is the caller's job.
func New(chainID uint64) *Server {
	s := &Server{
		chainID: chainID,
		mode:    ModeHealthy,
		stuckHt: 0x1000,
		upgrader: websocket.Upgrader{
			CheckOrigin: func(r *http.Request) bool { return true },
		},
	}
	s.srv = httptest.NewServer(http.HandlerFunc(s.handle))
	return s
}

// NewT is the testing.T-aware constructor that registers t.Cleanup.
func NewT(t *testing.T, chainID uint64) *Server {
	t.Helper()
	s := New(chainID)
	t.Cleanup(s.Stop)
	return s
}

// Stop tears the server down and closes any active WebSocket
// connections.
func (s *Server) Stop() {
	s.closeAllWS()
	s.srv.Close()
}

// URL returns the ws:// URL of the fake. Suitable for passing to
// `rpc.DialContext`.
func (s *Server) URL() string {
	return "ws" + strings.TrimPrefix(s.srv.URL, "http")
}

// Set switches the server into a new fault mode. Subsequent requests
// honour the new mode. This is the FaultableServer interface
// implementation used by the harness.
func (s *Server) Set(f common.Fault) {
	s.setMode(translateFault(f))
}

// SetMode is the EVM-native equivalent of Set, taking an internal
// FaultMode directly. Useful for fine-grained EVM-only tests.
func (s *Server) SetMode(m FaultMode) {
	s.setMode(m)
}

func (s *Server) setMode(m FaultMode) {
	s.mu.Lock()
	defer s.mu.Unlock()
	s.mode = m
	if m == ModeCloseAllWS {
		s.closeAllWSLocked()
	}
}

func translateFault(f common.Fault) FaultMode {
	switch f {
	case common.FaultNone:
		return ModeHealthy
	case common.FaultCloseAllConnections:
		return ModeCloseAllWS
	case common.FaultMalformed:
		return ModeMalformed
	case common.FaultSlow:
		return ModeSlow
	case common.FaultStuck:
		return ModeStuck
	default:
		return ModeHealthy
	}
}

// SetSlowDelay configures the artificial delay applied in ModeSlow.
func (s *Server) SetSlowDelay(ns int64) {
	atomic.StoreInt64(&s.slowDelay, ns)
}

// RequestCount returns the number of JSON-RPC requests served since
// startup.
func (s *Server) RequestCount() int64 {
	return atomic.LoadInt64(&s.requestCount)
}

func (s *Server) mode_() FaultMode {
	s.mu.Lock()
	defer s.mu.Unlock()
	return s.mode
}

func (s *Server) closeAllWS() {
	s.mu.Lock()
	defer s.mu.Unlock()
	s.closeAllWSLocked()
}

func (s *Server) closeAllWSLocked() {
	s.openWS.Range(func(k, _ interface{}) bool {
		if c, ok := k.(*websocket.Conn); ok {
			_ = c.Close()
		}
		s.openWS.Delete(k)
		return true
	})
}

func (s *Server) handle(w http.ResponseWriter, r *http.Request) {
	if strings.EqualFold(r.Header.Get("Upgrade"), "websocket") {
		s.serveWS(w, r)
		return
	}
	s.serveHTTP(w, r)
}

func (s *Server) serveHTTP(w http.ResponseWriter, r *http.Request) {
	defer r.Body.Close()
	var req jsonRPCReq
	if err := json.NewDecoder(r.Body).Decode(&req); err != nil {
		http.Error(w, "bad request", http.StatusBadRequest)
		return
	}
	atomic.AddInt64(&s.requestCount, 1)
	resp := s.dispatch(req)
	w.Header().Set("Content-Type", "application/json")
	_ = json.NewEncoder(w).Encode(resp)
}

func (s *Server) serveWS(w http.ResponseWriter, r *http.Request) {
	conn, err := s.upgrader.Upgrade(w, r, nil)
	if err != nil {
		return
	}
	if s.mode_() == ModeCloseAllWS {
		_ = conn.Close()
		return
	}
	s.openWS.Store(conn, struct{}{})
	defer func() {
		s.openWS.Delete(conn)
		_ = conn.Close()
	}()

	for {
		var req jsonRPCReq
		if err := conn.ReadJSON(&req); err != nil {
			return
		}
		atomic.AddInt64(&s.requestCount, 1)
		// Snapshot the fault mode once per iteration so a mid-handler
		// flip cannot wedge the response state machine.
		mode := s.mode_()
		if mode == ModeCloseAllWS {
			return
		}
		if mode == ModeSlow {
			delay := time.Duration(atomic.LoadInt64(&s.slowDelay))
			if delay > 0 {
				time.Sleep(delay)
			}
		}
		resp := s.dispatch(req)
		if mode == ModeMalformed {
			_ = conn.WriteMessage(websocket.TextMessage, []byte("not json"))
			continue
		}
		if err := conn.WriteJSON(resp); err != nil {
			return
		}
	}
}

type jsonRPCReq struct {
	JSONRPC string          `json:"jsonrpc"`
	Method  string          `json:"method"`
	Params  json.RawMessage `json:"params"`
	ID      json.RawMessage `json:"id"`
}

type jsonRPCResp struct {
	JSONRPC string          `json:"jsonrpc"`
	ID      json.RawMessage `json:"id"`
	Result  interface{}     `json:"result,omitempty"`
	Error   *rpcErr         `json:"error,omitempty"`
}

type rpcErr struct {
	Code    int    `json:"code"`
	Message string `json:"message"`
}

func (s *Server) dispatch(req jsonRPCReq) jsonRPCResp {
	resp := jsonRPCResp{JSONRPC: "2.0", ID: req.ID}
	switch req.Method {
	case "eth_chainId":
		resp.Result = fmt.Sprintf("0x%x", s.chainID)
	case "net_version":
		resp.Result = fmt.Sprintf("%d", s.chainID)
	case "eth_blockNumber":
		resp.Result = fmt.Sprintf("0x%x", s.stuckHt)
	default:
		resp.Error = &rpcErr{Code: -32601, Message: "method not found: " + req.Method}
	}
	return resp
}
