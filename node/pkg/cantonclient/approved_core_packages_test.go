package cantonclient

import (
	"testing"

	"github.com/stretchr/testify/assert"
	"github.com/stretchr/testify/require"
)

// deployedCorePackageIDHex is wormhole-core 0.8.0 as built on canton/overnight-u2-fixes.
const deployedCorePackageIDHex = "03a65dc33bc2e08ad04318e300227e3f4dc6b983be09b650b59f2676f43a22b6"

func TestDefaultApprovedCorePackagesFile(t *testing.T) {
	approved := DefaultApprovedCorePackages()

	require.NotEmpty(t, approved.entries)
	require.NoError(t, approved.CheckCorePackageApproved(mustParsePackageID(t, deployedCorePackageIDHex), mustParseCorePackageVersion(t, "0.8.0")))
}

func TestParseApprovedCorePackages(t *testing.T) {
	cases := []struct {
		name     string
		fileJSON string
		wantErr  string
	}{
		{name: "one entry", fileJSON: `[{"packageId":"` + currentPackageIDHex + `","version":"0.8.1"}]`},
		{name: "two entries", fileJSON: `[{"packageId":"` + currentPackageIDHex + `","version":"0.8.1"},{"packageId":"` + retiringPackageIDHex + `","version":"0.8.0"}]`},
		{name: "empty list", fileJSON: `[]`, wantErr: "no approved"},
		{name: "malformed id", fileJSON: `[{"packageId":"pkg","version":"0.8.1"}]`, wantErr: "64 lowercase hex"},
		{name: "malformed version", fileJSON: `[{"packageId":"` + currentPackageIDHex + `","version":"0.8"}]`, wantErr: "MAJOR.MINOR.PATCH"},
		{name: "duplicate id", fileJSON: `[{"packageId":"` + currentPackageIDHex + `","version":"0.8.1"},{"packageId":"` + currentPackageIDHex + `","version":"0.8.2"}]`, wantErr: "duplicate"},
		{name: "duplicate version", fileJSON: `[{"packageId":"` + currentPackageIDHex + `","version":"0.8.1"},{"packageId":"` + retiringPackageIDHex + `","version":"0.8.1"}]`, wantErr: "duplicate"},
		{name: "unknown field", fileJSON: `[{"packageId":"` + currentPackageIDHex + `","version":"0.8.1","name":"x"}]`, wantErr: "unknown field"},
		{name: "not a list", fileJSON: `{}`, wantErr: "cannot unmarshal"},
		{name: "trailing data", fileJSON: `[] []`, wantErr: "trailing"},
	}
	for _, tc := range cases {
		t.Run(tc.name, func(t *testing.T) {
			approved, err := ParseApprovedCorePackages([]byte(tc.fileJSON))
			if tc.wantErr != "" {
				require.ErrorContains(t, err, tc.wantErr)
				assert.Empty(t, approved.entries)
				return
			}
			require.NoError(t, err)
		})
	}
}

func TestCheckCorePackageApproved(t *testing.T) {
	approved, err := ParseApprovedCorePackages([]byte(`[{"packageId":"` + currentPackageIDHex + `","version":"0.8.1"}]`))
	require.NoError(t, err)
	cases := []struct {
		name         string
		packageIDHex string
		version      string
		wantErr      string
	}{
		{name: "approved id and version", packageIDHex: currentPackageIDHex, version: "0.8.1"},
		{name: "approved id with another version", packageIDHex: currentPackageIDHex, version: "0.8.0", wantErr: "approved as 0.8.1"},
		{name: "unknown id", packageIDHex: otherPackageIDHex, version: "0.8.1", wantErr: "not in the approved"},
	}
	for _, tc := range cases {
		t.Run(tc.name, func(t *testing.T) {
			err := approved.CheckCorePackageApproved(mustParsePackageID(t, tc.packageIDHex), mustParseCorePackageVersion(t, tc.version))
			if tc.wantErr == "" {
				require.NoError(t, err)
				return
			}
			require.ErrorContains(t, err, tc.wantErr)
		})
	}
}

func TestCheckAcceptedPackageIDsApproved(t *testing.T) {
	approved, err := ParseApprovedCorePackages([]byte(`[{"packageId":"` + currentPackageIDHex + `","version":"0.8.1"},{"packageId":"` + retiringPackageIDHex + `","version":"0.8.0"}]`))
	require.NoError(t, err)
	parse := func(current CurrentPackageConfig, retiring RetiringPackageConfig) AcceptedPackageIDs {
		accepted, err := ParseAcceptedPackageIDs(current, retiring, testStartupWallClock)
		require.NoError(t, err)
		return accepted
	}
	cases := []struct {
		name     string
		accepted AcceptedPackageIDs
		wantErr  string
	}{
		{name: "approved window", accepted: parse(CurrentPackageConfig{PackageIDHex: currentPackageIDHex, Version: "0.8.1"}, RetiringPackageConfig{PackageIDHex: retiringPackageIDHex, AcceptedUntilUTC: testRetiringDeadlineText, Version: "0.8.0"})},
		{name: "approved current only", accepted: parse(CurrentPackageConfig{PackageIDHex: retiringPackageIDHex, Version: "0.8.0"}, RetiringPackageConfig{})},
		{name: "unapproved retiring id", accepted: parse(CurrentPackageConfig{PackageIDHex: currentPackageIDHex, Version: "0.8.1"}, RetiringPackageConfig{PackageIDHex: otherPackageIDHex, AcceptedUntilUTC: testRetiringDeadlineText, Version: "0.8.0"}), wantErr: "retiring"},
		{name: "current without version", accepted: parse(CurrentPackageConfig{PackageIDHex: currentPackageIDHex}, RetiringPackageConfig{}), wantErr: "needs an expected version"},
		{name: "wildcard", accepted: AcceptAnyPackageID(), wantErr: "wildcard"},
		{name: "zero value", accepted: AcceptedPackageIDs{}, wantErr: "no accepted"},
	}
	for _, tc := range cases {
		t.Run(tc.name, func(t *testing.T) {
			err := approved.CheckAcceptedPackageIDsApproved(tc.accepted)
			if tc.wantErr == "" {
				require.NoError(t, err)
				return
			}
			require.ErrorContains(t, err, tc.wantErr)
		})
	}
}

func mustParsePackageID(t *testing.T, packageIDHex string) PackageID {
	t.Helper()
	packageID, err := ParsePackageID(packageIDHex)
	require.NoError(t, err)
	return packageID
}

func mustParseCorePackageVersion(t *testing.T, versionText string) CorePackageVersion {
	t.Helper()
	version, err := ParseCorePackageVersion(versionText)
	require.NoError(t, err)
	return version
}
