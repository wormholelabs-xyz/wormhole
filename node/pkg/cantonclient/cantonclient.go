// Package cantonclient provides a thin, watcher-oriented client for the Canton
// Ledger API v2 (gRPC). It mirrors the shape of node/pkg/suiclient: a small
// interface over the chain RPC plus plain domain types, so the watcher in
// node/pkg/watchers/canton has no direct dependency on the generated protobuf
// stubs.
//
// The concrete gRPC implementation lives in cantongrpc.go and depends on the Go
// stubs generated from the vendored Ledger API v2 protos under ./proto (the
// stubs are committed and CI-verified; regenerate with
// `make generate-canton-proto`). See the package README.
package cantonclient

import (
	"context"
	"encoding/binary"
	"fmt"
	"time"
)

// TemplateID identifies a Daml template (or the template whose choice we filter
// for) on the Ledger API. It maps to com.daml.ledger.api.v2.Identifier.
type TemplateID struct {
	PackageID  string
	ModuleName string
	EntityName string
}

// CantonMessage is the decoded Wormhole.Core.State.WormholeMessage record
// produced by the PublishMessage choice. Field types match the Daml definition:
// hex Text becomes []byte here, Daml Int becomes the appropriately-sized Go int.
type CantonMessage struct {
	// IdentityCID is the Canton contract-id (hex) of the emitter's immutable
	// EmitterIdentity (Daml ContractId -> Ledger API contract_id string). The
	// 32-byte Wormhole emitter address is keccak256 of this contract-id's bytes,
	// derived by the watcher (the address is never stored on-ledger); see
	// node/pkg/watchers/canton and Wormhole.Core.State.WormholeMessage.
	IdentityCID string
	// Sequence is the per-emitter message sequence (Daml Int -> uint64).
	Sequence uint64
	// Nonce is the integrator-provided nonce (Daml Int -> uint32).
	Nonce uint32
	// ConsistencyLevel is passed through from the integrator (Daml Int -> uint8).
	ConsistencyLevel uint8
	// Payload is the raw message payload.
	Payload []byte
}

// CantonMessageEvent pairs a decoded message with the metadata of the
// transaction that produced it. EffectiveAt is the ledger effective time, used
// as the VAA timestamp (whitepaper 0001/0004: the timestamp is block-derived,
// not the message body's).
type CantonMessageEvent struct {
	// Offset is the participant offset of the transaction (the canonical TxID).
	Offset int64
	// UpdateID is the Daml update id (string), carried for human correlation.
	UpdateID string
	// EffectiveAt is the transaction's ledger effective time.
	EffectiveAt time.Time
	// Message is the decoded Wormhole message.
	Message CantonMessage
}

// CantonTransaction holds every Wormhole message found in a single transaction,
// used by reobservation (one offset may carry multiple PublishMessage choices).
type CantonTransaction struct {
	Offset      int64
	UpdateID    string
	EffectiveAt time.Time
	Messages    []CantonMessage
}

// Subscription is the handle returned by SubscribeUpdates. It mirrors
// suiclient.SuiSubscription.
type Subscription struct {
	err    chan error
	done   chan struct{}
	cancel context.CancelFunc
}

// NewSubscription constructs a Subscription. Exported for the gRPC
// implementation (and tests) in this package.
func NewSubscription(cancel context.CancelFunc) *Subscription {
	return &Subscription{
		err:    make(chan error, 1),
		done:   make(chan struct{}),
		cancel: cancel,
	}
}

// Err returns a channel that receives a terminal streaming error, if any.
func (s *Subscription) Err() <-chan error { return s.err }

// Done is closed when the subscription's background goroutine has exited.
func (s *Subscription) Done() <-chan struct{} { return s.done }

// Unsubscribe cancels the subscription.
func (s *Subscription) Unsubscribe() { s.cancel() }

// Fail records a terminal error (non-blocking) and marks the subscription done.
func (s *Subscription) Fail(err error) {
	select {
	case s.err <- err:
	default:
	}
	s.Close()
}

// Close marks the subscription's goroutine as exited (idempotent).
func (s *Subscription) Close() {
	select {
	case <-s.done:
	default:
		close(s.done)
	}
}

// CantonClient is the watcher-facing surface of the Canton Ledger API.
type CantonClient interface {
	// GetLedgerEnd returns the current participant offset — the Canton analog of
	// a block height, used for readiness and metrics. Maps to
	// StateService.GetLedgerEnd.
	GetLedgerEnd(ctx context.Context) (int64, error)

	// SubscribeUpdates streams transactions with offset strictly greater than
	// beginExclusive, decoding each result of the named choice on the given
	// template into a CantonMessageEvent delivered on out. Maps to
	// UpdateService.GetUpdates.
	SubscribeUpdates(ctx context.Context, beginExclusive int64, tmpl TemplateID, choiceName string, out chan<- CantonMessageEvent) (*Subscription, error)

	// GetUpdateByOffset fetches the transaction at the given offset and returns
	// every Wormhole message it produced. Used for reobservation. Maps to
	// UpdateService.GetUpdateByOffset / GetTransactionByOffset.
	GetUpdateByOffset(ctx context.Context, offset int64, tmpl TemplateID, choiceName string) (CantonTransaction, error)

	// GetActiveContracts returns the active contracts visible to the client's
	// read-as party (the ACS snapshot at the current ledger end). Used to read
	// on-ledger state directly — e.g. to confirm a "public" party can read the
	// core-bridge contracts it observes. Maps to StateService.GetActiveContracts.
	GetActiveContracts(ctx context.Context) ([]ActiveContract, error)

	// Close releases the underlying gRPC connection.
	Close() error
}

// ActiveContract is a minimal view of a created contract from the ACS: the
// template it instantiates and its contract id.
type ActiveContract struct {
	TemplateID TemplateID
	ContractID string
}

// offsetTxIDLen is the fixed width of a Canton TxID (a 32-byte, left-padded,
// big-endian participant offset). 32 bytes so it round-trips through the
// reobservation request's tx_hash field and matches other chains' hash width.
const offsetTxIDLen = 32

// OffsetToTxID encodes a participant offset as a 32-byte big-endian, left-padded
// identifier for use as MessagePublication.TxID.
func OffsetToTxID(offset int64) []byte {
	b := make([]byte, offsetTxIDLen)
	binary.BigEndian.PutUint64(b[offsetTxIDLen-8:], uint64(offset)) //nolint:gosec // offsets are non-negative and well within int64
	return b
}

// TxIDToOffset decodes a TxID produced by OffsetToTxID back into a participant
// offset. It tolerates any length <= 8 of trailing significant bytes as long as
// the leading bytes are zero.
func TxIDToOffset(txID []byte) (int64, error) {
	if len(txID) > offsetTxIDLen {
		return 0, fmt.Errorf("canton txID too long: %d bytes", len(txID))
	}
	// Right-align into an 8-byte window; any bytes above the low 8 must be zero.
	var buf [offsetTxIDLen]byte
	copy(buf[offsetTxIDLen-len(txID):], txID)
	for _, b := range buf[:offsetTxIDLen-8] {
		if b != 0 {
			return 0, fmt.Errorf("canton txID out of int64 range")
		}
	}
	v := binary.BigEndian.Uint64(buf[offsetTxIDLen-8:])
	if v > 1<<62 { // sanity bound; offsets are small and well below this
		return 0, fmt.Errorf("canton txID out of int64 range")
	}
	return int64(v), nil
}
