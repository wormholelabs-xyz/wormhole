package main

import (
	"encoding/binary"
	"encoding/hex"
	"fmt"
	"unicode/utf8"

	ethcrypto "github.com/ethereum/go-ethereum/crypto"
)

// Wormhole.Core.Replay constants (wormhole-core 0.12.3).
const (
	claimedRootsNamespace    = "wormhole-core:replay-roots"
	claimedRootDomainTag     = "wormhole-core:replay-root-claim"
	replayNamespaceMinLength = 1
	replayNamespaceMaxLength = 128
)

// checkReplayNamespace applies the ClaimReplayRoot namespace rules. Daml T.length counts code points.
func checkReplayNamespace(namespace string) error {
	if !utf8.ValidString(namespace) {
		return fmt.Errorf("-namespace is not valid UTF-8")
	}
	characterCount := utf8.RuneCountInString(namespace)
	if characterCount < replayNamespaceMinLength || characterCount > replayNamespaceMaxLength {
		return fmt.Errorf("-namespace has %d characters, expected %d to %d", characterCount, replayNamespaceMinLength, replayNamespaceMaxLength)
	}
	if namespace == claimedRootsNamespace {
		return fmt.Errorf("-namespace is reserved for the core")
	}
	return nil
}

// claimedRootDigestHex is Wormhole.Core.Replay.claimedRootDigest:
// keccak256(utf8(tag) ‖ lp(utf8(consumer)) ‖ lp(utf8(namespace))).
func claimedRootDigestHex(consumerPartyText string, namespace string) string {
	preimage := appendUint32LengthPrefixed([]byte(claimedRootDomainTag), []byte(consumerPartyText))
	preimage = appendUint32LengthPrefixed(preimage, []byte(namespace))
	return hex.EncodeToString(ethcrypto.Keccak256(preimage))
}

const (
	nttNamespaceTag    = "wormhole:ntt-namespace:v1"
	nttNamespacePrefix = "ntt:"
	// Core maxEmitterId and NTT maxNttManagerId: 2^53 - 1.
	maxNttManagerID = 9007199254740991
)

// nttNamespaceForTexts is Wormhole.Ntt.Manager.nttNamespaceFromText.
func nttNamespaceForTexts(instrumentAdminText string, instrumentIDText string, adminPartyText string, managerID uint64) string {
	preimage := appendUint32LengthPrefixed([]byte(nttNamespaceTag), []byte(instrumentAdminText))
	preimage = appendUint32LengthPrefixed(preimage, []byte(instrumentIDText))
	preimage = appendUint32LengthPrefixed(preimage, []byte(adminPartyText))
	preimage = binary.BigEndian.AppendUint64(preimage, managerID)
	return nttNamespacePrefix + hex.EncodeToString(ethcrypto.Keccak256(preimage))
}
