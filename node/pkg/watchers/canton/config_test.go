package canton

import (
	"testing"

	"github.com/certusone/wormhole/node/pkg/common"
	gossipv1 "github.com/certusone/wormhole/node/pkg/proto/gossip/v1"
	"github.com/stretchr/testify/assert"
	"github.com/stretchr/testify/require"
	"github.com/wormhole-foundation/wormhole/sdk/vaa"
)

const (
	testGovernanceParty  = "guardianGovernance::1220abcd"
	testPrimaryPackageID = "1111111111111111111111111111111111111111111111111111111111111111"
	testNextPackageID    = "2222222222222222222222222222222222222222222222222222222222222222"
	testOtherPackageID   = "3333333333333333333333333333333333333333333333333333333333333333"
)

func createWith(t *testing.T, pkgID string, env common.Environment) error {
	t.Helper()
	return createWithReadAs(t, pkgID, testGovernanceParty, env)
}

func createWithReadAs(t *testing.T, pkgID string, readAsParty string, env common.Environment) error {
	t.Helper()
	return createWithPackageIDs(t, pkgID, "", readAsParty, env)
}

func createWithPackageIDs(t *testing.T, primaryPackageID string, nextPackageID string, readAsParty string, env common.Environment) error {
	t.Helper()
	wc := &WatcherConfig{
		NetworkID:     "canton",
		ChainID:       vaa.ChainIDCanton,
		Rpc:           "canton:6865",
		PackageID:     primaryPackageID,
		PackageIDNext: nextPackageID,
		ReadAsParty:   readAsParty,
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
		require.NoError(t, createWith(t, testPrimaryPackageID, env), "pinned packageID must be accepted in %s", env)
	}
}

func TestCreateValidatesCorePackageIDs(t *testing.T) {
	cases := []struct {
		name             string
		primaryPackageID string
		nextPackageID    string
		env              common.Environment
		wantErr          string
	}{
		{name: "valid single", primaryPackageID: testPrimaryPackageID, env: common.MainNet},
		{name: "valid pair", primaryPackageID: testPrimaryPackageID, nextPackageID: testNextPackageID, env: common.MainNet},
		{name: "valid pair in dev mode", primaryPackageID: testPrimaryPackageID, nextPackageID: testNextPackageID, env: common.UnsafeDevNet},
		{name: "duplicate", primaryPackageID: testPrimaryPackageID, nextPackageID: testPrimaryPackageID, env: common.MainNet, wantErr: "must differ"},
		{name: "malformed primary", primaryPackageID: "abc123", env: common.MainNet, wantErr: "primary core package id"},
		{name: "malformed primary in dev mode", primaryPackageID: "pkg", env: common.UnsafeDevNet, wantErr: "primary core package id"},
		{name: "uppercase primary", primaryPackageID: "AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA", env: common.MainNet, wantErr: "primary core package id"},
		{name: "malformed next", primaryPackageID: testPrimaryPackageID, nextPackageID: "abc123", env: common.MainNet, wantErr: "next core package id"},
		{name: "three ids", primaryPackageID: testPrimaryPackageID, nextPackageID: testNextPackageID + "," + testOtherPackageID, env: common.MainNet, wantErr: "next core package id"},
		{name: "three ids in primary", primaryPackageID: testPrimaryPackageID + "," + testNextPackageID, nextPackageID: testOtherPackageID, env: common.MainNet, wantErr: "primary core package id"},
		{name: "missing primary outside dev mode", nextPackageID: testNextPackageID, env: common.MainNet, wantErr: "PackageID must be set"},
		{name: "missing primary in testnet", nextPackageID: testNextPackageID, env: common.TestNet, wantErr: "PackageID must be set"},
		{name: "next without primary in dev mode", nextPackageID: testNextPackageID, env: common.UnsafeDevNet, wantErr: "primary core package id"},
		{name: "wildcard in dev mode", env: common.UnsafeDevNet},
	}
	for _, tc := range cases {
		t.Run(tc.name, func(t *testing.T) {
			err := createWithPackageIDs(t, tc.primaryPackageID, tc.nextPackageID, testGovernanceParty, tc.env)
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
		err := createWithReadAs(t, testPrimaryPackageID, "", env)
		require.Error(t, err, "empty readAsParty must be rejected in %s", env)
		assert.Contains(t, err.Error(), "ReadAsParty")
	}

	require.NoError(t, createWithReadAs(t, testPrimaryPackageID, "", common.UnsafeDevNet))

	for _, env := range []common.Environment{common.MainNet, common.TestNet, common.UnsafeDevNet} {
		require.NoError(t, createWithReadAs(t, testPrimaryPackageID, testGovernanceParty, env), "readAsParty must be accepted in %s", env)
	}
}

func createWithAuth(t *testing.T, auth AuthConfig, env common.Environment) error {
	t.Helper()
	wc := &WatcherConfig{
		NetworkID:   "canton",
		ChainID:     vaa.ChainIDCanton,
		Rpc:         "canton:6865",
		PackageID:   testPrimaryPackageID,
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
