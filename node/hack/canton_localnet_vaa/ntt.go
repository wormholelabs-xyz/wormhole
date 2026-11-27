package main

import (
	"encoding/binary"
	"encoding/hex"
	"fmt"
	"math"
	"regexp"

	ethcrypto "github.com/ethereum/go-ethereum/crypto"
)

// Wire layouts match Wormhole.Ntt.Payload in the canton/ntt package of the NTT repository.
var (
	nativeTokenTransferPrefix = []byte{0x99, 0x4e, 0x54, 0x54}
	wormholeTransceiverPrefix = []byte{0x99, 0x45, 0xff, 0x10}
)

const (
	nttRecipientAddressTag = "wormhole:ntt-recipient:v1"
	nttInstrumentIDTag     = "wormhole:ntt-instrument:v1"
	nttInstrumentIDPrefix  = "wormhole-ntt:"
	maxTrimmedDecimals     = 8
)

var lowercaseBytes32Hex = regexp.MustCompile(`^[0-9a-f]{64}$`)

// nttTransfer is one inbound NTT transfer: the WormholeTransceiverMessage, its NttManagerMessage
// and its NativeTokenTransfer (no additional payload).
type nttTransfer struct {
	SourceManager    [32]byte
	RecipientManager [32]byte
	MessageID        [32]byte
	Sender           [32]byte
	TrimmedDecimals  uint8
	TrimmedAmount    uint64
	SourceToken      [32]byte
	RecipientAddress [32]byte
	RecipientChain   uint16
}

func parseBytes32Hex(hexText string) ([32]byte, error) {
	var address [32]byte
	if !lowercaseBytes32Hex.MatchString(hexText) {
		return address, fmt.Errorf("expected 32 bytes of lowercase hex, got %q", hexText)
	}
	decoded, err := hex.DecodeString(hexText)
	if err != nil {
		return address, err
	}
	copy(address[:], decoded)
	return address, nil
}

func appendUint16LengthPrefixed(buffer []byte, field []byte) []byte {
	if len(field) > math.MaxUint16 {
		panic("ntt: length-prefixed field exceeds uint16")
	}
	buffer = binary.BigEndian.AppendUint16(buffer, uint16(len(field))) // #nosec G115 -- checked above
	return append(buffer, field...)
}

func appendUint32LengthPrefixed(buffer []byte, field []byte) []byte {
	if len(field) > math.MaxUint32 {
		panic("ntt: length-prefixed field exceeds uint32")
	}
	buffer = binary.BigEndian.AppendUint32(buffer, uint32(len(field))) // #nosec G115 -- checked above
	return append(buffer, field...)
}

func encodeNativeTokenTransfer(transfer nttTransfer) []byte {
	if transfer.TrimmedDecimals > maxTrimmedDecimals {
		panic("ntt: trimmed decimals exceed 8")
	}
	encoded := append([]byte{}, nativeTokenTransferPrefix...)
	encoded = append(encoded, transfer.TrimmedDecimals)
	encoded = binary.BigEndian.AppendUint64(encoded, transfer.TrimmedAmount)
	encoded = append(encoded, transfer.SourceToken[:]...)
	encoded = append(encoded, transfer.RecipientAddress[:]...)
	return binary.BigEndian.AppendUint16(encoded, transfer.RecipientChain)
}

func encodeNttManagerMessage(transfer nttTransfer) []byte {
	encoded := append([]byte{}, transfer.MessageID[:]...)
	encoded = append(encoded, transfer.Sender[:]...)
	return appendUint16LengthPrefixed(encoded, encodeNativeTokenTransfer(transfer))
}

func encodeWormholeTransceiverMessage(transfer nttTransfer) []byte {
	encoded := append([]byte{}, wormholeTransceiverPrefix...)
	encoded = append(encoded, transfer.SourceManager[:]...)
	encoded = append(encoded, transfer.RecipientManager[:]...)
	encoded = appendUint16LengthPrefixed(encoded, encodeNttManagerMessage(transfer))
	return appendUint16LengthPrefixed(encoded, nil)
}

// nttRecipientAddressForPartyText is Wormhole.Ntt.Manager.recipientAddressFromText.
func nttRecipientAddressForPartyText(partyText string) []byte {
	preimage := appendUint32LengthPrefixed([]byte(nttRecipientAddressTag), []byte(partyText))
	return ethcrypto.Keccak256(preimage)
}

// nttInstrumentIDForAdminText is Wormhole.Ntt.Manager.nttInstrumentIdFromText.
func nttInstrumentIDForAdminText(adminPartyText string, instrumentNonce uint64) string {
	preimage := appendUint32LengthPrefixed([]byte(nttInstrumentIDTag), []byte(adminPartyText))
	preimage = binary.BigEndian.AppendUint64(preimage, instrumentNonce)
	return nttInstrumentIDPrefix + hex.EncodeToString(ethcrypto.Keccak256(preimage))
}
