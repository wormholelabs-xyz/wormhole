package cantonclient

import (
	"strings"
	"testing"

	"github.com/stretchr/testify/assert"
	"github.com/stretchr/testify/require"
)

// Namespace fingerprints in the Canton form: 1220 multihash prefix, SHA-256 digest.
const (
	testGovernanceFingerprint = "1220a4d7463bd34b2ba3704401b48ab41d8f88cdcbe512fc1ef071aad97fef106161"
	testLocalNetFingerprint   = "122080c014fb0f3f4e1e1bd7bbd6e5a8c3a1a7c7d3f0e0a3e9a0c2b1d4e5f60718"
)

const (
	testGovernancePartyID = "guardianGovernance::" + testGovernanceFingerprint
	testLocalNetPartyID   = "app_user_localnet-localparty-1::" + testLocalNetFingerprint
)

func TestParsePartyID(t *testing.T) {
	cases := []struct {
		name    string
		text    string
		wantErr string
	}{
		{name: "guardianGovernance party", text: testGovernancePartyID},
		{name: "LocalNet party", text: testLocalNetPartyID},
		{name: "hint of maximum length", text: strings.Repeat("h", 185) + "::" + testGovernanceFingerprint},
		{name: "empty", text: "", wantErr: "no '::'"},
		{name: "no delimiter", text: "guardianGovernance" + testGovernanceFingerprint, wantErr: "no '::'"},
		{name: "two delimiters", text: "guardian::Governance::" + testGovernanceFingerprint, wantErr: "fingerprint"},
		{name: "delimiter in fingerprint position", text: "guardianGovernance::" + testGovernanceFingerprint + "::", wantErr: "fingerprint"},
		{name: "empty hint", text: "::" + testGovernanceFingerprint, wantErr: "hint length 0"},
		{name: "hint above maximum length", text: strings.Repeat("h", 186) + "::" + testGovernanceFingerprint, wantErr: "hint length 186"},
		{name: "hint with single colon", text: "guardian:Governance::" + testGovernanceFingerprint, wantErr: "hint character"},
		{name: "hint with space", text: "guardian Governance::" + testGovernanceFingerprint, wantErr: "hint character"},
		{name: "fingerprint of 63 hex characters after the prefix", text: "guardianGovernance::" + testGovernanceFingerprint[:67], wantErr: "fingerprint length 67"},
		{name: "fingerprint of 65 hex characters after the prefix", text: testGovernancePartyID + "0", wantErr: "fingerprint length 69"},
		{name: "empty fingerprint", text: "guardianGovernance::", wantErr: "fingerprint length 0"},
		{name: "truncated fixture fingerprint", text: "guardianGovernance::1220abcd", wantErr: "fingerprint length 8"},
		{name: "uppercase fingerprint hex", text: "guardianGovernance::1220" + strings.ToUpper(testGovernanceFingerprint[4:]), wantErr: "lowercase hex"},
		{name: "wrong multihash prefix", text: "guardianGovernance::1320" + testGovernanceFingerprint[4:], wantErr: "prefix"},
		{name: "non-hex fingerprint character", text: "guardianGovernance::1220" + strings.Repeat("g", 64), wantErr: "lowercase hex"},
		{name: "leading space", text: " " + testGovernancePartyID, wantErr: "hint character"},
		{name: "trailing space", text: testGovernancePartyID + " ", wantErr: "fingerprint length 69"},
		{name: "trailing newline", text: testGovernancePartyID + "\n", wantErr: "fingerprint length 69"},
	}
	for _, tc := range cases {
		t.Run(tc.name, func(t *testing.T) {
			partyID, err := ParsePartyID(tc.text)

			if tc.wantErr != "" {
				require.ErrorContains(t, err, tc.wantErr)
				assert.Equal(t, PartyID{}, partyID)
				return
			}
			require.NoError(t, err)
			assert.Equal(t, tc.text, partyID.String())
		})
	}
}

func TestParseReadScope(t *testing.T) {
	cases := []struct {
		name                        string
		readAsParty                 string
		unsafeDevMode               bool
		wantGuardianGovernanceParty string
		wantAnyPartyUnsafeDevMode   bool
		wantErr                     string
	}{
		{name: "party outside dev mode", readAsParty: testGovernancePartyID, wantGuardianGovernanceParty: testGovernancePartyID},
		{name: "party in dev mode", readAsParty: testGovernancePartyID, unsafeDevMode: true, wantGuardianGovernanceParty: testGovernancePartyID},
		{name: "any party in dev mode", readAsParty: "", unsafeDevMode: true, wantAnyPartyUnsafeDevMode: true},
		{name: "any party outside dev mode", readAsParty: "", wantErr: "unsafe dev mode"},
		{name: "malformed party outside dev mode", readAsParty: "guardianGovernance::1220abcd", wantErr: "fingerprint length 8"},
		{name: "malformed party in dev mode", readAsParty: "guardianGovernance::1220abcd", unsafeDevMode: true, wantErr: "fingerprint length 8"},
	}
	for _, tc := range cases {
		t.Run(tc.name, func(t *testing.T) {
			readScope, err := ParseReadScope(tc.readAsParty, tc.unsafeDevMode)

			if tc.wantErr != "" {
				require.ErrorContains(t, err, tc.wantErr)
				require.Error(t, readScope.CheckWasConstructed())
				return
			}
			require.NoError(t, err)
			require.NoError(t, readScope.CheckWasConstructed())
			party, isGuardianGovernance := readScope.GuardianGovernanceParty()
			assert.Equal(t, tc.wantGuardianGovernanceParty != "", isGuardianGovernance)
			assert.Equal(t, tc.wantGuardianGovernanceParty, party.String())
			assert.Equal(t, tc.wantAnyPartyUnsafeDevMode, readScope.IsAnyPartyUnsafeDevMode())
		})
	}
}

func TestReadAnyPartyUnsafeDevModeRequiresTheFlag(t *testing.T) {
	_, err := ReadAnyPartyUnsafeDevMode(false)
	require.ErrorContains(t, err, "unsafe dev mode")

	readScope, err := ReadAnyPartyUnsafeDevMode(true)
	require.NoError(t, err)
	assert.True(t, readScope.IsAnyPartyUnsafeDevMode())
	_, isGuardianGovernance := readScope.GuardianGovernanceParty()
	assert.False(t, isGuardianGovernance)
}

func TestZeroReadScopeIsNotConstructed(t *testing.T) {
	var readScope ReadScope

	require.ErrorContains(t, readScope.CheckWasConstructed(), "not constructed")
	assert.False(t, readScope.IsAnyPartyUnsafeDevMode())
	_, isGuardianGovernance := readScope.GuardianGovernanceParty()
	assert.False(t, isGuardianGovernance)
}

func TestReadAsGuardianGovernanceRefusesZeroPartyID(t *testing.T) {
	_, err := ReadAsGuardianGovernance(PartyID{})
	require.ErrorContains(t, err, "zero PartyID")

	party, err := ParsePartyID(testGovernancePartyID)
	require.NoError(t, err)
	readScope, err := ReadAsGuardianGovernance(party)
	require.NoError(t, err)
	gotParty, isGuardianGovernance := readScope.GuardianGovernanceParty()
	assert.True(t, isGuardianGovernance)
	assert.Equal(t, party, gotParty)
}
