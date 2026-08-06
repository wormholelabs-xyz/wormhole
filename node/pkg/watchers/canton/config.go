package canton

import (
	"fmt"

	"github.com/certusone/wormhole/node/pkg/common"
	gossipv1 "github.com/certusone/wormhole/node/pkg/proto/gossip/v1"
	"github.com/certusone/wormhole/node/pkg/query"
	"github.com/certusone/wormhole/node/pkg/supervisor"
	"github.com/certusone/wormhole/node/pkg/watchers"
	"github.com/certusone/wormhole/node/pkg/watchers/interfaces"
	"github.com/wormhole-foundation/wormhole/sdk/vaa"
)

type WatcherConfig struct {
	NetworkID watchers.NetworkID // human readable name
	ChainID   vaa.ChainID
	Rpc       string // host:port of the Canton Ledger API v2 (gRPC)
	// PackageID is the package-id of the deployed wormhole-core Daml package.
	// It pins observations to the canonical core bridge; an empty PackageID
	// matches any package declaring Wormhole.Core.State.Emitter and is a
	// spoofing risk, so Create REQUIRES it outside unsafe dev mode. (Daml
	// package upgrades change the package-id, so it must be updated on upgrade.)
	PackageID string
	// ReadAsParty optionally narrows the update stream to a single Canton party.
	// Production guardians set this to the read-only guardianObserver party (an
	// observer on the Emitter/CoreState attestation surface). Empty observes
	// every party on the participant (the devnet default).
	ReadAsParty string
	// Auth carries OAuth2 client-credentials for a Keycloak-fronted Ledger API.
	// All three fields set enables per-RPC bearer tokens; all empty disables
	// authentication; anything else is a configuration error. Requires TLS,
	// so it is incompatible with unsafe dev mode (plaintext).
	Auth AuthConfig
}

func (wc *WatcherConfig) GetNetworkID() watchers.NetworkID {
	return wc.NetworkID
}

func (wc *WatcherConfig) GetChainID() vaa.ChainID {
	return wc.ChainID
}

func (wc *WatcherConfig) Create(
	msgC chan<- *common.MessagePublication,
	obsvReqC <-chan *gossipv1.ObservationRequest,
	_ <-chan *query.PerChainQueryInternal,
	_ chan<- *query.PerChainQueryResponseInternal,
	_ chan<- *common.GuardianSet,
	env common.Environment,
) (supervisor.Runnable, interfaces.Reobserver, error) {
	devMode := (env == common.UnsafeDevNet)

	// Outside unsafe dev mode the package id MUST be pinned. With an empty
	// PackageID the watcher matches PublishMessage on ANY package that declares
	// Wormhole.Core.State.Emitter — a party could upload a look-alike package
	// and have forged messages observed and signed. Pinning the core bridge's
	// package id closes that spoofing surface.
	if !devMode && wc.PackageID == "" {
		return nil, nil, fmt.Errorf("canton: PackageID must be set outside unsafe dev mode (an empty package id matches any package and is a message-spoofing risk)")
	}

	// OAuth is all-or-nothing: a partial config would silently dial without
	// credentials (or with an unusable token source) and fail at runtime.
	if wc.Auth.enabled() && !wc.Auth.complete() {
		return nil, nil, fmt.Errorf("canton: auth token URL, client id, and client secret must all be set together (or all empty)")
	}
	// Bearer credentials require transport security; dev mode is plaintext.
	if wc.Auth.enabled() && devMode {
		return nil, nil, fmt.Errorf("canton: OAuth auth requires TLS and cannot be used in unsafe dev mode")
	}

	watcher := NewWatcher(
		wc.Rpc,
		wc.PackageID,
		wc.ReadAsParty,
		wc.Auth,
		devMode,
		msgC,
		obsvReqC,
	)

	return watcher.Run, nil, nil
}
