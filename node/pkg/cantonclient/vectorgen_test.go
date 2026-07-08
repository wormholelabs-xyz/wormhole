package cantonclient

// Temporary generator for a Daml test vector: a governance VAA (SetMessageFee,
// chain 72) signed by the well-known devnet guardian key. Run:
//   go test ./pkg/cantonclient/ -run TestGenerateCantonVector -v
// then copy the printed hex into canton/test/daml/Test/TestCore.daml. Not a real test.

import (
	"encoding/binary"
	"encoding/hex"
	"os"
	"testing"

	"github.com/ethereum/go-ethereum/crypto"
)

func TestGenerateCantonVector(t *testing.T) {
	if os.Getenv("GEN_CANTON_VECTORS") == "" {
		t.Skip("set GEN_CANTON_VECTORS=1 to regenerate the Daml test vector")
	}
	// Well-known Wormhole devnet guardian key (address beFA429d...05d0FBe).
	priv, err := crypto.HexToECDSA("cfb12303a19cde580bb4dd771639b0d26bc68353645571a8cff516ab2ee113a0")
	if err != nil {
		t.Fatal(err)
	}
	addr := crypto.PubkeyToAddress(priv.PublicKey)
	pub := crypto.FromECDSAPub(&priv.PublicKey) // 65-byte uncompressed 0x04||X||Y

	// Governance packet: module "Core" (0x..436f7265) | action 3 (SetMessageFee)
	// | chain 72 | fee (uint256 = 1000).
	module, _ := hex.DecodeString("00000000000000000000000000000000000000000000000000000000436f7265")
	payload := append([]byte{}, module...)
	payload = append(payload, 3)                     // action
	payload = append(payload, []byte{0x00, 0x48}...) // chain 72
	fee := make([]byte, 32)
	binary.BigEndian.PutUint64(fee[24:], 1000)
	payload = append(payload, fee...)

	// VAA body.
	gov, _ := hex.DecodeString("0000000000000000000000000000000000000000000000000000000000000004")
	body := make([]byte, 0)
	ts := make([]byte, 4)
	binary.BigEndian.PutUint32(ts, 1700000000)
	body = append(body, ts...)      // timestamp
	body = append(body, 0, 0, 0, 0) // nonce 0
	body = append(body, 0x00, 0x01) // emitterChain 1 (governance)
	body = append(body, gov...)     // emitterAddress 0x..04
	seq := make([]byte, 8)
	binary.BigEndian.PutUint64(seq, 1)
	body = append(body, seq...) // sequence 1
	body = append(body, 0x00)   // consistencyLevel 0
	body = append(body, payload...)

	digest := crypto.Keccak256(crypto.Keccak256(body))
	sig, err := crypto.Sign(digest, priv) // 65 bytes R||S||V
	if err != nil {
		t.Fatal(err)
	}

	vaa := []byte{0x01}           // version
	vaa = append(vaa, 0, 0, 0, 0) // guardianSetIndex 0
	vaa = append(vaa, 0x01)       // sig count
	vaa = append(vaa, 0x00)       // guardian index 0
	vaa = append(vaa, sig...)     // r||s||v
	vaa = append(vaa, body...)

	t.Logf("guardianAddr = %x", addr.Bytes())
	t.Logf("pubKey       = %x", pub)
	t.Logf("vaa          = %x", vaa)
	t.Logf("innerHash    = %x", crypto.Keccak256(body))
	t.Logf("digest       = %x", digest)
}

// TestGenerateAddressVectors prints the canonical registry-address test vector
// shared with canton/test/daml/Test/TestCore.daml (testAddressVector) and pinned
// on the Go side by pkg/watchers/canton/watcher_test.go. The preimage is
//
//	utf8(tag) ‖ uint32be(len(registrar)) ‖ utf8(registrar)
//	          ‖ uint32be(len(owner))     ‖ utf8(owner) ‖ uint64be(id)
//
// keccak256 of which is the 32-byte Wormhole emitter address.
func TestGenerateAddressVectors(t *testing.T) {
	if os.Getenv("GEN_CANTON_VECTORS") == "" {
		t.Skip("set GEN_CANTON_VECTORS=1 to regenerate the Daml test vector")
	}
	const registrar = "vector-operator::1220deadbeef"
	const owner = "vector-owner::1220cafebabe"
	const id = uint64(7)
	lp := func(s string) []byte {
		var l [4]byte
		binary.BigEndian.PutUint32(l[:], uint32(len(s))) //nolint:gosec // fixture strings are short
		return append(l[:], s...)
	}
	buf := append([]byte("wormhole:emitter:v1"), lp(registrar)...)
	buf = append(buf, lp(owner)...)
	var idb [8]byte
	binary.BigEndian.PutUint64(idb[:], id)
	buf = append(buf, idb[:]...)
	t.Logf("emitter(%s, %s, %d) = %x", registrar, owner, id, crypto.Keccak256(buf))
}

// TestGenerateTokenBridgeVectors prints the token-bridge address vectors
// shared with canton/examples/token-bridge/daml/Wormhole/Example/Test/TestTokenBridge.daml
// (testTokenAddressForVector, testTokenBridgeRecipientAddressForVector). The
// preimages are:
//
//	tokenAddressFor:          utf8(tag) ‖ lp(utf8(adminText)) ‖ lp(utf8(idText))
//	tokenBridgeRecipientAddressFor: utf8(tag) ‖ lp(utf8(recipientText))
func TestGenerateTokenBridgeVectors(t *testing.T) {
	if os.Getenv("GEN_CANTON_VECTORS") == "" {
		t.Skip("set GEN_CANTON_VECTORS=1 to regenerate the Daml test vector")
	}
	lp := func(s string) []byte {
		var l [4]byte
		binary.BigEndian.PutUint32(l[:], uint32(len(s))) //nolint:gosec // fixture strings are short
		return append(l[:], s...)
	}

	const adminText = "vector-admin::1220deadbeef"
	const idText = "USD"
	tokenBuf := append([]byte("wormhole:token-bridge-token:v1"), lp(adminText)...)
	tokenBuf = append(tokenBuf, lp(idText)...)
	t.Logf("tokenAddressFor(%s, %s) = %x", adminText, idText, crypto.Keccak256(tokenBuf))

	const recipientText = "vector-recipient::1220deadbeef"
	recipientBuf := append([]byte("wormhole:token-bridge-recipient:v1"), lp(recipientText)...)
	t.Logf("tokenBridgeRecipientAddressFor(%s) = %x", recipientText, crypto.Keccak256(recipientBuf))
}
