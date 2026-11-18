package canton

import (
	"testing"
	"time"

	"github.com/certusone/wormhole/node/pkg/cantonclient"
	"github.com/certusone/wormhole/node/pkg/common"
	gossipv1 "github.com/certusone/wormhole/node/pkg/proto/gossip/v1"
	"github.com/stretchr/testify/assert"
	"github.com/stretchr/testify/require"
	"github.com/wormhole-foundation/wormhole/sdk/vaa"
)

const (
	testGovernanceParty   = "guardianGovernance::1220abcd"
	testCurrentPackageID  = "1111111111111111111111111111111111111111111111111111111111111111"
	testRetiringPackageID = "2222222222222222222222222222222222222222222222222222222222222222"
	testOtherPackageID    = "3333333333333333333333333333333333333333333333333333333333333333"

	testCurrentPackageVersion  = "0.8.1"
	testRetiringPackageVersion = "0.8.0"
)

func createWith(t *testing.T, pkgID string, env common.Environment) error {
	t.Helper()
	return createWithReadAs(t, pkgID, testGovernanceParty, env)
}

func createWithReadAs(t *testing.T, pkgID string, readAsParty string, env common.Environment) error {
	t.Helper()
	flags := corePackageFlags{currentPackageID: pkgID}
	if pkgID != "" {
		flags.currentPackageVersion = testCurrentPackageVersion
	}
	return createWithPackageFlags(t, flags, readAsParty, env)
}

// corePackageFlags mirrors the --cantonPackageID and --cantonRetiringPackage* flags.
type corePackageFlags struct {
	currentPackageID             string
	currentPackageVersion        string
	retiringPackageID            string
	retiringPackageAcceptedUntil string
	retiringPackageVersion       string
}

// testApprovedCorePackages approves the test current and retiring ids at their test versions.
func testApprovedCorePackages(t *testing.T) *cantonclient.ApprovedCorePackages {
	t.Helper()
	approved, err := cantonclient.ParseApprovedCorePackages([]byte(`[` +
		`{"packageId":"` + testCurrentPackageID + `","version":"` + testCurrentPackageVersion + `"},` +
		`{"packageId":"` + testRetiringPackageID + `","version":"` + testRetiringPackageVersion + `"}]`))
	require.NoError(t, err)
	return &approved
}

func createWithPackageFlags(t *testing.T, flags corePackageFlags, readAsParty string, env common.Environment) error {
	t.Helper()
	wc := &WatcherConfig{
		approvedCorePackages:         testApprovedCorePackages(t),
		NetworkID:                    "canton",
		ChainID:                      vaa.ChainIDCanton,
		Rpc:                          "canton:6865",
		PackageID:                    flags.currentPackageID,
		PackageVersion:               flags.currentPackageVersion,
		RetiringPackageID:            flags.retiringPackageID,
		RetiringPackageAcceptedUntil: flags.retiringPackageAcceptedUntil,
		RetiringPackageVersion:       flags.retiringPackageVersion,
		ReadAsParty:                  readAsParty,
	}
	_, _, err := wc.Create(
		make(chan *common.MessagePublication, 1),
		make(chan *gossipv1.ObservationRequest, 1),
		nil, nil, nil,
		env,
	)
	return err
}

// TestCreateRequiresPackageIDOutsideDevMode locks in the anti-spoofing guard:
// an empty package id (which matches any package) is rejected outside dev mode.
func TestCreateRequiresPackageIDOutsideDevMode(t *testing.T) {
	// Empty package id is rejected outside dev mode.
	for _, env := range []common.Environment{common.MainNet, common.TestNet} {
		err := createWith(t, "", env)
		require.Error(t, err, "empty packageID must be rejected in %s", env)
		assert.Contains(t, err.Error(), "PackageID")
	}

	// Empty package id is allowed in unsafe dev mode (devnet wildcard).
	require.NoError(t, createWith(t, "", common.UnsafeDevNet))

	// A pinned package id is accepted everywhere.
	for _, env := range []common.Environment{common.MainNet, common.TestNet, common.UnsafeDevNet} {
		require.NoError(t, createWith(t, testCurrentPackageID, env), "pinned packageID must be accepted in %s", env)
	}
}

// retiringDeadlineFromNow is a deadline offset from the wall clock, in the flag format.
func retiringDeadlineFromNow(offset time.Duration) string {
	return time.Now().UTC().Add(offset).Truncate(time.Second).Format("2006-01-02T15:04:05Z")
}

func TestCreateValidatesCorePackageIDs(t *testing.T) {
	inOneDay := retiringDeadlineFromNow(24 * time.Hour)
	current := corePackageFlags{currentPackageID: testCurrentPackageID, currentPackageVersion: testCurrentPackageVersion}
	window := func(retiringPackageID string, retiringPackageAcceptedUntil string) corePackageFlags {
		flags := current
		flags.retiringPackageID = retiringPackageID
		flags.retiringPackageAcceptedUntil = retiringPackageAcceptedUntil
		flags.retiringPackageVersion = testRetiringPackageVersion
		return flags
	}
	withoutVersions := func(flags corePackageFlags) corePackageFlags {
		flags.currentPackageVersion = ""
		flags.retiringPackageVersion = ""
		return flags
	}
	withCurrentID := func(flags corePackageFlags, currentPackageID string) corePackageFlags {
		flags.currentPackageID = currentPackageID
		return flags
	}
	cases := []struct {
		name    string
		flags   corePackageFlags
		env     common.Environment
		wantErr string
	}{
		{name: "valid single", flags: current, env: common.MainNet},
		{name: "valid window", flags: window(testRetiringPackageID, inOneDay), env: common.MainNet},
		{name: "valid window in dev mode", flags: window(testRetiringPackageID, inOneDay), env: common.UnsafeDevNet},
		{name: "window with a past deadline", flags: window(testRetiringPackageID, retiringDeadlineFromNow(-time.Hour)), env: common.MainNet},
		{name: "window longer than the maximum", flags: window(testRetiringPackageID, retiringDeadlineFromNow(8*24*time.Hour)), env: common.MainNet, wantErr: "more than"},
		{name: "retiring id without deadline", flags: window(testRetiringPackageID, ""), env: common.MainNet, wantErr: "requires an accepted-until deadline"},
		{name: "deadline without retiring id", flags: corePackageFlags{currentPackageID: testCurrentPackageID, currentPackageVersion: testCurrentPackageVersion, retiringPackageAcceptedUntil: inOneDay}, env: common.MainNet, wantErr: "requires a retiring core package id"},
		{name: "deadline with offset", flags: window(testRetiringPackageID, "2026-11-18T13:00:00+01:00"), env: common.MainNet, wantErr: "RFC 3339 UTC"},
		{name: "duplicate", flags: window(testCurrentPackageID, inOneDay), env: common.MainNet, wantErr: "must differ"},
		{name: "malformed current", flags: withCurrentID(current, "abc123"), env: common.MainNet, wantErr: "current core package id"},
		{name: "malformed current in dev mode", flags: withCurrentID(current, "pkg"), env: common.UnsafeDevNet, wantErr: "current core package id"},
		{name: "uppercase current", flags: withCurrentID(current, "AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA"), env: common.MainNet, wantErr: "current core package id"},
		{name: "malformed retiring", flags: window("abc123", inOneDay), env: common.MainNet, wantErr: "retiring core package id"},
		{name: "three ids", flags: window(testRetiringPackageID+","+testOtherPackageID, inOneDay), env: common.MainNet, wantErr: "retiring core package id"},
		{name: "three ids in current", flags: withCurrentID(window(testOtherPackageID, inOneDay), testCurrentPackageID+","+testRetiringPackageID), env: common.MainNet, wantErr: "current core package id"},
		{name: "missing current outside dev mode", flags: withCurrentID(window(testRetiringPackageID, inOneDay), ""), env: common.MainNet, wantErr: "PackageID must be set"},
		{name: "missing current in testnet", flags: withCurrentID(window(testRetiringPackageID, inOneDay), ""), env: common.TestNet, wantErr: "PackageID must be set"},
		{name: "retiring without current in dev mode", flags: withCurrentID(window(testRetiringPackageID, inOneDay), ""), env: common.UnsafeDevNet, wantErr: "current core package id"},
		{name: "deadline alone in dev mode", flags: corePackageFlags{retiringPackageAcceptedUntil: inOneDay}, env: common.UnsafeDevNet, wantErr: "current core package id"},
		{name: "current version alone in dev mode", flags: corePackageFlags{currentPackageVersion: testCurrentPackageVersion}, env: common.UnsafeDevNet, wantErr: "current core package id"},
		{name: "missing current version outside dev mode", flags: corePackageFlags{currentPackageID: testCurrentPackageID}, env: common.MainNet, wantErr: "PackageVersion must be set"},
		{name: "missing retiring version outside dev mode", flags: corePackageFlags{currentPackageID: testCurrentPackageID, currentPackageVersion: testCurrentPackageVersion, retiringPackageID: testRetiringPackageID, retiringPackageAcceptedUntil: inOneDay}, env: common.TestNet, wantErr: "RetiringPackageVersion must be set"},
		{name: "malformed current version", flags: corePackageFlags{currentPackageID: testCurrentPackageID, currentPackageVersion: "0.8"}, env: common.MainNet, wantErr: "MAJOR.MINOR.PATCH"},
		{name: "malformed retiring version", flags: corePackageFlags{currentPackageID: testCurrentPackageID, currentPackageVersion: testCurrentPackageVersion, retiringPackageID: testRetiringPackageID, retiringPackageAcceptedUntil: inOneDay, retiringPackageVersion: "0.8.0.1"}, env: common.MainNet, wantErr: "MAJOR.MINOR.PATCH"},
		{name: "retiring version without retiring id", flags: corePackageFlags{currentPackageID: testCurrentPackageID, currentPackageVersion: testCurrentPackageVersion, retiringPackageVersion: testRetiringPackageVersion}, env: common.MainNet, wantErr: "requires a retiring core package id"},
		{name: "window without versions in dev mode", flags: withoutVersions(window(testRetiringPackageID, inOneDay)), env: common.UnsafeDevNet},
		{name: "unapproved current outside dev mode", flags: withCurrentID(current, testOtherPackageID), env: common.MainNet, wantErr: "not in the approved"},
		{name: "current at an unapproved version outside dev mode", flags: corePackageFlags{currentPackageID: testCurrentPackageID, currentPackageVersion: "0.8.9"}, env: common.TestNet, wantErr: "approved as"},
		{name: "unapproved retiring outside dev mode", flags: window(testOtherPackageID, inOneDay), env: common.MainNet, wantErr: "not in the approved"},
		{name: "unapproved current in dev mode", flags: withCurrentID(current, testOtherPackageID), env: common.UnsafeDevNet},
		{name: "wildcard in dev mode", env: common.UnsafeDevNet},
	}
	for _, tc := range cases {
		t.Run(tc.name, func(t *testing.T) {
			err := createWithPackageFlags(t, tc.flags, testGovernanceParty, tc.env)
			if tc.wantErr == "" {
				require.NoError(t, err)
				return
			}
			require.ErrorContains(t, err, tc.wantErr)
		})
	}
}

// TestCreateRequiresReadAsPartyOutsideDevMode: the wildcard stream is dev-only.
func TestCreateRequiresReadAsPartyOutsideDevMode(t *testing.T) {
	for _, env := range []common.Environment{common.MainNet, common.TestNet} {
		err := createWithReadAs(t, testCurrentPackageID, "", env)
		require.Error(t, err, "empty readAsParty must be rejected in %s", env)
		assert.Contains(t, err.Error(), "ReadAsParty")
	}

	require.NoError(t, createWithReadAs(t, testCurrentPackageID, "", common.UnsafeDevNet))

	for _, env := range []common.Environment{common.MainNet, common.TestNet, common.UnsafeDevNet} {
		require.NoError(t, createWithReadAs(t, testCurrentPackageID, testGovernanceParty, env), "readAsParty must be accepted in %s", env)
	}
}

func createWithAuth(t *testing.T, auth AuthConfig, env common.Environment) error {
	t.Helper()
	wc := &WatcherConfig{
		approvedCorePackages: testApprovedCorePackages(t),
		NetworkID:            "canton",
		ChainID:              vaa.ChainIDCanton,
		Rpc:                  "canton:6865",
		PackageID:            testCurrentPackageID,
		PackageVersion:       testCurrentPackageVersion,
		ReadAsParty:          testGovernanceParty,
		Auth:                 auth,
	}
	_, _, err := wc.Create(
		make(chan *common.MessagePublication, 1),
		make(chan *gossipv1.ObservationRequest, 1),
		nil, nil, nil,
		env,
	)
	return err
}

// TestCreateAuthConfigValidation locks in the all-or-nothing OAuth contract
// and the TLS requirement (no bearer auth over dev-mode plaintext).
func TestCreateAuthConfigValidation(t *testing.T) {
	full := AuthConfig{TokenURL: "https://kc.example/token", ClientID: "cid", ClientSecret: "sec"}

	// Complete config is accepted outside dev mode.
	require.NoError(t, createWithAuth(t, full, common.MainNet))
	// No auth remains fine everywhere.
	require.NoError(t, createWithAuth(t, AuthConfig{}, common.MainNet))
	require.NoError(t, createWithAuth(t, AuthConfig{}, common.UnsafeDevNet))

	// Partial configs are rejected.
	partials := []AuthConfig{
		{TokenURL: full.TokenURL},
		{ClientID: full.ClientID},
		{ClientSecret: full.ClientSecret},
		{TokenURL: full.TokenURL, ClientID: full.ClientID},
	}
	for i, p := range partials {
		err := createWithAuth(t, p, common.MainNet)
		require.Error(t, err, "partial auth config %d must be rejected", i)
		assert.Contains(t, err.Error(), "all be set together")
	}

	// Complete config is still rejected in dev mode (plaintext transport).
	err := createWithAuth(t, full, common.UnsafeDevNet)
	require.Error(t, err)
	assert.Contains(t, err.Error(), "unsafe dev mode")
}
