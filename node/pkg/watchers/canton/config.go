package canton

import (
	"fmt"
	"time"

	"github.com/certusone/wormhole/node/pkg/cantonclient"
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
	// RetiringPackageID is the previous wormhole-core package id during an
	// upgrade window (canton/README.md section 7.5). Empty outside the window.
	// SECURITY: it must be a guardian-approved, vetted core version; messages
	// it executes before RetiringPackageAcceptedUntil are signed.
	RetiringPackageID string
	// RetiringPackageAcceptedUntil is the exclusive record-time deadline for
	// RetiringPackageID (RFC 3339 UTC, whole seconds). Required with it.
	RetiringPackageAcceptedUntil string
	// PackageVersion and RetiringPackageVersion are the wormhole-core versions
	// (MAJOR.MINOR.PATCH) the participant's vetting metadata must show for
	// PackageID and RetiringPackageID. Required outside unsafe dev mode.
	PackageVersion         string
	RetiringPackageVersion string
	// ReadAsParty narrows the update stream and reobservation lookups to the
	// guardianGovernance party (signatory of CoreState, observer of every
	// Emitter). Create parses it as a cantonclient.PartyID. Empty observes every
	// party on the participant and is unsafe dev mode only.
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
	watcher, err := wc.buildWatcher(cantonclient.DefaultApprovedCorePackages(), msgC, obsvReqC, env)
	if err != nil {
		return nil, nil, err
	}
	return watcher.Run, nil, nil
}

// buildWatcher runs the checks of Create and returns the Watcher. Package tests call it to inject a client or an
// approved list.
func (wc *WatcherConfig) buildWatcher(
	approvedCorePackages cantonclient.ApprovedCorePackages,
	msgC chan<- *common.MessagePublication,
	obsvReqC <-chan *gossipv1.ObservationRequest,
	env common.Environment,
) (*Watcher, error) {
	devMode := (env == common.UnsafeDevNet)

	// Outside unsafe dev mode the package id MUST be pinned. With an empty
	// PackageID the watcher matches PublishMessage on ANY package that declares
	// Wormhole.Core.State.Emitter — a party could upload a look-alike package
	// and have forged messages observed and signed. Pinning the core bridge's
	// package id closes that spoofing surface.
	if !devMode && wc.PackageID == "" {
		return nil, fmt.Errorf("canton: PackageID must be set outside unsafe dev mode (an empty package id matches any package and is a message-spoofing risk)")
	}
	if !devMode && wc.PackageID != "" && wc.PackageVersion == "" {
		return nil, fmt.Errorf("canton: PackageVersion must be set with PackageID outside unsafe dev mode")
	}
	if !devMode && wc.RetiringPackageID != "" && wc.RetiringPackageVersion == "" {
		return nil, fmt.Errorf("canton: RetiringPackageVersion must be set with RetiringPackageID outside unsafe dev mode")
	}
	acceptedPackageIDs, err := wc.acceptedPackageIDs(time.Now())
	if err != nil {
		return nil, err
	}
	// Redundant with the PackageID check above.
	if !devMode && acceptedPackageIDs.MatchesAnyPackage() {
		return nil, fmt.Errorf("canton: the any-package wildcard is unsafe dev mode only")
	}
	// SECURITY: outside unsafe dev mode only reviewed core releases can be pinned.
	if !devMode {
		if err := approvedCorePackages.CheckAcceptedPackageIDsApproved(acceptedPackageIDs); err != nil {
			return nil, fmt.Errorf("canton: %w", err)
		}
	}

	// SECURITY: outside unsafe dev mode the stream MUST be narrowed to the
	// guardianGovernance party. The read-party signatory check admits an Emitter
	// of which that party is the owner or the registrar; the registrar pin drops
	// the owner-only case. A wildcard stream would also carry publishes
	// witnessed by any other party hosted on the participant.
	readScope, err := cantonclient.ParseReadScope(wc.ReadAsParty, devMode)
	if err != nil {
		return nil, fmt.Errorf("canton: ReadAsParty: %w", err)
	}

	// OAuth is all-or-nothing: a partial config would silently dial without
	// credentials (or with an unusable token source) and fail at runtime.
	if wc.Auth.enabled() && !wc.Auth.complete() {
		return nil, fmt.Errorf("canton: auth token URL, client id, and client secret must all be set together (or all empty)")
	}
	// Bearer credentials require transport security; dev mode is plaintext.
	if wc.Auth.enabled() && devMode {
		return nil, fmt.Errorf("canton: OAuth auth requires TLS and cannot be used in unsafe dev mode")
	}

	return newWatcherWithApprovedList(
		approvedCorePackages,
		wc.Rpc,
		acceptedPackageIDs,
		readScope,
		wc.Auth,
		devMode,
		msgC,
		obsvReqC,
	)
}

// acceptedPackageIDs is the dev-mode wildcard when every package flag is
// empty; Create rejects that case outside unsafe dev mode before calling it.
func (wc *WatcherConfig) acceptedPackageIDs(startupWallClock time.Time) (cantonclient.AcceptedPackageIDs, error) {
	current := cantonclient.CurrentPackageConfig{
		PackageIDHex: wc.PackageID,
		Version:      wc.PackageVersion,
	}
	retiring := cantonclient.RetiringPackageConfig{
		PackageIDHex:     wc.RetiringPackageID,
		AcceptedUntilUTC: wc.RetiringPackageAcceptedUntil,
		Version:          wc.RetiringPackageVersion,
	}
	if current == (cantonclient.CurrentPackageConfig{}) && retiring == (cantonclient.RetiringPackageConfig{}) {
		return cantonclient.AcceptAnyPackageID(), nil
	}
	acceptedPackageIDs, err := cantonclient.ParseAcceptedPackageIDs(current, retiring, startupWallClock)
	if err != nil {
		return cantonclient.AcceptedPackageIDs{}, fmt.Errorf("canton: %w", err)
	}
	return acceptedPackageIDs, nil
}
