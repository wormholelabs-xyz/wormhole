package cantondisclosure

import (
	"strings"
	"testing"

	"github.com/stretchr/testify/require"

	"github.com/certusone/wormhole/node/pkg/cantonclient"
)

func TestParseReplayDigest(t *testing.T) {
	testCases := []struct {
		name        string
		hexText     string
		expectError bool
	}{
		{name: "64 lowercase hex", hexText: testCoveredDigestHexText},
		{name: "all zero", hexText: strings.Repeat("0", 64)},
		{name: "all f", hexText: strings.Repeat("f", 64)},
		{name: "empty", hexText: "", expectError: true},
		{name: "63 characters", hexText: strings.Repeat("a", 63), expectError: true},
		{name: "65 characters", hexText: strings.Repeat("a", 65), expectError: true},
		{name: "uppercase", hexText: strings.Repeat("A", 64), expectError: true},
		{name: "0x prefix", hexText: "0x" + strings.Repeat("a", 62), expectError: true},
		{name: "non-hex letter", hexText: strings.Repeat("a", 63) + "g", expectError: true},
		{name: "multibyte character", hexText: strings.Repeat("a", 62) + "é", expectError: true},
	}
	for _, testCase := range testCases {
		t.Run(testCase.name, func(t *testing.T) {
			digest, err := ParseReplayDigest(testCase.hexText)
			if testCase.expectError {
				require.Error(t, err)
				require.Equal(t, ReplayDigest{}, digest)
				return
			}
			require.NoError(t, err)
			require.Equal(t, testCase.hexText, digest.String())
		})
	}
}

// Daml Wormhole.Core.Bytes.DigestNibblePrefix admits 0 to 63 nibbles, so a covering lookup checks 64 prefixes.
func TestCoveringPrefixCandidateCountMatchesDigestNibblePrefixBound(t *testing.T) {
	require.Equal(t, 64, coveringPrefixCandidateCount)
	_, err := cantonclient.ParseDigestNibblePrefix(strings.Repeat("a", coveringPrefixCandidateCount-1))
	require.NoError(t, err)
	_, err = cantonclient.ParseDigestNibblePrefix(strings.Repeat("a", coveringPrefixCandidateCount))
	require.Error(t, err)
}

func TestReplayDigestPrefixOfNibbleCount(t *testing.T) {
	digest := testReplayDigest(t, testCoveredDigestHexText)
	testCases := []struct {
		name           string
		nibbleCount    int
		expectedPrefix string
		expectError    bool
	}{
		{name: "root", nibbleCount: 0, expectedPrefix: ""},
		{name: "one nibble", nibbleCount: 1, expectedPrefix: "a"},
		{name: "deepest node", nibbleCount: 63, expectedPrefix: testCoveredDigestHexText[:63]},
		{name: "whole digest", nibbleCount: 64, expectError: true},
		{name: "negative", nibbleCount: -1, expectError: true},
	}
	for _, testCase := range testCases {
		t.Run(testCase.name, func(t *testing.T) {
			prefix, err := digest.prefixOfNibbleCount(testCase.nibbleCount)
			if testCase.expectError {
				require.Error(t, err)
				return
			}
			require.NoError(t, err)
			require.Equal(t, testCase.expectedPrefix, prefix.String())
		})
	}
}

// Golden vector of canton/test/daml/Test/TestClaimedReplayRoots.daml testClaimedRootDigestGolden.
func TestClaimedRootDigestMatchesDamlGoldenVector(t *testing.T) {
	digest := claimedRootDigestOfText("GoldenConsumer", "golden-namespace")
	require.Equal(t, "58d4dc29f0852dca5f0f626ac31c93ace1028ca0a41ba3ef972c0d681a841b4b", digest.String())
}

// Pairs of canton/test/daml/Test/TestClaimedReplayRoots.daml testClaimedRootDigestIsInjectiveOnPairs.
func TestClaimedRootDigestIsInjectiveOnPairs(t *testing.T) {
	pairs := []struct {
		consumerText  string
		namespaceText string
	}{
		{"ab", "c"},
		{"a", "bc"},
		{"ab", "cd"},
		{"abc", "d"},
		{"a", "c"},
		{"c", "a"},
	}
	seenDigests := map[ReplayDigest]bool{}
	for _, pair := range pairs {
		digest := claimedRootDigestOfText(pair.consumerText, pair.namespaceText)
		require.False(t, seenDigests[digest], "pair (%q, %q)", pair.consumerText, pair.namespaceText)
		seenDigests[digest] = true
	}
	require.Len(t, seenDigests, len(pairs))
}

func TestClaimedRootDigestOfTypedPairEqualsTextForm(t *testing.T) {
	consumer := testPartyID(t, testConsumer)
	namespace := testReplayNamespace(t, testNamespace)
	require.Equal(t, claimedRootDigestOfText(testConsumer, testNamespace), ClaimedRootDigest(consumer, namespace))
	require.NotEqual(t, ClaimedRootDigest(consumer, namespace), ClaimedRootDigest(consumer, testReplayNamespace(t, testOtherNamespace)))
}
