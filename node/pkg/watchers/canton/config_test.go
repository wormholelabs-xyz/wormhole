package canton

import (
	"testing"
	"time"

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
)

func createWith(t *testing.T, pkgID string, env common.Environment) error {
	t.Helper()
	return createWithReadAs(t, pkgID, testGovernanceParty, env)
}

func createWithReadAs(t *testing.T, pkgID string, readAsParty string, env common.Environment) error {
	t.Helper()
	return createWithPackageIDs(t, pkgID, "", "", readAsParty, env)
}

func createWithPackageIDs(t *testing.T, currentPackageID string, retiringPackageID string, retiringPackageAcceptedUntil string, readAsParty string, env common.Environment) error {
	t.Helper()
	wc := &WatcherConfig{
		NetworkID:                    "canton",
		ChainID:                      vaa.ChainIDCanton,
		Rpc:                          "canton:6865",
		PackageID:                    currentPackageID,
		RetiringPackageID:            retiringPackageID,
		RetiringPackageAcceptedUntil: retiringPackageAcceptedUntil,
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
	cases := []struct {
		name                         string
		currentPackageID             string
		retiringPackageID            string
		retiringPackageAcceptedUntil string
		env                          common.Environment
		wantErr                      string
	}{
		{name: "valid single", currentPackageID: testCurrentPackageID, env: common.MainNet},
		{name: "valid window", currentPackageID: testCurrentPackageID, retiringPackageID: testRetiringPackageID, retiringPackageAcceptedUntil: inOneDay, env: common.MainNet},
		{name: "valid window in dev mode", currentPackageID: testCurrentPackageID, retiringPackageID: testRetiringPackageID, retiringPackageAcceptedUntil: inOneDay, env: common.UnsafeDevNet},
		{name: "window with a past deadline", currentPackageID: testCurrentPackageID, retiringPackageID: testRetiringPackageID, retiringPackageAcceptedUntil: retiringDeadlineFromNow(-time.Hour), env: common.MainNet},
		{name: "window longer than the maximum", currentPackageID: testCurrentPackageID, retiringPackageID: testRetiringPackageID, retiringPackageAcceptedUntil: retiringDeadlineFromNow(8 * 24 * time.Hour), env: common.MainNet, wantErr: "more than"},
		{name: "retiring id without deadline", currentPackageID: testCurrentPackageID, retiringPackageID: testRetiringPackageID, env: common.MainNet, wantErr: "requires an accepted-until deadline"},
		{name: "deadline without retiring id", currentPackageID: testCurrentPackageID, retiringPackageAcceptedUntil: inOneDay, env: common.MainNet, wantErr: "requires a retiring core package id"},
		{name: "deadline with offset", currentPackageID: testCurrentPackageID, retiringPackageID: testRetiringPackageID, retiringPackageAcceptedUntil: "2026-11-18T13:00:00+01:00", env: common.MainNet, wantErr: "RFC 3339 UTC"},
		{name: "duplicate", currentPackageID: testCurrentPackageID, retiringPackageID: testCurrentPackageID, retiringPackageAcceptedUntil: inOneDay, env: common.MainNet, wantErr: "must differ"},
		{name: "malformed current", currentPackageID: "abc123", env: common.MainNet, wantErr: "current core package id"},
		{name: "malformed current in dev mode", currentPackageID: "pkg", env: common.UnsafeDevNet, wantErr: "current core package id"},
		{name: "uppercase current", currentPackageID: "AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA", env: common.MainNet, wantErr: "current core package id"},
		{name: "malformed retiring", currentPackageID: testCurrentPackageID, retiringPackageID: "abc123", retiringPackageAcceptedUntil: inOneDay, env: common.MainNet, wantErr: "retiring core package id"},
		{name: "three ids", currentPackageID: testCurrentPackageID, retiringPackageID: testRetiringPackageID + "," + testOtherPackageID, retiringPackageAcceptedUntil: inOneDay, env: common.MainNet, wantErr: "retiring core package id"},
		{name: "three ids in current", currentPackageID: testCurrentPackageID + "," + testRetiringPackageID, retiringPackageID: testOtherPackageID, retiringPackageAcceptedUntil: inOneDay, env: common.MainNet, wantErr: "current core package id"},
		{name: "missing current outside dev mode", retiringPackageID: testRetiringPackageID, retiringPackageAcceptedUntil: inOneDay, env: common.MainNet, wantErr: "PackageID must be set"},
		{name: "missing current in testnet", retiringPackageID: testRetiringPackageID, retiringPackageAcceptedUntil: inOneDay, env: common.TestNet, wantErr: "PackageID must be set"},
		{name: "retiring without current in dev mode", retiringPackageID: testRetiringPackageID, retiringPackageAcceptedUntil: inOneDay, env: common.UnsafeDevNet, wantErr: "current core package id"},
		{name: "deadline alone in dev mode", retiringPackageAcceptedUntil: inOneDay, env: common.UnsafeDevNet, wantErr: "current core package id"},
		{name: "wildcard in dev mode", env: common.UnsafeDevNet},
	}
	for _, tc := range cases {
		t.Run(tc.name, func(t *testing.T) {
			err := createWithPackageIDs(t, tc.currentPackageID, tc.retiringPackageID, tc.retiringPackageAcceptedUntil, testGovernanceParty, tc.env)
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
		NetworkID:   "canton",
		ChainID:     vaa.ChainIDCanton,
		Rpc:         "canton:6865",
		PackageID:   testCurrentPackageID,
		ReadAsParty: testGovernanceParty,
		Auth:        auth,
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
