// Package common defines the chain-family abstraction the leak
// harness uses to plug in fake-RPC implementations per chain.
//
// Each chain family (EVM, Sui, Cosmwasm, XRPL, …) provides:
//
//   1. A FaultableServer — an httptest-style fake whose behaviour can
//      be flipped at runtime;
//   2. A Worker — a loop that dials the fake, performs whatever
//      chain-family-specific lifecycle exercises the relevant leak
//      class, and closes cleanly. The loop must respect ctx.Done.
//
// The harness instantiates one (server, worker) pair per configured
// chain and runs them concurrently.
package common

import "context"

// Fault enumerates the failure modes a FaultableServer can be driven
// into. Not every family supports every fault; unsupported faults are
// no-ops by convention.
type Fault int

const (
	FaultNone Fault = iota
	FaultCloseAllConnections
	FaultMalformed
	FaultSlow
	FaultStuck
)

// FaultableServer is the runtime contract for a fake RPC.
type FaultableServer interface {
	URL() string
	Set(f Fault)
	Stop()
}

// Worker is the contract for a chain-family-specific lifecycle loop.
type Worker interface {
	// Run loops dial-use-close against `serverURL` until ctx is
	// cancelled. Errors are swallowed by design: the harness measures
	// leakage, not correctness of individual calls.
	Run(ctx context.Context, serverURL string)
}

// Family bundles the constructor for a fake server and a worker.
// `Name` is the FakeKind identifier used by scenarios.
type Family struct {
	Name      string
	NewServer func(id uint64) FaultableServer
	NewWorker func() Worker
}
