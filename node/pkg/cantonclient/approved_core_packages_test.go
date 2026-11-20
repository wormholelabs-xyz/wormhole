package cantonclient

import (
	"testing"

	"github.com/stretchr/testify/assert"
	"github.com/stretchr/testify/require"
)

// deployedCorePackageIDHex is wormhole-core 0.12.1 built with Daml SDK 3.7.3 (TransferFees accepts chain 0).
const deployedCorePackageIDHex = "2ebee1d99f4bb435c025fd1d63d4c0646a081498b403a6a83f8b17646bc92ef4"

// replacedCore0120PackageIDHex is wormhole-core 0.12.0 built with Daml SDK 3.7.3: TransferFees refuses chain 0.
const replacedCore0120PackageIDHex = "aec76d21ac140c49f91754db4667e0572d24d3b1cacd85b69dc76c92ae38087b"

// replacedCore0111PackageIDHex is wormhole-core 0.11.1 built with Daml SDK 3.7.3: each verification call takes guardian public-key hints.
const replacedCore0111PackageIDHex = "019609a33c112e13fa98642f1d5b4dd75e8ab7bfa00a14fcf57caa510ed1b9d5"

// replacedCore0110PackageIDHex is wormhole-core 0.11.0 built with Daml SDK 3.7.3: an oversized governance amount aborts with ArithmeticError.
const replacedCore0110PackageIDHex = "3e555cc2975764a29323906d74ab513025a3ba96376210b69daa542b294352bc"

// replacedCore0100PackageIDHex is wormhole-core 0.10.0 built with Daml SDK 3.7.3: its CoreState governance collections grow without a bound.
const replacedCore0100PackageIDHex = "bf75fd518ed8f524832ac25c65a353215bb472a531d7ef745bf050848936eddb"

// replacedSDK373Core090PackageIDHex is wormhole-core 0.9.0 built with Daml SDK 3.7.3: its byte fields are unconstrained Text.
const replacedSDK373Core090PackageIDHex = "a562ac78ce2617dda9dc1ccd8c7500b4e523ff7ae1d216e28f8836b1fb264034"

// replacedSDK351Core090PackageIDHex is the third 0.9.0 build, with Daml SDK 3.5.1; the SDK 3.7.3 build replaces it.
const replacedSDK351Core090PackageIDHex = "bba94798a73c0fafadd836ec6754cab75205cb09556d13263813151bd748006a"

// replacedCore090PackageIDHex is the first 0.9.0 build: it accepts guardianGovernance as the fee instrument admin.
const replacedCore090PackageIDHex = "dd75f77780f2b93e5a30f12a04f54f9e4577c12ebcaf1720a413f09cc229af1d"

// replacedSecondCore090PackageIDHex is the second 0.9.0 build: TransferFees accepts chain 0.
const replacedSecondCore090PackageIDHex = "ec382591e253d8e5c9e8cd4f1de5fd3c40cb655c9f78da2d9de15af74958d66a"

// Withdrawn wormhole-core builds: the operator signs core contracts.
const (
	withdrawnCore080PackageIDHex      = "03a65dc33bc2e08ad04318e300227e3f4dc6b983be09b650b59f2676f43a22b6"
	withdrawnCore081PackageIDHex      = "4a214c8015945441a74dcc2a702f0652b3136e8f7488ab13d8196313c4d47d89"
	withdrawnCore082PackageIDHex      = "132d32bfdba695df16b78b1c2250579d9369b13fe030d951973293f4e23b70c1"
	withdrawnCore083PackageIDHex      = "9d546b11014d1272be6b09a0ff9f6204a3401333829531d360e20fabd1b4163d"
	withdrawnFirstCore083PackageIDHex = "b5613c61c1aac18db84a87e2eb13cde6d544dd953d10c40de0440747e92a7e69"
)

func TestDefaultApprovedCorePackagesFile(t *testing.T) {
	approved := DefaultApprovedCorePackages()
	require.Len(t, approved.entries, 1)

	cases := []struct {
		name         string
		packageIDHex string
		version      string
		wantErr      string
	}{
		{name: "deployed 0.12.1", packageIDHex: deployedCorePackageIDHex, version: "0.12.1"},
		{name: "deployed 0.12.1 id as 0.12.0", packageIDHex: deployedCorePackageIDHex, version: "0.12.0", wantErr: "is approved as 0.12.1, not 0.12.0"},
		{name: "replaced 0.12.0", packageIDHex: replacedCore0120PackageIDHex, version: "0.12.0", wantErr: "not in the approved"},
		{name: "replaced 0.12.0 id as 0.12.1", packageIDHex: replacedCore0120PackageIDHex, version: "0.12.1", wantErr: "not in the approved"},
		{name: "replaced 0.11.1", packageIDHex: replacedCore0111PackageIDHex, version: "0.11.1", wantErr: "not in the approved"},
		{name: "replaced 0.11.1 id as 0.12.0", packageIDHex: replacedCore0111PackageIDHex, version: "0.12.0", wantErr: "not in the approved"},
		{name: "replaced 0.11.0", packageIDHex: replacedCore0110PackageIDHex, version: "0.11.0", wantErr: "not in the approved"},
		{name: "replaced 0.11.0 id as 0.11.1", packageIDHex: replacedCore0110PackageIDHex, version: "0.11.1", wantErr: "not in the approved"},
		{name: "replaced 0.10.0", packageIDHex: replacedCore0100PackageIDHex, version: "0.10.0", wantErr: "not in the approved"},
		{name: "replaced 0.10.0 id as 0.11.0", packageIDHex: replacedCore0100PackageIDHex, version: "0.11.0", wantErr: "not in the approved"},
		{name: "replaced SDK 3.7.3 0.9.0 build", packageIDHex: replacedSDK373Core090PackageIDHex, version: "0.9.0", wantErr: "not in the approved"},
		{name: "withdrawn 0.8.0", packageIDHex: withdrawnCore080PackageIDHex, version: "0.8.0", wantErr: "not in the approved"},
		{name: "withdrawn 0.8.1", packageIDHex: withdrawnCore081PackageIDHex, version: "0.8.1", wantErr: "not in the approved"},
		{name: "withdrawn 0.8.2", packageIDHex: withdrawnCore082PackageIDHex, version: "0.8.2", wantErr: "not in the approved"},
		{name: "withdrawn 0.8.3", packageIDHex: withdrawnCore083PackageIDHex, version: "0.8.3", wantErr: "not in the approved"},
		{name: "withdrawn first 0.8.3 build", packageIDHex: withdrawnFirstCore083PackageIDHex, version: "0.8.3", wantErr: "not in the approved"},
		{name: "withdrawn 0.8.3 id as 0.9.0", packageIDHex: withdrawnCore083PackageIDHex, version: "0.9.0", wantErr: "not in the approved"},
		{name: "replaced first 0.9.0 build", packageIDHex: replacedCore090PackageIDHex, version: "0.9.0", wantErr: "not in the approved"},
		{name: "replaced second 0.9.0 build", packageIDHex: replacedSecondCore090PackageIDHex, version: "0.9.0", wantErr: "not in the approved"},
		{name: "replaced SDK 3.5.1 0.9.0 build", packageIDHex: replacedSDK351Core090PackageIDHex, version: "0.9.0", wantErr: "not in the approved"},
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
