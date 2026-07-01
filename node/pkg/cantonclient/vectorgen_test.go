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
	payload = append(payload, 3)                       // action
	payload = append(payload, []byte{0x00, 0x48}...)   // chain 72
	fee := make([]byte, 32)
	binary.BigEndian.PutUint64(fee[24:], 1000)
	payload = append(payload, fee...)

	// VAA body.
	gov, _ := hex.DecodeString("0000000000000000000000000000000000000000000000000000000000000004")
	body := make([]byte, 0)
	ts := make([]byte, 4)
	binary.BigEndian.PutUint32(ts, 1700000000)
	body = append(body, ts...)             // timestamp
	body = append(body, 0, 0, 0, 0)        // nonce 0
	body = append(body, 0x00, 0x01)        // emitterChain 1 (governance)
	body = append(body, gov...)            // emitterAddress 0x..04
	seq := make([]byte, 8)
	binary.BigEndian.PutUint64(seq, 1)
	body = append(body, seq...)            // sequence 1
	body = append(body, 0x00)              // consistencyLevel 0
	body = append(body, payload...)

	digest := crypto.Keccak256(crypto.Keccak256(body))
	sig, err := crypto.Sign(digest, priv) // 65 bytes R||S||V
	if err != nil {
		t.Fatal(err)
	}

	vaa := []byte{0x01}                 // version
	vaa = append(vaa, 0, 0, 0, 0)       // guardianSetIndex 0
	vaa = append(vaa, 0x01)             // sig count
	vaa = append(vaa, 0x00)             // guardian index 0
	vaa = append(vaa, sig...)           // r||s||v
	vaa = append(vaa, body...)

	t.Logf("guardianAddr = %x", addr.Bytes())
	t.Logf("pubKey       = %x", pub)
	t.Logf("vaa          = %x", vaa)
	t.Logf("innerHash    = %x", crypto.Keccak256(body))
	t.Logf("digest       = %x", digest)
}

// b32 returns a 32-byte slice whose last byte is `last` (matching the
// 0x00..XX addresses used in the Daml tests).
func b32(last byte) []byte {
	b := make([]byte, 32)
	b[31] = last
	return b
}

// lenPrefixed prepends a uint16 big-endian length to b.
func lenPrefixed(b []byte) []byte {
	out := make([]byte, 2)
	binary.BigEndian.PutUint16(out, uint16(len(b))) //nolint:gosec // test fixtures are small
	return append(out, b...)
}

// TestGenerateNttVector generates the signed NTT transfer VAA fixture used by
// canton/test/daml/Test/TestNtt.daml (receive path). Same devnet guardian as
// above. Addresses: peer manager 0x..bb, peer transceiver 0x..cc (the VAA
// emitter), our manager 0x..aa; source chain 2; transfer of 1_000_000 @ 8
// decimals of token 0x..dd to recipient 0x..ee on chain 72.
func TestGenerateNttVector(t *testing.T) {
	if os.Getenv("GEN_CANTON_VECTORS") == "" {
		t.Skip("set GEN_CANTON_VECTORS=1 to regenerate the Daml test vector")
	}
	priv, err := crypto.HexToECDSA("cfb12303a19cde580bb4dd771639b0d26bc68353645571a8cff516ab2ee113a0")
	if err != nil {
		t.Fatal(err)
	}

	// NativeTokenTransfer.
	ntt := []byte{0x99, 0x4e, 0x54, 0x54} // prefix
	ntt = append(ntt, 0x08)               // decimals 8
	amount := make([]byte, 8)
	binary.BigEndian.PutUint64(amount, 1_000_000)
	ntt = append(ntt, amount...)     // amount
	ntt = append(ntt, b32(0xdd)...)  // sourceToken
	ntt = append(ntt, b32(0xee)...)  // recipientAddress
	ntt = append(ntt, 0x00, 0x48)    // recipientChain 72

	// NttManagerMessage: id 0x..01, sender = peer manager 0x..bb, payload = ntt.
	mm := append([]byte{}, b32(0x01)...)
	mm = append(mm, b32(0xbb)...)
	mm = append(mm, lenPrefixed(ntt)...)

	// WormholeTransceiverMessage: source = peer manager 0x..bb, recipient = our
	// manager 0x..aa, managerPayload = mm, transceiverPayload = empty.
	wm := []byte{0x99, 0x45, 0xff, 0x10}
	wm = append(wm, b32(0xbb)...)
	wm = append(wm, b32(0xaa)...)
	wm = append(wm, lenPrefixed(mm)...)
	wm = append(wm, lenPrefixed([]byte{})...)

	// VAA body: emitter chain 2, emitter = peer transceiver 0x..cc.
	body := make([]byte, 0)
	ts := make([]byte, 4)
	binary.BigEndian.PutUint32(ts, 1700000000)
	body = append(body, ts...)      // timestamp
	body = append(body, 0, 0, 0, 0) // nonce 0
	body = append(body, 0x00, 0x02) // emitterChain 2
	body = append(body, b32(0xcc)...)
	seq := make([]byte, 8)
	binary.BigEndian.PutUint64(seq, 1)
	body = append(body, seq...) // sequence 1
	body = append(body, 0x00)   // consistencyLevel 0
	body = append(body, wm...)  // payload

	digest := crypto.Keccak256(crypto.Keccak256(body))
	sig, err := crypto.Sign(digest, priv)
	if err != nil {
		t.Fatal(err)
	}

	vaa := []byte{0x01}           // version
	vaa = append(vaa, 0, 0, 0, 0) // guardianSetIndex 0
	vaa = append(vaa, 0x01)       // sig count
	vaa = append(vaa, 0x00)       // guardian index 0
	vaa = append(vaa, sig...)
	vaa = append(vaa, body...)

	t.Logf("nttTransferVAA = %x", vaa)
}
