package cantonclient

import (
	"bytes"
	"encoding/hex"
	"fmt"
)

// UpdateIDTxIDLen is the byte length of a Canton update id: a SHA2-256
// multihash (2-byte header, 32-byte digest).
const UpdateIDTxIDLen = 34

// updateIDHexLen is the length of the lowercase hex update id the Ledger API returns.
const updateIDHexLen = 2 * UpdateIDTxIDLen

// updateIDMultihashHeader is the multihash header: SHA2-256 (0x12), 32-byte digest (0x20).
var updateIDMultihashHeader = [2]byte{0x12, 0x20}

// UpdateID is a Canton update (transaction) id. Every participant hosting the
// transaction reports the same update id, unlike the participant-local offset.
// The zero value is invalid; construct with ParseUpdateID or UpdateIDFromTxID.
type UpdateID struct {
	multihash [UpdateIDTxIDLen]byte
}

// ParseUpdateID parses the Ledger API's update_id string: exactly
// updateIDHexLen lowercase hex characters beginning with the SHA2-256
// multihash header "1220".
func ParseUpdateID(updateIDHex string) (UpdateID, error) {
	if len(updateIDHex) != updateIDHexLen {
		return UpdateID{}, fmt.Errorf("canton update id %q: got %d characters, want %d", updateIDHex, len(updateIDHex), updateIDHexLen)
	}
	multihash, err := hex.DecodeString(updateIDHex)
	if err != nil {
		return UpdateID{}, fmt.Errorf("canton update id %q: %w", updateIDHex, err)
	}
	// Lowercase only, so each update id has exactly one TxID and one string form.
	if hex.EncodeToString(multihash) != updateIDHex {
		return UpdateID{}, fmt.Errorf("canton update id %q: not lowercase hex", updateIDHex)
	}
	return UpdateIDFromTxID(multihash)
}

// UpdateIDFromTxID decodes a TxID produced by UpdateID.TxID, as carried in
// gossipv1.ObservationRequest.tx_hash.
func UpdateIDFromTxID(txID []byte) (UpdateID, error) {
	if len(txID) != UpdateIDTxIDLen {
		return UpdateID{}, fmt.Errorf("canton TxID: got %d bytes, want %d", len(txID), UpdateIDTxIDLen)
	}
	if !bytes.Equal(txID[:len(updateIDMultihashHeader)], updateIDMultihashHeader[:]) {
		return UpdateID{}, fmt.Errorf("canton TxID: multihash header %x, want %x", txID[:len(updateIDMultihashHeader)], updateIDMultihashHeader)
	}
	var updateID UpdateID
	copy(updateID.multihash[:], txID)
	return updateID, nil
}

// TxID returns the hex-decoded update id (UpdateIDTxIDLen bytes) for
// MessagePublication.TxID. Its 0x-hex form is the Ledger API update_id.
func (u UpdateID) TxID() []byte {
	return bytes.Clone(u.multihash[:])
}

// String returns the Ledger API update_id string.
func (u UpdateID) String() string {
	return hex.EncodeToString(u.multihash[:])
}

// IsZero reports whether u is the zero value.
func (u UpdateID) IsZero() bool {
	return u == UpdateID{}
}
