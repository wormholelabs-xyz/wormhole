package cantonclient

import (
	"testing"

	"github.com/stretchr/testify/assert"
	"github.com/stretchr/testify/require"
)

func TestParseCorePackageVersion(t *testing.T) {
	cases := []struct {
		versionText string
		wantErr     bool
	}{
		{versionText: "0.8.0"},
		{versionText: "0.8.1"},
		{versionText: "10.20.30"},
		{versionText: "999999999.0.0"},
		{versionText: "", wantErr: true},
		{versionText: "0.8", wantErr: true},
		{versionText: "0.8.0.1", wantErr: true},
		{versionText: "v0.8.0", wantErr: true},
		{versionText: "00.8.0", wantErr: true},
		{versionText: "0.08.0", wantErr: true},
		{versionText: "0.8.-1", wantErr: true},
		{versionText: "0.8.0-rc1", wantErr: true},
		{versionText: " 0.8.0", wantErr: true},
		{versionText: "1000000000.0.0", wantErr: true},
	}
	for _, tc := range cases {
		t.Run(tc.versionText, func(t *testing.T) {
			version, err := ParseCorePackageVersion(tc.versionText)
			if tc.wantErr {
				require.ErrorContains(t, err, "MAJOR.MINOR.PATCH")
				assert.True(t, version.IsZero())
				return
			}
			require.NoError(t, err)
			assert.Equal(t, tc.versionText, version.String())
			assert.False(t, version.IsZero())
		})
	}
}

func TestCheckVettedPackageMatchesExpected(t *testing.T) {
	packageID, err := ParsePackageID(currentPackageIDHex)
	require.NoError(t, err)
	expectedVersion, err := ParseCorePackageVersion("0.8.0")
	require.NoError(t, err)
	matching := VettedPackageMetadata{PackageID: currentPackageIDHex, PackageName: CorePackageName, PackageVersion: "0.8.0"}
	withoutMetadata := VettedPackageMetadata{PackageID: currentPackageIDHex}
	cases := []struct {
		name    string
		entries []VettedPackageMetadata
		want    PackageMetadataVerdict
	}{
		{name: "matching entry", entries: []VettedPackageMetadata{matching}, want: PackageMetadataVerdictVerified},
		{name: "matching entry on two synchronizers", entries: []VettedPackageMetadata{matching, matching}, want: PackageMetadataVerdictVerified},
		{name: "matching entry and an entry without metadata", entries: []VettedPackageMetadata{withoutMetadata, matching}, want: PackageMetadataVerdictVerified},
		{name: "look-alike name", entries: []VettedPackageMetadata{{PackageID: currentPackageIDHex, PackageName: "wormhole-core-lookalike", PackageVersion: "0.8.0"}}, want: PackageMetadataVerdictMismatched},
		{name: "other version", entries: []VettedPackageMetadata{{PackageID: currentPackageIDHex, PackageName: CorePackageName, PackageVersion: "0.8.1"}}, want: PackageMetadataVerdictMismatched},
		{name: "name without version", entries: []VettedPackageMetadata{{PackageID: currentPackageIDHex, PackageName: CorePackageName}}, want: PackageMetadataVerdictMismatched},
		{name: "version without name", entries: []VettedPackageMetadata{{PackageID: currentPackageIDHex, PackageVersion: "0.8.0"}}, want: PackageMetadataVerdictMismatched},
		{name: "one matching and one mismatching entry", entries: []VettedPackageMetadata{matching, {PackageID: currentPackageIDHex, PackageName: CorePackageName, PackageVersion: "0.7.0"}}, want: PackageMetadataVerdictMismatched},
		{name: "only entries without metadata", entries: []VettedPackageMetadata{withoutMetadata, withoutMetadata}, want: PackageMetadataVerdictNotYetVetted},
		{name: "no entries", want: PackageMetadataVerdictNotYetVetted},
		{name: "entry for a different id is ignored", entries: []VettedPackageMetadata{{PackageID: otherPackageIDHex, PackageName: "other", PackageVersion: "9.9.9"}, matching}, want: PackageMetadataVerdictVerified},
		{name: "only an entry for a different id", entries: []VettedPackageMetadata{{PackageID: otherPackageIDHex, PackageName: CorePackageName, PackageVersion: "0.8.0"}}, want: PackageMetadataVerdictNotYetVetted},
	}
	for _, tc := range cases {
		t.Run(tc.name, func(t *testing.T) {
			assert.Equal(t, tc.want, CheckVettedPackageMatchesExpected(tc.entries, packageID, expectedVersion))
		})
	}
}
