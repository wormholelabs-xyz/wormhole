package canton

import (
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
	// PackageID is the package-id of the deployed wormhole-core Daml package. May
	// be empty to match any package version (Daml upgrades change the package-id
	// while preserving module/entity names).
	PackageID string
	// ReadAsParty optionally narrows the update stream to a single Canton party.
	// For a production guardian this should be the read-only guardianObserver
	// party (a template observer on the Emitter/CoreState attestation surface):
	// it is a stakeholder of every PublishMessage exercise and CoreState
	// transition, so reading as it observes the full message surface without any
	// authority and without depending on the operator party id. Leaving it empty
	// observes every party hosted on the participant; if the participant hosts
	// only guardianObserver the two are equivalent, but setting the flag makes
	// the intended trust boundary explicit (defense in depth). In devnet the
	// sandbox is a single participant, so the wildcard default is used.
	ReadAsParty string
}

func (wc *WatcherConfig) GetNetworkID() watchers.NetworkID {
	return wc.NetworkID
}

func (wc *WatcherConfig) GetChainID() vaa.ChainID {
	return wc.ChainID
}

//nolint:unparam // error is always nil here but the return type is required to satisfy the interface.
func (wc *WatcherConfig) Create(
	msgC chan<- *common.MessagePublication,
	obsvReqC <-chan *gossipv1.ObservationRequest,
	_ <-chan *query.PerChainQueryInternal,
	_ chan<- *query.PerChainQueryResponseInternal,
	_ chan<- *common.GuardianSet,
	env common.Environment,
) (supervisor.Runnable, interfaces.Reobserver, error) {
	devMode := (env == common.UnsafeDevNet)

	watcher := NewWatcher(
		wc.Rpc,
		wc.PackageID,
		wc.ReadAsParty,
		devMode,
		msgC,
		obsvReqC,
	)

	return watcher.Run, nil, nil
}
