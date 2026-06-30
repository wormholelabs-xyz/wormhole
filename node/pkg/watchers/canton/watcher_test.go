package canton

import (
	"context"
	"encoding/hex"
	"testing"
	"time"

	"github.com/certusone/wormhole/node/pkg/cantonclient"
	"github.com/certusone/wormhole/node/pkg/common"
	gossipv1 "github.com/certusone/wormhole/node/pkg/proto/gossip/v1"
	ethcrypto "github.com/ethereum/go-ethereum/crypto"
	"github.com/stretchr/testify/assert"
	"github.com/stretchr/testify/require"
	"github.com/wormhole-foundation/wormhole/sdk/vaa"
	"go.uber.org/zap"
)

// sampleCID is a representative authenticated Canton contract-id (hex): a 0x00
// version prefix, a 32-byte discriminator, and a suffix. The emitter address is
// keccak256 of its decoded bytes.
const sampleCID = "00d3b1f4a2c6e7890a1b2c3d4e5f60718293a4b5c6d7e8f90112233445566778" +
	"8120abccddeeff00112233445566778899aabbccddeeff0011223344556677"

// wantAddr is the emitter address the watcher must derive from sampleCID.
func wantAddr(t *testing.T) vaa.Address {
	t.Helper()
	cidBytes, err := hex.DecodeString(sampleCID)
	require.NoError(t, err)
	var a vaa.Address
	copy(a[:], ethcrypto.Keccak256(cidBytes))
	return a
}

// fakeClient is a CantonClient that returns canned data for tests.
type fakeClient struct {
	ledgerEnd   int64
	byOffset    map[int64]cantonclient.CantonTransaction
	closeCalled bool
}

func (f *fakeClient) GetLedgerEnd(_ context.Context) (int64, error) { return f.ledgerEnd, nil }
func (f *fakeClient) SubscribeUpdates(_ context.Context, _ int64, _ cantonclient.TemplateID, _ string, _ chan<- cantonclient.CantonMessageEvent) (*cantonclient.Subscription, error) {
	return cantonclient.NewSubscription(func() {}), nil
}
func (f *fakeClient) GetUpdateByOffset(_ context.Context, offset int64, _ cantonclient.TemplateID, _ string) (cantonclient.CantonTransaction, error) {
	return f.byOffset[offset], nil
}
func (f *fakeClient) Close() error { f.closeCalled = true; return nil }

func testWatcher(msgC chan<- *common.MessagePublication) *Watcher {
	return NewWatcher("canton:5011", "pkg123", "Operator::ns", true, msgC, make(chan *gossipv1.ObservationRequest))
}

func TestProcessMessageBuildsObservation(t *testing.T) {
	msgC := make(chan *common.MessagePublication, 1)
	w := testWatcher(msgC)

	ts := time.Unix(1_700_000_000, 0).UTC()
	w.processMessage(zap.NewNop(), cantonclient.CantonMessageEvent{
		Offset:      42,
		UpdateID:    "update-1",
		EffectiveAt: ts,
		Message: cantonclient.CantonMessage{
			IdentityCID:      sampleCID,
			Sequence:         7,
			Nonce:            99,
			ConsistencyLevel: 0,
			Payload:          []byte{0xde, 0xad},
		},
	}, false)

	got := <-msgC
	assert.Equal(t, vaa.ChainIDCanton, got.EmitterChain)
	assert.Equal(t, uint64(7), got.Sequence)
	assert.Equal(t, uint32(99), got.Nonce)
	assert.Equal(t, ts, got.Timestamp)
	assert.Equal(t, []byte{0xde, 0xad}, got.Payload)
	assert.False(t, got.IsReobservation)
	// TxID is the offset encoded as 32 big-endian bytes.
	assert.Equal(t, cantonclient.OffsetToTxID(42), got.TxID)
	// Emitter address is keccak256 of the identity contract-id's bytes.
	assert.Equal(t, wantAddr(t), got.EmitterAddress)
}

func TestProcessMessageDropsBadIdentity(t *testing.T) {
	for name, cid := range map[string]string{
		"not hex":    "zzzz",
		"empty":      "",
		"odd length": "abc",
	} {
		t.Run(name, func(t *testing.T) {
			msgC := make(chan *common.MessagePublication, 1)
			w := testWatcher(msgC)

			w.processMessage(zap.NewNop(), cantonclient.CantonMessageEvent{
				Offset:      1,
				EffectiveAt: time.Unix(1, 0),
				Message:     cantonclient.CantonMessage{IdentityCID: cid},
			}, false)

			select {
			case <-msgC:
				t.Fatal("expected no observation for malformed identity contract-id")
			default:
			}
		})
	}
}

func TestHandleReobservation(t *testing.T) {
	msgC := make(chan *common.MessagePublication, 1)
	w := testWatcher(msgC)

	const offset int64 = 1234
	client := &fakeClient{
		byOffset: map[int64]cantonclient.CantonTransaction{
			offset: {
				Offset:      offset,
				UpdateID:    "update-reobs",
				EffectiveAt: time.Unix(1_700_000_001, 0).UTC(),
				Messages: []cantonclient.CantonMessage{{
					IdentityCID: sampleCID,
					Sequence:    3,
					Payload:     []byte{0x01},
				}},
			},
		},
	}

	w.handleReobservation(context.Background(), zap.NewNop(), client, &gossipv1.ObservationRequest{
		ChainId: uint32(vaa.ChainIDCanton),
		TxHash:  cantonclient.OffsetToTxID(offset),
	})

	got := <-msgC
	assert.True(t, got.IsReobservation)
	assert.Equal(t, uint64(3), got.Sequence)
	assert.Equal(t, cantonclient.OffsetToTxID(offset), got.TxID)
	assert.Equal(t, wantAddr(t), got.EmitterAddress)
}

func TestHandleReobservationWrongChainPanics(t *testing.T) {
	w := testWatcher(make(chan *common.MessagePublication, 1))
	require.Panics(t, func() {
		w.handleReobservation(context.Background(), zap.NewNop(), &fakeClient{}, &gossipv1.ObservationRequest{
			ChainId: uint32(vaa.ChainIDEthereum),
			TxHash:  cantonclient.OffsetToTxID(1),
		})
	})
}
