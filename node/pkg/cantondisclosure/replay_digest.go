package cantondisclosure

import (
	"encoding/binary"
	"encoding/hex"
	"fmt"
	"math"

	ethcrypto "github.com/ethereum/go-ethereum/crypto"

	"github.com/certusone/wormhole/node/pkg/cantonclient"
)

// Daml Wormhole.Core.Replay.digestNibbleCount and maximumPrefixNibbles.
const (
	replayDigestNibbleCount    = 64
	maxReplayNodePrefixNibbles = 63
)

// coveringPrefixCandidateCount is the prefix depths 0 to maxReplayNodePrefixNibbles.
const coveringPrefixCandidateCount = maxReplayNodePrefixNibbles + 1

// Compile-time assertion: coveringPrefixCandidateCount == replayDigestNibbleCount.
var _ = [1]struct{}{}[coveringPrefixCandidateCount-replayDigestNibbleCount]

// ReplayDigest is a Daml Wormhole.Core.Bytes.VaaDigest: exactly 64 lowercase hex nibbles.
type ReplayDigest struct {
	hexText string
}

// ParseReplayDigest accepts exactly one 64-nibble lowercase hex digest.
func ParseReplayDigest(hexText string) (ReplayDigest, error) {
	if len(hexText) != replayDigestNibbleCount {
		return ReplayDigest{}, fmt.Errorf("replay digest length %d is not %d", len(hexText), replayDigestNibbleCount)
	}
	if !isLowercaseHex(hexText) {
		return ReplayDigest{}, fmt.Errorf("replay digest %q is not lowercase hex", hexText)
	}
	return ReplayDigest{hexText: hexText}, nil
}

func (digest ReplayDigest) String() string { return digest.hexText }

// prefixOfNibbleCount is the ReplayNode prefix of depth nibbleCount that covers digest.
func (digest ReplayDigest) prefixOfNibbleCount(nibbleCount int) (cantonclient.DigestNibblePrefix, error) {
	if len(digest.hexText) != replayDigestNibbleCount {
		return cantonclient.DigestNibblePrefix{}, fmt.Errorf("replay digest length %d is not %d", len(digest.hexText), replayDigestNibbleCount)
	}
	if nibbleCount < 0 || nibbleCount > maxReplayNodePrefixNibbles {
		return cantonclient.DigestNibblePrefix{}, fmt.Errorf("prefix nibble count %d is outside [0, %d]", nibbleCount, maxReplayNodePrefixNibbles)
	}
	return cantonclient.ParseDigestNibblePrefix(digest.hexText[:nibbleCount])
}

// Daml Wormhole.Core.Replay.claimedRootsNamespace and claimedRootDomainTag.
const (
	claimedRootsNamespace = "wormhole-core:replay-roots"
	claimedRootDomainTag  = "wormhole-core:replay-root-claim"
)

// ClaimedRootDigest is Daml Wormhole.Core.Replay.claimedRootDigest:
// keccak256(utf8(tag) ‖ lp(utf8(consumer)) ‖ lp(utf8(namespace))), lp a 4-byte big-endian byte length.
func ClaimedRootDigest(consumer cantonclient.PartyID, namespace cantonclient.ReplayNamespace) ReplayDigest {
	return claimedRootDigestOfText(consumer.String(), namespace.String())
}

func claimedRootDigestOfText(consumerText string, namespaceText string) ReplayDigest {
	if len(consumerText) == 0 {
		panic("cantondisclosure: claimed root digest of an empty consumer")
	}
	if len(namespaceText) == 0 {
		panic("cantondisclosure: claimed root digest of an empty namespace")
	}
	preimage := []byte(claimedRootDomainTag)
	preimage = appendLengthPrefixed(preimage, []byte(consumerText))
	preimage = appendLengthPrefixed(preimage, []byte(namespaceText))
	digestBytes := ethcrypto.Keccak256(preimage)
	if len(digestBytes) != replayDigestNibbleCount/2 {
		panic(fmt.Sprintf("cantondisclosure: keccak256 returned %d bytes", len(digestBytes)))
	}
	digest, err := ParseReplayDigest(hex.EncodeToString(digestBytes))
	if err != nil {
		panic(fmt.Sprintf("cantondisclosure: claimed root digest: %v", err))
	}
	return digest
}

// appendLengthPrefixed is Daml Wormhole.Core.Bytes.lenPrefixed.
func appendLengthPrefixed(preimage []byte, field []byte) []byte {
	if uint64(len(field)) > math.MaxUint32 {
		panic(fmt.Sprintf("cantondisclosure: length-prefixed field of %d bytes exceeds 4 bytes of length", len(field)))
	}
	lengthPrefixedStart := len(preimage)
	preimage = binary.BigEndian.AppendUint32(preimage, uint32(len(field)))
	preimage = append(preimage, field...)
	if len(preimage)-lengthPrefixedStart != 4+len(field) {
		panic("cantondisclosure: length-prefixed field has a wrong byte count")
	}
	return preimage
}

func isLowercaseHex(text string) bool {
	for _, character := range []byte(text) {
		if !((character >= '0' && character <= '9') || (character >= 'a' && character <= 'f')) {
			return false
		}
	}
	return true
}
