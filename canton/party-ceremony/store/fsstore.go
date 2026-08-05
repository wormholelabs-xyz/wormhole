// Package store persists ceremony state on the filesystem in the layout the
// production workflow shares through a Git repo: one directory per ceremony
// holding an immutable workflow.json and an append-only reports.json.
package store

import (
	"bytes"
	"encoding/json"
	"fmt"
	"os"
	"path/filepath"

	"github.com/wormhole-foundation/wormhole/canton/party-ceremony/ceremony"
)

const (
	specFile    = "workflow.json"
	reportsFile = "reports.json"
)

// FS is a filesystem-backed ceremony.Store rooted at one ceremony directory.
// Writes are atomic (temp file + rename) so an interrupted process never
// corrupts the report file. It assumes ONE writer at a time — the ceremony's
// turn-taking model, where operators act sequentially and share the directory
// through Git — and is not safe for concurrent processes on one directory.
type FS struct {
	dir string
}

// Init creates the ceremony directory and writes the immutable spec. It fails
// if a different spec is already present.
func Init(dir string, spec ceremony.Spec) (*FS, error) {
	if err := os.MkdirAll(dir, 0o755); err != nil {
		return nil, fmt.Errorf("store: creating %s: %w", dir, err)
	}
	raw, err := json.MarshalIndent(spec, "", "  ")
	if err != nil {
		return nil, fmt.Errorf("store: marshaling spec: %w", err)
	}
	path := filepath.Join(dir, specFile)
	if existing, err := os.ReadFile(path); err == nil {
		if !bytes.Equal(bytes.TrimSpace(existing), bytes.TrimSpace(raw)) {
			return nil, fmt.Errorf("store: %s already holds a different spec", path)
		}
		return &FS{dir: dir}, nil
	}
	if err := writeAtomic(path, raw); err != nil {
		return nil, err
	}
	return &FS{dir: dir}, nil
}

// Open loads an existing ceremony directory.
func Open(dir string) (*FS, ceremony.Spec, error) {
	raw, err := os.ReadFile(filepath.Join(dir, specFile))
	if err != nil {
		return nil, ceremony.Spec{}, fmt.Errorf("store: reading spec: %w", err)
	}
	var spec ceremony.Spec
	if err := json.Unmarshal(raw, &spec); err != nil {
		return nil, ceremony.Spec{}, fmt.Errorf("store: parsing spec: %w", err)
	}
	return &FS{dir: dir}, spec, nil
}

// Put implements ceremony.Store.
func (f *FS) Put(key string, value any) error {
	raw, err := json.Marshal(value)
	if err != nil {
		return fmt.Errorf("store: marshaling %q: %w", key, err)
	}
	reports, err := f.reports()
	if err != nil {
		return err
	}
	if existing, ok := reports[key]; ok {
		// Compare canonically: the on-disk value was re-indented by
		// MarshalIndent, so a byte compare against the compact `raw` would
		// falsely conflict on every non-scalar value. Canonicalizing both
		// sides makes identical re-puts idempotent regardless of formatting.
		same, err := sameJSON(existing, raw)
		if err != nil {
			return fmt.Errorf("store: comparing %q: %w", key, err)
		}
		if !same {
			return fmt.Errorf("store: conflicting write for %q", key)
		}
		return nil
	}
	reports[key] = raw
	out, err := json.MarshalIndent(reports, "", "  ")
	if err != nil {
		return fmt.Errorf("store: marshaling reports: %w", err)
	}
	return writeAtomic(filepath.Join(f.dir, reportsFile), out)
}

// sameJSON reports whether two JSON encodings are semantically equal, ignoring
// whitespace/formatting differences.
func sameJSON(a, b []byte) (bool, error) {
	var ca, cb bytes.Buffer
	if err := json.Compact(&ca, a); err != nil {
		return false, err
	}
	if err := json.Compact(&cb, b); err != nil {
		return false, err
	}
	return bytes.Equal(ca.Bytes(), cb.Bytes()), nil
}

// Get implements ceremony.Store.
func (f *FS) Get(key string, into any) (bool, error) {
	reports, err := f.reports()
	if err != nil {
		return false, err
	}
	raw, ok := reports[key]
	if !ok {
		return false, nil
	}
	if err := json.Unmarshal(raw, into); err != nil {
		return true, fmt.Errorf("store: unmarshaling %q: %w", key, err)
	}
	return true, nil
}

// Keys lists recorded operation keys (for status display).
func (f *FS) Keys() ([]string, error) {
	reports, err := f.reports()
	if err != nil {
		return nil, err
	}
	keys := make([]string, 0, len(reports))
	for k := range reports {
		keys = append(keys, k)
	}
	return keys, nil
}

func (f *FS) reports() (map[string]json.RawMessage, error) {
	raw, err := os.ReadFile(filepath.Join(f.dir, reportsFile))
	if os.IsNotExist(err) {
		return map[string]json.RawMessage{}, nil
	}
	if err != nil {
		return nil, fmt.Errorf("store: reading reports: %w", err)
	}
	var reports map[string]json.RawMessage
	if err := json.Unmarshal(raw, &reports); err != nil {
		return nil, fmt.Errorf("store: parsing reports: %w", err)
	}
	return reports, nil
}

func writeAtomic(path string, data []byte) error {
	tmp, err := os.CreateTemp(filepath.Dir(path), filepath.Base(path)+".*.tmp")
	if err != nil {
		return fmt.Errorf("store: creating temp for %s: %w", path, err)
	}
	defer os.Remove(tmp.Name())
	if _, err := tmp.Write(data); err != nil {
		tmp.Close()
		return fmt.Errorf("store: writing %s: %w", tmp.Name(), err)
	}
	if err := tmp.Close(); err != nil {
		return fmt.Errorf("store: closing %s: %w", tmp.Name(), err)
	}
	if err := os.Rename(tmp.Name(), path); err != nil {
		return fmt.Errorf("store: committing %s: %w", path, err)
	}
	return nil
}
