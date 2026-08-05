package fake

import (
	"context"
	"fmt"
	"os"
	"path/filepath"

	"github.com/wormhole-foundation/wormhole/canton/party-ceremony/ceremony"
)

// FileTopology is a Participant whose ledger state round-trips through a file
// before and after every call. It lets separate CLI processes — one per
// guardian, run at different times — act against one shared fake ledger,
// which is what makes a full multi-process ceremony rehearsal possible
// without Canton infrastructure. Not safe for concurrent writers; the
// ceremony's turn-taking makes that acceptable for a rehearsal tool.
type FileTopology struct {
	path string
	uid  string
}

// NewFileTopology binds a participant uid to a ledger snapshot file. The file
// is created empty on first use.
func NewFileTopology(path string, uid string) *FileTopology {
	return &FileTopology{path: path, uid: uid}
}

func (f *FileTopology) load() (*Ledger, *Participant, error) {
	ledger := NewLedger()
	data, err := os.ReadFile(f.path)
	if err != nil && !os.IsNotExist(err) {
		return nil, nil, fmt.Errorf("fake file ledger: %w", err)
	}
	if len(data) > 0 {
		if err := ledger.Restore(data); err != nil {
			return nil, nil, err
		}
	}
	return ledger, NewParticipant(ledger, f.uid), nil
}

func (f *FileTopology) save(ledger *Ledger) error {
	data, err := ledger.Snapshot()
	if err != nil {
		return err
	}
	tmp, err := os.CreateTemp(filepath.Dir(f.path), filepath.Base(f.path)+".*.tmp")
	if err != nil {
		return fmt.Errorf("fake file ledger: %w", err)
	}
	defer os.Remove(tmp.Name())
	if _, err := tmp.Write(data); err != nil {
		tmp.Close()
		return fmt.Errorf("fake file ledger: %w", err)
	}
	if err := tmp.Close(); err != nil {
		return fmt.Errorf("fake file ledger: %w", err)
	}
	return os.Rename(tmp.Name(), f.path)
}

// ParticipantID implements ceremony.Topology.
func (f *FileTopology) ParticipantID(ctx context.Context) (string, error) {
	ledger, p, err := f.load()
	if err != nil {
		return "", err
	}
	uid, err := p.ParticipantID(ctx)
	if err != nil {
		return "", err
	}
	return uid, f.save(ledger)
}

// PrepareRootDelegation implements ceremony.Topology.
func (f *FileTopology) PrepareRootDelegation(ctx context.Context, ownerPubDER []byte) (ceremony.PreparedTx, error) {
	_, p, err := f.load()
	if err != nil {
		return ceremony.PreparedTx{}, err
	}
	return p.PrepareRootDelegation(ctx, ownerPubDER)
}

// PrepareDecentralizedNamespace implements ceremony.Topology.
func (f *FileTopology) PrepareDecentralizedNamespace(ctx context.Context, ownerPubsDER [][]byte, threshold int) (ceremony.PreparedTx, error) {
	_, p, err := f.load()
	if err != nil {
		return ceremony.PreparedTx{}, err
	}
	return p.PrepareDecentralizedNamespace(ctx, ownerPubsDER, threshold)
}

// PreparePartyHosting implements ceremony.Topology.
func (f *FileTopology) PreparePartyHosting(ctx context.Context, spec ceremony.HostingSpec) (ceremony.PreparedTx, error) {
	_, p, err := f.load()
	if err != nil {
		return ceremony.PreparedTx{}, err
	}
	return p.PreparePartyHosting(ctx, spec)
}

// Describe implements ceremony.Topology.
func (f *FileTopology) Describe(ctx context.Context, tx ceremony.PreparedTx) (ceremony.TxView, error) {
	_, p, err := f.load()
	if err != nil {
		return ceremony.TxView{}, err
	}
	return p.Describe(ctx, tx)
}

// Fingerprint implements ceremony.Topology.
func (f *FileTopology) Fingerprint(ctx context.Context, pubDER []byte) (string, error) {
	_, p, err := f.load()
	if err != nil {
		return "", err
	}
	return p.Fingerprint(ctx, pubDER)
}

// Consent implements ceremony.Topology.
func (f *FileTopology) Consent(ctx context.Context, tx ceremony.PreparedTx) (string, error) {
	ledger, p, err := f.load()
	if err != nil {
		return "", err
	}
	blob, err := p.Consent(ctx, tx)
	if err != nil {
		return "", err
	}
	return blob, f.save(ledger)
}

// Submit implements ceremony.Topology.
func (f *FileTopology) Submit(ctx context.Context, tx ceremony.PreparedTx, ownerSigs []ceremony.OwnerSignature, consents []string) error {
	ledger, p, err := f.load()
	if err != nil {
		return err
	}
	if err := p.Submit(ctx, tx, ownerSigs, consents); err != nil {
		return err
	}
	return f.save(ledger)
}

// Inspect loads the current ledger snapshot for assertions and status
// display.
func (f *FileTopology) Inspect() (*Ledger, error) {
	ledger, _, err := f.load()
	return ledger, err
}
