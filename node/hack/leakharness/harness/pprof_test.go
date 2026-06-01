package harness

import (
	"net/http"
	"os"
	"path/filepath"
	"strings"
	"testing"
	"time"

	"github.com/stretchr/testify/require"
)

func TestCaptureHeap_WritesNonEmptyGzippedProfile(t *testing.T) {
	dir := t.TempDir()
	path := filepath.Join(dir, "heap.pprof")

	require.NoError(t, CaptureHeap(path))

	data, err := os.ReadFile(path)
	require.NoError(t, err)
	require.Greater(t, len(data), 0, "heap profile must be non-empty")
	// pprof profiles are gzip-compressed protobufs; verify the magic
	// bytes so we know we wrote a valid file, not just an empty stub.
	require.GreaterOrEqual(t, len(data), 2)
	require.Equal(t, byte(0x1f), data[0], "gzip magic byte 0")
	require.Equal(t, byte(0x8b), data[1], "gzip magic byte 1")
}

func TestCaptureGoroutine_WritesNonEmpty(t *testing.T) {
	dir := t.TempDir()
	path := filepath.Join(dir, "goroutine.pprof")

	require.NoError(t, CaptureGoroutine(path))

	info, err := os.Stat(path)
	require.NoError(t, err)
	require.Greater(t, info.Size(), int64(0), "goroutine profile must be non-empty")
}

func TestStartPprofServer_DisabledIsNoop(t *testing.T) {
	stop, addr, err := StartPprofServer("")
	require.NoError(t, err)
	require.Empty(t, addr)
	require.NotNil(t, stop)
	require.NoError(t, stop())
}

func TestStartPprofServer_ServesHeapAndGoroutine(t *testing.T) {
	// 127.0.0.1:0 -> kernel assigns a free ephemeral port so the test
	// is resilient to a developer already running pprof on :6061.
	stop, addr, err := StartPprofServer("127.0.0.1:0")
	require.NoError(t, err)
	defer func() {
		require.NoError(t, stop())
	}()
	require.True(t, strings.HasPrefix(addr, "127.0.0.1:"), "should bind localhost, got %q", addr)

	client := &http.Client{Timeout: 5 * time.Second}

	for _, profile := range []string{"heap", "goroutine"} {
		resp, err := client.Get("http://" + addr + "/debug/pprof/" + profile)
		require.NoError(t, err, profile)
		require.Equal(t, http.StatusOK, resp.StatusCode, profile)
		_ = resp.Body.Close()
	}
}
