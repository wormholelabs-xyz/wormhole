package main

import (
	"strings"
	"testing"

	"github.com/stretchr/testify/require"
)

// Golden value of Test.TestClaimedReplayRoots.goldenClaimedRootDigest (wormhole-core 0.12.3).
func TestClaimedRootDigestMatchesCoreGoldenValue(t *testing.T) {
	require.Equal(t, "58d4dc29f0852dca5f0f626ac31c93ace1028ca0a41ba3ef972c0d681a841b4b",
		claimedRootDigestHex("GoldenConsumer", "golden-namespace"))
}

func TestClaimedRootDigestIsInjectiveOnPairs(t *testing.T) {
	require.NotEqual(t, claimedRootDigestHex("ab", "c"), claimedRootDigestHex("a", "bc"))
}

func TestParseVAARequestClaimedRootDigest(t *testing.T) {
	request, err := parseVAARequest(strings.Fields("-kind claimed-root-digest -consumer-party consumer::1220ab -namespace ntt-a"))
	require.NoError(t, err)
	require.Equal(t, vaaKindClaimedRootDigest, request.kind)
	require.Equal(t, "consumer::1220ab", request.claimedRootConsumerParty)
	require.Equal(t, "ntt-a", request.claimedRootNamespace)

	testCases := []struct {
		name                 string
		commandLine          string
		expectedErrorMessage string
	}{
		{"without consumer party", "-kind claimed-root-digest -namespace a", "-kind claimed-root-digest requires -consumer-party"},
		{"without namespace", "-kind claimed-root-digest -consumer-party c", "-kind claimed-root-digest requires -namespace"},
		{"empty consumer party", "-kind claimed-root-digest -consumer-party= -namespace a", "-consumer-party must not be empty"},
		{"empty namespace", "-kind claimed-root-digest -consumer-party c -namespace=", "-namespace has 0 characters, expected 1 to 128"},
		{"namespace of 129 characters", "-kind claimed-root-digest -consumer-party c -namespace " + strings.Repeat("n", 129),
			"-namespace has 129 characters, expected 1 to 128"},
		{"reserved namespace", "-kind claimed-root-digest -consumer-party c -namespace wormhole-core:replay-roots",
			"-namespace is reserved for the core"},
		{"namespace flag on message", "-kind message -namespace a", "flag -namespace applies only to -kind claimed-root-digest"},
	}
	for _, testCase := range testCases {
		t.Run(testCase.name, func(t *testing.T) {
			_, parseErr := parseVAARequest(strings.Fields(testCase.commandLine))
			require.ErrorContains(t, parseErr, testCase.expectedErrorMessage)
		})
	}

	request, err = parseVAARequest(strings.Fields("-kind claimed-root-digest -consumer-party c -namespace " + strings.Repeat("n", 128)))
	require.NoError(t, err)
	require.Len(t, request.claimedRootNamespace, 128)
}

// Vector of Test.TestNtt.testNamespaceVector (ntt 0.6.2).
func TestNttNamespaceMatchesNttVector(t *testing.T) {
	require.Equal(t, "ntt:ab792801446bfcbef62e6559bd83cae570df69488931273fd99820325a7b8af0",
		nttNamespaceForTexts("vector-instrument-admin::1220deadbeef", "VECTOR", "vector-admin::1220cafebabe", 0))
	require.NotEqual(t, nttNamespaceForTexts("vector-instrument-admin::1220deadbeef", "VECTOR", "vector-admin::1220cafebabe", 0),
		nttNamespaceForTexts("vector-instrument-admin::1220deadbeef", "VECTOR", "vector-admin::1220cafebabe", 1))
}

func TestParseVAARequestNttNamespace(t *testing.T) {
	request, err := parseVAARequest(strings.Fields(
		"-kind ntt-namespace -namespace-instrument-admin dso::1220ab -namespace-instrument-id Amulet -namespace-admin-party admin::1220cd -namespace-manager-id 2"))
	require.NoError(t, err)
	require.Equal(t, vaaKindNttNamespace, request.kind)
	require.Equal(t, "dso::1220ab", request.nttNamespaceInstrumentAdmin)
	require.Equal(t, "Amulet", request.nttNamespaceInstrumentID)
	require.Equal(t, "admin::1220cd", request.nttNamespaceAdminParty)
	require.Equal(t, uint64(2), request.nttNamespaceManagerID)

	complete := "-kind ntt-namespace -namespace-instrument-admin i -namespace-instrument-id d -namespace-admin-party a -namespace-manager-id 0"
	testCases := []struct {
		name                 string
		commandLine          string
		expectedErrorMessage string
	}{
		{"without manager id", strings.Replace(complete, " -namespace-manager-id 0", "", 1), "-kind ntt-namespace requires -namespace-manager-id"},
		{"without admin party", strings.Replace(complete, " -namespace-admin-party a", "", 1), "-kind ntt-namespace requires -namespace-admin-party"},
		{"empty instrument admin", strings.Replace(complete, "-namespace-instrument-admin i", "-namespace-instrument-admin=", 1),
			"-namespace-instrument-admin must not be empty"},
		{"empty instrument id", strings.Replace(complete, "-namespace-instrument-id d", "-namespace-instrument-id=", 1),
			"-namespace-instrument-id must not be empty"},
		{"empty admin party", strings.Replace(complete, "-namespace-admin-party a", "-namespace-admin-party=", 1),
			"-namespace-admin-party must not be empty"},
		{"manager id above 2^53 - 1", strings.Replace(complete, "-namespace-manager-id 0", "-namespace-manager-id 9007199254740992", 1),
			"-namespace-manager-id exceeds 9007199254740991"},
		{"namespace flag on claimed-root-digest", "-kind claimed-root-digest -consumer-party c -namespace n -namespace-manager-id 0",
			"flag -namespace-manager-id applies only to -kind ntt-namespace"},
	}
	for _, testCase := range testCases {
		t.Run(testCase.name, func(t *testing.T) {
			_, parseErr := parseVAARequest(strings.Fields(testCase.commandLine))
			require.ErrorContains(t, parseErr, testCase.expectedErrorMessage)
		})
	}
}
