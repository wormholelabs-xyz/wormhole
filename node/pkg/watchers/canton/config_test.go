package canton

import (
	"testing"

	"github.com/certusone/wormhole/node/pkg/common"
	gossipv1 "github.com/certusone/wormhole/node/pkg/proto/gossip/v1"
	"github.com/stretchr/testify/assert"
	"github.com/stretchr/testify/require"
	"github.com/wormhole-foundation/wormhole/sdk/vaa"
)

func createWith(t *testing.T, pkgID string, env common.Environment) error {
	t.Helper()
	wc := &WatcherConfig{
		NetworkID: "canton",
		ChainID:   vaa.ChainIDCanton,
		Rpc:       "canton:6865",
		PackageID: pkgID,
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
		require.NoError(t, createWith(t, "abc123", env), "pinned packageID must be accepted in %s", env)
	}
}

func createWithAuth(t *testing.T, auth AuthConfig, env common.Environment) error {
	t.Helper()
	wc := &WatcherConfig{
		NetworkID: "canton",
		ChainID:   vaa.ChainIDCanton,
		Rpc:       "canton:6865",
		PackageID: "pkg",
		Auth:      auth,
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
