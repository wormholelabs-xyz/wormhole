package harness

import (
	"context"
	"fmt"
	"net"
	"net/http"
	_ "net/http/pprof" // side-effect: registers /debug/pprof/* on http.DefaultServeMux
	"os"
	"runtime"
	"runtime/pprof"
	"time"
)

// CaptureHeap writes an inuse-memory heap profile to `path`. Forces a
// GC first so the profile reflects truly-reachable allocations rather
// than as-yet-uncollected garbage. The resulting file is gzipped
// protobuf consumable by `go tool pprof`.
func CaptureHeap(path string) error {
	f, err := os.Create(path)
	if err != nil {
		return fmt.Errorf("create %q: %w", path, err)
	}
	defer f.Close()
	runtime.GC()
	if err := pprof.WriteHeapProfile(f); err != nil {
		return fmt.Errorf("WriteHeapProfile %q: %w", path, err)
	}
	return nil
}

// CaptureGoroutine writes a goroutine stack profile to `path`. The
// pprof debug level is 0 so the output is the compact protobuf form,
// suitable for `go tool pprof`.
func CaptureGoroutine(path string) error {
	f, err := os.Create(path)
	if err != nil {
		return fmt.Errorf("create %q: %w", path, err)
	}
	defer f.Close()
	prof := pprof.Lookup("goroutine")
	if prof == nil {
		return fmt.Errorf("no goroutine profile registered")
	}
	if err := prof.WriteTo(f, 0); err != nil {
		return fmt.Errorf("WriteTo %q: %w", path, err)
	}
	return nil
}

// StartPprofServer brings up `net/http/pprof` on the given address.
// Bind to a loopback address — anything else exposes a heap dump
// (potential signing-key leak) and a DoS surface to the network.
// Returns a stop function that gracefully shuts the server down with
// a 5-second deadline. If `addr` is empty, this is a no-op and stop
// is harmless.
func StartPprofServer(addr string) (stop func() error, actualAddr string, err error) {
	if addr == "" {
		return func() error { return nil }, "", nil
	}
	ln, err := net.Listen("tcp", addr)
	if err != nil {
		return nil, "", fmt.Errorf("listen %q: %w", addr, err)
	}
	srv := &http.Server{
		Handler:      nil, // use http.DefaultServeMux where net/http/pprof registered
		ReadTimeout:  10 * time.Second,
		WriteTimeout: 60 * time.Second, // pprof CPU profile dumps can be slow
	}
	go func() {
		_ = srv.Serve(ln)
	}()
	stop = func() error {
		ctx, cancel := context.WithTimeout(context.Background(), 5*time.Second)
		defer cancel()
		return srv.Shutdown(ctx)
	}
	return stop, ln.Addr().String(), nil
}
