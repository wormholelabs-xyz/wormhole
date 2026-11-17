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
	"time"
)

// TemplateID identifies a Daml template (or the template whose choice we filter
// for) on the Ledger API. It maps to com.daml.ledger.api.v2.Identifier.
type TemplateID struct {
	PackageID  string
	ModuleName string
	EntityName string
}

// MaxEmitterID is Wormhole.Core.State.maxEmitterId (2^53 - 1), pinned by TestMaxEmitterIDMatchesDaml.
const MaxEmitterID = 1<<53 - 1

// CantonMessage is the decoded Wormhole.Core.State.WormholeMessage record
// produced by the PublishMessage choice. Field types match the Daml definition:
// hex Text becomes []byte here, Daml Int becomes the appropriately-sized Go int.
type CantonMessage struct {
	// Registrar, Owner, and EmitterID are the emitter's contract-key components
	// (Daml Parties + registry-allocated id). Together they determine the 32-byte
	// Wormhole emitter address:
	//
	//	keccak256("wormhole:emitter:v1" ‖ lp(Registrar) ‖ lp(Owner) ‖ uint64be(EmitterID))
	//
	// where lp is a uint32 big-endian byte-length prefix. The address is derived
	// by the watcher (never stored on-ledger); Owner is in the preimage so a
	// compromised operator cannot impersonate an existing emitter's address. See
	// node/pkg/watchers/canton and Wormhole.Core.State.WormholeMessage.
	Registrar string
	Owner     string
	EmitterID uint64
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
	// ParticipantOffset is local to the reading participant: the stream resume point.
	ParticipantOffset int64
	// UpdateID is identical on every participant: the source of the TxID.
	UpdateID UpdateID
	// EffectiveAt is the transaction's ledger effective time.
	EffectiveAt time.Time
	// Message is the decoded Wormhole message.
	Message CantonMessage
}

// CantonTransaction holds every Wormhole message found in a single transaction,
// used by reobservation (one transaction may carry multiple PublishMessage choices).
type CantonTransaction struct {
	ParticipantOffset int64
	UpdateID          UpdateID
	EffectiveAt       time.Time
	Messages          []CantonMessage
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

	// GetTransactionByUpdateID fetches the transaction with the given update id
	// and returns every Wormhole message it produced, filtered like
	// SubscribeUpdates. Used for reobservation. Maps to UpdateService.GetUpdateById.
	GetTransactionByUpdateID(ctx context.Context, updateID UpdateID, tmpl TemplateID, choiceName string) (CantonTransaction, error)

	// Close releases the underlying gRPC connection.
	Close() error
}
