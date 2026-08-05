package store

import (
	"path/filepath"
	"testing"

	"github.com/wormhole-foundation/wormhole/canton/party-ceremony/ceremony"
)

func testSpec(t *testing.T) ceremony.Spec {
	t.Helper()
	spec, err := ceremony.NewOnboardingSpec("wf-1", "g1", 1, []ceremony.Owner{
		{ID: "g1", PublicKeyDER: []byte{1, 2, 3}},
	})
	if err != nil {
		t.Fatalf("building spec: %v", err)
	}
	return spec
}

func TestInitOpenRoundTrip(t *testing.T) {
	dir := filepath.Join(t.TempDir(), "ceremony")
	spec := testSpec(t)
	if _, err := Init(dir, spec); err != nil {
		t.Fatalf("init: %v", err)
	}
	// Re-init with the identical spec is idempotent.
	if _, err := Init(dir, spec); err != nil {
		t.Fatalf("re-init with same spec: %v", err)
	}
	// Re-init with a different spec is refused.
	other := spec
	other.WorkflowID = "wf-2"
	if _, err := Init(dir, other); err == nil {
		t.Fatalf("re-init with different spec accepted")
	}

	_, loaded, err := Open(dir)
	if err != nil {
		t.Fatalf("open: %v", err)
	}
	if loaded.WorkflowID != "wf-1" || loaded.Coordinator != "g1" {
		t.Fatalf("loaded spec = %+v, want the initialized one", loaded)
	}
}

func TestPutGetConflict(t *testing.T) {
	dir := filepath.Join(t.TempDir(), "ceremony")
	fs, err := Init(dir, testSpec(t))
	if err != nil {
		t.Fatalf("init: %v", err)
	}

	if err := fs.Put("op/a", "value"); err != nil {
		t.Fatalf("put: %v", err)
	}
	// Identical re-put is idempotent; different value is a conflict.
	if err := fs.Put("op/a", "value"); err != nil {
		t.Fatalf("idempotent re-put: %v", err)
	}
	if err := fs.Put("op/a", "other"); err == nil {
		t.Fatalf("conflicting put accepted")
	}

	var got string
	ok, err := fs.Get("op/a", &got)
	if err != nil || !ok || got != "value" {
		t.Fatalf("get = %q ok=%v err=%v, want value", got, ok, err)
	}
	ok, err = fs.Get("op/missing", &got)
	if err != nil || ok {
		t.Fatalf("missing key: ok=%v err=%v, want absent", ok, err)
	}

	// Composite (map/struct) values re-put identically must be idempotent, not
	// falsely conflict: the on-disk copy is indented while a fresh marshal is
	// compact, so the comparison has to be canonical.
	composite := map[string]any{"b": 2, "a": 1, "nested": map[string]int{"y": 9, "x": 8}}
	if err := fs.Put("op/obj", composite); err != nil {
		t.Fatalf("put composite: %v", err)
	}
	if err := fs.Put("op/obj", composite); err != nil {
		t.Fatalf("idempotent re-put of composite falsely conflicted: %v", err)
	}
	if err := fs.Put("op/obj", map[string]any{"a": 1, "b": 3}); err == nil {
		t.Fatalf("conflicting composite put accepted")
	}

	// A fresh handle sees persisted state (process restart).
	reopened, _, err := Open(dir)
	if err != nil {
		t.Fatalf("reopen: %v", err)
	}
	ok, err = reopened.Get("op/a", &got)
	if err != nil || !ok || got != "value" {
		t.Fatalf("reopened get = %q ok=%v err=%v, want value", got, ok, err)
	}
	keys, err := reopened.Keys()
	if err != nil || len(keys) != 2 {
		t.Fatalf("keys = %v err=%v, want op/a and op/obj", keys, err)
	}
}
