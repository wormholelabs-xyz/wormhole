package cantonclient

import (
	"strings"
	"testing"

	"github.com/stretchr/testify/assert"
	"github.com/stretchr/testify/require"
)

var (
	primaryPackageIDHex = strings.Repeat("a1", PackageIDHexLen/2)
	nextPackageIDHex    = strings.Repeat("b2", PackageIDHexLen/2)
	otherPackageIDHex   = strings.Repeat("c3", PackageIDHexLen/2)
)

func TestParsePackageID(t *testing.T) {
	cases := []struct {
		name         string
		packageIDHex string
		wantErr      string
	}{
		{name: "lowercase 64-hex id", packageIDHex: primaryPackageIDHex},
		{name: "empty", packageIDHex: "", wantErr: "64 lowercase hex"},
		{name: "63 hex characters", packageIDHex: primaryPackageIDHex[:63], wantErr: "64 lowercase hex"},
		{name: "65 hex characters", packageIDHex: primaryPackageIDHex + "a", wantErr: "64 lowercase hex"},
		{name: "uppercase hex", packageIDHex: strings.ToUpper(primaryPackageIDHex), wantErr: "64 lowercase hex"},
		{name: "0x prefix", packageIDHex: "0x" + primaryPackageIDHex[2:], wantErr: "64 lowercase hex"},
		{name: "non-hex character", packageIDHex: primaryPackageIDHex[:63] + "g", wantErr: "64 lowercase hex"},
		{name: "surrounding whitespace", packageIDHex: " " + primaryPackageIDHex[1:], wantErr: "64 lowercase hex"},
	}
	for _, tc := range cases {
		t.Run(tc.name, func(t *testing.T) {
			packageID, err := ParsePackageID(tc.packageIDHex)
			if tc.wantErr != "" {
				require.ErrorContains(t, err, tc.wantErr)
				assert.Equal(t, PackageID{}, packageID)
				return
			}
			require.NoError(t, err)
			assert.Equal(t, tc.packageIDHex, packageID.String())
		})
	}
}

func TestParseAcceptedPackageIDs(t *testing.T) {
	cases := []struct {
		name            string
		primaryHex      string
		nextHex         string
		wantErr         string
		wantAccepted    []string
		wantDescription string
		wantPrimaryHex  string
		wantNextHex     string
		wantNotAccepted []string
	}{
		{
			name:            "primary only",
			primaryHex:      primaryPackageIDHex,
			wantAccepted:    []string{primaryPackageIDHex},
			wantNotAccepted: []string{nextPackageIDHex, otherPackageIDHex, ""},
			wantDescription: primaryPackageIDHex,
			wantPrimaryHex:  primaryPackageIDHex,
			wantNextHex:     "",
		},
		{
			name:            "primary and next",
			primaryHex:      primaryPackageIDHex,
			nextHex:         nextPackageIDHex,
			wantAccepted:    []string{primaryPackageIDHex, nextPackageIDHex},
			wantNotAccepted: []string{otherPackageIDHex, ""},
			wantDescription: primaryPackageIDHex + "," + nextPackageIDHex,
			wantPrimaryHex:  primaryPackageIDHex,
			wantNextHex:     nextPackageIDHex,
		},
		{name: "duplicate ids", primaryHex: primaryPackageIDHex, nextHex: primaryPackageIDHex, wantErr: "must differ"},
		{name: "malformed primary", primaryHex: "pkg", wantErr: "primary"},
		{name: "malformed next", primaryHex: primaryPackageIDHex, nextHex: "pkg", wantErr: "next"},
		{name: "next without primary", nextHex: nextPackageIDHex, wantErr: "primary"},
		{name: "missing primary", wantErr: "primary"},
		{name: "two ids in next", primaryHex: primaryPackageIDHex, nextHex: nextPackageIDHex + "," + otherPackageIDHex, wantErr: "next"},
	}
	for _, tc := range cases {
		t.Run(tc.name, func(t *testing.T) {
			accepted, err := ParseAcceptedPackageIDs(tc.primaryHex, tc.nextHex)
			if tc.wantErr != "" {
				require.ErrorContains(t, err, tc.wantErr)
				assert.Equal(t, AcceptedPackageIDs{}, accepted)
				return
			}
			require.NoError(t, err)
			for _, packageIDHex := range tc.wantAccepted {
				assert.Truef(t, accepted.Accepts(packageIDHex), "must accept %s", packageIDHex)
			}
			for _, packageIDHex := range tc.wantNotAccepted {
				assert.Falsef(t, accepted.Accepts(packageIDHex), "must reject %q", packageIDHex)
			}
			assert.False(t, accepted.MatchesAnyPackage())
			assert.Equal(t, tc.wantDescription, accepted.String())
			primary, hasPrimary := accepted.PrimaryPackageID()
			assert.True(t, hasPrimary)
			assert.Equal(t, tc.wantPrimaryHex, primary.String())
			next, hasNext := accepted.NextPackageID()
			assert.Equal(t, tc.wantNextHex != "", hasNext)
			assert.Equal(t, tc.wantNextHex, next.String())
		})
	}
}

func TestAcceptAnyPackageID(t *testing.T) {
	accepted := AcceptAnyPackageID()

	assert.True(t, accepted.MatchesAnyPackage())
	for _, packageIDHex := range []string{primaryPackageIDHex, otherPackageIDHex, "lookalike-package"} {
		assert.Truef(t, accepted.Accepts(packageIDHex), "wildcard must accept %s", packageIDHex)
	}
	_, hasPrimary := accepted.PrimaryPackageID()
	assert.False(t, hasPrimary)
	_, hasNext := accepted.NextPackageID()
	assert.False(t, hasNext)
	assert.Equal(t, "*", accepted.String())
}

func TestZeroAcceptedPackageIDsAcceptsNothing(t *testing.T) {
	var accepted AcceptedPackageIDs

	assert.False(t, accepted.MatchesAnyPackage())
	assert.True(t, accepted.IsEmpty())
	for _, packageIDHex := range []string{primaryPackageIDHex, ""} {
		assert.Falsef(t, accepted.Accepts(packageIDHex), "zero value must reject %q", packageIDHex)
	}
}
