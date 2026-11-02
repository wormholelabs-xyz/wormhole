package accountant

import (
	"fmt"
	"time"

	"go.uber.org/zap"
)

// Ledger is an accountant ledger: the wormchain contracts or the Solana programs.
type Ledger uint8

const (
	LedgerUnset Ledger = iota
	LedgerWormchain
	LedgerSolana
)

// ParseLedger parses an active ledger flag value. The empty string is LedgerUnset.
func ParseLedger(flagValue string) (Ledger, error) {
	switch flagValue {
	case "":
		return LedgerUnset, nil
	case "wormchain":
		return LedgerWormchain, nil
	case "solana":
		return LedgerSolana, nil
	}
	return LedgerUnset, fmt.Errorf("ledger %q is not wormchain or solana", flagValue)
}

func (l Ledger) String() string {
	switch l {
	case LedgerWormchain:
		return "wormchain"
	case LedgerSolana:
		return "solana"
	}
	return "unset"
}

// ActiveLedgers selects, per program family, the ledger that releases and drops transfers.
// The other enabled ledger is the mirror.
type ActiveLedgers struct {
	WTT Ledger
	NTT Ledger
}

// resolveActiveLedger returns requested, or for LedgerUnset the only enabled ledger, or
// wormchain when both are enabled.
func resolveActiveLedger(requested Ledger, wormchainEnabled bool, solanaEnabled bool) Ledger {
	if requested != LedgerUnset {
		return requested
	}
	switch {
	case wormchainEnabled:
		return LedgerWormchain
	case solanaEnabled:
		return LedgerSolana
	}
	return LedgerUnset
}

// checkActiveLedgerEnabled returns an error if active names a ledger that family does not enable.
func checkActiveLedgerEnabled(family string, active Ledger, wormchainEnabled bool, solanaEnabled bool) error {
	switch {
	case active == LedgerWormchain && !wormchainEnabled:
		return fmt.Errorf("the active %s ledger is wormchain, but the wormchain %s accountant is not enabled", family, family)
	case active == LedgerSolana && !solanaEnabled:
		return fmt.Errorf("the active %s ledger is solana, but the solana %s accountant is not enabled", family, family)
	}
	return nil
}

// resolveActiveLedgersFromConfig resolves both families from the static configuration, so
// FeatureString agrees with Start.
func (acct *Accountant) resolveActiveLedgersFromConfig(requested ActiveLedgers) ActiveLedgers {
	return ActiveLedgers{
		WTT: resolveActiveLedger(requested.WTT, acct.wormchainBaseEnabled(), !acct.solanaCfg.Program.IsZero()),
		NTT: resolveActiveLedger(requested.NTT, acct.wormchainNttEnabled(), !acct.solanaCfg.NttProgram.IsZero()),
	}
}

// checkActiveLedgersEnabled returns an error if an explicit active ledger is not enabled.
func (acct *Accountant) checkActiveLedgersEnabled() error {
	if err := checkActiveLedgerEnabled("WTT", acct.activeLedgers.WTT, acct.wormchainBaseEnabled(), !acct.solanaCfg.Program.IsZero()); err != nil {
		return err
	}
	return checkActiveLedgerEnabled("NTT", acct.activeLedgers.NTT, acct.wormchainNttEnabled(), !acct.solanaCfg.NttProgram.IsZero())
}

// backendLedger returns the ledger that backend submits to.
func backendLedger(backend accountantBackend) Ledger {
	if backend == backendWormchain {
		return LedgerWormchain
	}
	return LedgerSolana
}

// isActiveBackend returns true if backend submits pe to the active ledger of its family.
func (acct *Accountant) isActiveBackend(pe *pendingEntry, backend accountantBackend) bool {
	active := acct.activeLedgers.WTT
	if pe.isNTT {
		active = acct.activeLedgers.NTT
	}
	return backendLedger(backend) == active
}

// backendOutcome is what one backend reported for a pending transfer.
type backendOutcome uint8

const (
	backendOutcomePending backendOutcome = iota
	backendOutcomeCommitted
	backendOutcomeRejected
	backendOutcomeMirrorTimedOut
)

// maxMirrorPendingAfterRelease bounds how long a released transfer waits for the mirror ledger.
const maxMirrorPendingAfterRelease = 24 * time.Hour

// backendOutcome returns what backend reported for pe. It grabs the state lock.
func (pe *pendingEntry) backendOutcome(backend accountantBackend) backendOutcome {
	pe.stateLock.Lock()
	defer pe.stateLock.Unlock()
	return pe.state.outcome[backend]
}

// backendSettled returns true if backend committed, rejected, or timed out on pe. Submission
// to backend stops when it is settled. It grabs the state lock.
func (pe *pendingEntry) backendSettled(backend accountantBackend) bool {
	return pe.backendOutcome(backend) != backendOutcomePending
}

// isReleasedByActiveLedger returns true if the active ledger committed pe. It grabs the state lock.
func (pe *pendingEntry) isReleasedByActiveLedger() bool {
	pe.stateLock.Lock()
	defer pe.stateLock.Unlock()
	return pe.state.releasedByActiveLedger
}

// solanaFamilyBackend returns the backend slot of a Solana program family.
func solanaFamilyBackend(family solanaProgramFamily) accountantBackend {
	if family == solanaFamilyNTT {
		return backendSolanaNTT
	}
	return backendSolana
}

// coveringBackends returns the enabled backends that account pe.
func (acct *Accountant) coveringBackends(pe *pendingEntry) []accountantBackend {
	out := make([]accountantBackend, 0, numAccountantBackends)
	for backend := range numAccountantBackends {
		if _, _, covered := acct.backendChannel(pe, backend); covered {
			out = append(out, backend)
		}
	}
	return out
}

// backendTag returns the log and metric tag of backend for pe.
func (acct *Accountant) backendTag(pe *pendingEntry, backend accountantBackend) string {
	_, tag, _ := acct.backendChannel(pe, backend)
	return tag
}

// recordBackendCommitAlreadyLocked records that backend committed pe. A commit by the active
// ledger publishes pe. A commit by a backend whose outcome is set is a no-op. The caller holds
// pendingTransfersLock.
func (acct *Accountant) recordBackendCommitAlreadyLocked(pe *pendingEntry, backend accountantBackend, source string) {
	active := acct.isActiveBackend(pe, backend)
	pe.stateLock.Lock()
	if pe.state.outcome[backend] != backendOutcomePending {
		pe.stateLock.Unlock()
		return
	}
	pe.state.outcome[backend] = backendOutcomeCommitted
	if active {
		pe.state.releasedByActiveLedger = true
		pe.state.releasedAt = time.Now()
	}
	pe.stateLock.Unlock()

	if active {
		acct.logger.Info("the active ledger committed the transfer, publishing it", zap.String("msgId", pe.msgId), zap.String("source", source))
		if pe.enforceFlag {
			select {
			case acct.msgChan <- pe.msg:
				acct.logger.Debug("published transfer to channel", zap.String("msgId", pe.msgId))
			default:
				acct.logger.Error("unable to publish transfer because the channel is full", zap.String("msgId", pe.msgId))
			}
		}
		acct.releasedEntryCount++
		for _, mirror := range acct.coveringBackends(pe) {
			if mirror != backend && pe.backendOutcome(mirror) == backendOutcomeRejected {
				acct.logger.Error("the active ledger committed a transfer that a mirror ledger rejected", zap.String("msgId", pe.msgId), zap.String("mirror", acct.backendTag(pe, mirror)))
			}
		}
	} else {
		mirrorSynced.WithLabelValues(acct.backendTag(pe, backend)).Inc()
		acct.logger.Info("a mirror ledger committed the transfer", zap.String("msgId", pe.msgId), zap.String("source", source))
	}
	acct.deleteIfCompleteAlreadyLocked(pe)
}

// recordBackendRejectionAlreadyLocked records that backend rejected pe. A rejection by the
// active ledger drops pe. A rejection by a mirror ledger counts a divergence. A rejection by a
// backend whose outcome is set is a no-op. The caller holds pendingTransfersLock.
func (acct *Accountant) recordBackendRejectionAlreadyLocked(pe *pendingEntry, backend accountantBackend, reason string) {
	pe.stateLock.Lock()
	if pe.state.outcome[backend] != backendOutcomePending {
		pe.stateLock.Unlock()
		return
	}
	pe.state.outcome[backend] = backendOutcomeRejected
	pe.stateLock.Unlock()

	if acct.isActiveBackend(pe, backend) {
		for _, mirror := range acct.coveringBackends(pe) {
			if mirror != backend && pe.backendOutcome(mirror) == backendOutcomeCommitted {
				mirrorDivergences.WithLabelValues(acct.backendTag(pe, mirror)).Inc()
				acct.logger.Error("the active ledger rejected a transfer that a mirror ledger committed", zap.String("msgId", pe.msgId), zap.String("mirror", acct.backendTag(pe, mirror)), zap.String("reason", reason))
			}
		}
		acct.logger.Error("the active ledger rejected the transfer, dropping it", zap.String("msgId", pe.msgId), zap.String("reason", reason))
		acct.deletePendingTransferAlreadyLocked(pe.msgId)
		return
	}
	mirrorDivergences.WithLabelValues(acct.backendTag(pe, backend)).Inc()
	acct.logger.Error("a mirror ledger rejected the transfer, it stops receiving it", zap.String("msgId", pe.msgId), zap.String("mirror", acct.backendTag(pe, backend)), zap.String("reason", reason))
	acct.deleteIfCompleteAlreadyLocked(pe)
}

// handleRejectedTransfer records that backend rejected the transfer msgId. It grabs the lock.
func (acct *Accountant) handleRejectedTransfer(msgId string, backend accountantBackend, reason string) {
	acct.pendingTransfersLock.Lock()
	defer acct.pendingTransfersLock.Unlock()
	if pe, exists := acct.pendingTransfers[msgId]; exists {
		acct.recordBackendRejectionAlreadyLocked(pe, backend, reason)
	}
}

// deleteIfCompleteAlreadyLocked deletes pe once the active ledger released it and every
// covering backend is settled. The caller holds pendingTransfersLock.
func (acct *Accountant) deleteIfCompleteAlreadyLocked(pe *pendingEntry) {
	if !pe.isReleasedByActiveLedger() {
		return
	}
	for _, backend := range acct.coveringBackends(pe) {
		if !pe.backendSettled(backend) {
			acct.publishPendingGaugesAlreadyLocked()
			return
		}
	}
	acct.deletePendingTransferAlreadyLocked(pe.msgId)
}

// publishPendingGaugesAlreadyLocked sets the outstanding and mirror pending gauges. The caller
// holds pendingTransfersLock.
func (acct *Accountant) publishPendingGaugesAlreadyLocked() {
	transfersOutstanding.Set(float64(len(acct.pendingTransfers) - acct.releasedEntryCount))
	mirrorPendingTransfers.Set(float64(acct.releasedEntryCount))
}

// expireStalledMirrors times out the mirror of each transfer released more than
// maxMirrorPendingAfterRelease ago, and deletes the transfer. It grabs the lock.
func (acct *Accountant) expireStalledMirrors() {
	acct.pendingTransfersLock.Lock()
	defer acct.pendingTransfersLock.Unlock()

	for _, pe := range acct.pendingTransfers {
		pe.stateLock.Lock()
		stalled := pe.state.releasedByActiveLedger && time.Since(pe.state.releasedAt) > maxMirrorPendingAfterRelease
		pe.stateLock.Unlock()
		if !stalled {
			continue
		}
		for _, backend := range acct.coveringBackends(pe) {
			pe.stateLock.Lock()
			timedOut := pe.state.outcome[backend] == backendOutcomePending
			if timedOut {
				pe.state.outcome[backend] = backendOutcomeMirrorTimedOut
			}
			pe.stateLock.Unlock()
			if timedOut {
				mirrorTimeouts.WithLabelValues(acct.backendTag(pe, backend)).Inc()
				acct.logger.Error("a mirror ledger did not commit a released transfer before the deadline", zap.String("msgId", pe.msgId), zap.String("mirror", acct.backendTag(pe, backend)), zap.Duration("deadline", maxMirrorPendingAfterRelease))
			}
		}
		acct.deleteIfCompleteAlreadyLocked(pe)
	}
}
