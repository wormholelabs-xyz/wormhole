package fake

import (
	"bytes"
	"encoding/json"
	"fmt"
	"sync"
)

// Store is an in-memory ceremony.Store with the same append-only contract as
// the filesystem store: identical re-puts are idempotent, conflicting puts
// fail.
type Store struct {
	mu      sync.Mutex
	entries map[string]json.RawMessage
}

// NewStore creates an empty in-memory store.
func NewStore() *Store {
	return &Store{entries: map[string]json.RawMessage{}}
}

// Put implements ceremony.Store.
func (s *Store) Put(key string, value any) error {
	raw, err := json.Marshal(value)
	if err != nil {
		return fmt.Errorf("fake store: marshaling %q: %w", key, err)
	}
	s.mu.Lock()
	defer s.mu.Unlock()
	if existing, ok := s.entries[key]; ok {
		if !bytes.Equal(existing, raw) {
			return fmt.Errorf("fake store: conflicting write for %q", key)
		}
		return nil
	}
	s.entries[key] = raw
	return nil
}

// Get implements ceremony.Store.
func (s *Store) Get(key string, into any) (bool, error) {
	s.mu.Lock()
	raw, ok := s.entries[key]
	s.mu.Unlock()
	if !ok {
		return false, nil
	}
	if err := json.Unmarshal(raw, into); err != nil {
		return true, fmt.Errorf("fake store: unmarshaling %q: %w", key, err)
	}
	return true, nil
}
