// Audit of the pending transfer map against one svm/accountant program, in two directions.
// The first pass starts from the guardian's own pending transfers. The second pass starts
// from the program's pending accounts. Reads use finalized commitment. The submission worker
// confirms at confirmed commitment. Thus the program can answer a resubmission with AlreadySigned.

package accountant

import (
	"bytes"
	"context"
	"errors"
	"fmt"
	"slices"
	"time"

	"github.com/certusone/wormhole/node/pkg/solacctconn"
	"github.com/gagliardetto/solana-go"

	"go.uber.org/zap"
)

const (
	getSignaturesForAddressPageLength = 1000
	maxPendingAccountsReadPerAudit    = 10_000
	// One partition per value of the first tx id byte.
	txIDPartitions                        = 256
	olderGuardianSetsSearchedForCommitLog = 4
	// maxReobservationRequestsPerAudit bounds the requests for pending accounts this guardian
	// did not sign and does not hold.
	maxReobservationRequestsPerAudit = 100
	// The NoReplay bucket search stops at block times older than the oldest source message
	// timestamp, because a commit cannot predate its message. Source-chain clocks can run
	// ahead of Solana block time, so the stop point moves back by this margin.
	noreplayBucketSearchTimestampMargin = 24 * time.Hour
)

// solanaHistorySearchLimits bounds one kind of transaction-history search.
type solanaHistorySearchLimits struct {
	maxSearchesPerAudit             int
	maxAddressesPerSearch           int
	maxTransactionFetchesPerAddress int
}

// maxRPCCallsPerSearch counts one getSignaturesForAddress page and the transaction fetches
// for each address.
func (l solanaHistorySearchLimits) maxRPCCallsPerSearch() int {
	return l.maxAddressesPerSearch * (1 + l.maxTransactionFetchesPerAddress)
}

func (l solanaHistorySearchLimits) maxAddressesPerAudit() int {
	return l.maxSearchesPerAudit * l.maxAddressesPerSearch
}

var (
	pendingAccountCommitLogSearchLimits = solanaHistorySearchLimits{
		maxSearchesPerAudit:             100,
		maxAddressesPerSearch:           (1 + olderGuardianSetsSearchedForCommitLog) * (1 + maxSolanaSiblingTxIDs),
		maxTransactionFetchesPerAddress: 20,
	}
	noreplayBucketCommitLogSearchLimits = solanaHistorySearchLimits{
		maxSearchesPerAudit:             10,
		maxAddressesPerSearch:           1,
		maxTransactionFetchesPerAddress: 100,
	}

	maxHistoryCursors = pendingAccountCommitLogSearchLimits.maxAddressesPerAudit() +
		noreplayBucketCommitLogSearchLimits.maxAddressesPerAudit()
)

// maxSolanaTransactionFetchFailures is the number of consecutive failed fetches of one
// history entry before the search counts it as examined.
const maxSolanaTransactionFetchFailures = 3

// solanaHistoryCursors resumes each address history search below the oldest entry that the
// previous audit examined. Spam that lands after a search is newer than its cursor.
//
// SECURITY: precondition: only the audit goroutine uses it. Each audit reads the cursors of
// the previous audit and writes at most maxHistoryCursors new ones.
type solanaHistoryCursors struct {
	previous map[solana.PublicKey]solana.Signature
	next     map[solana.PublicKey]solana.Signature
	// failures holds the failed fetches of the entry at each cursor. rotate keeps the
	// addresses that keep a cursor, so it holds at most maxHistoryCursors entries.
	failures map[solana.PublicKey]solanaFetchFailures
}

// solanaFetchFailures counts the consecutive failed fetches of one history entry.
type solanaFetchFailures struct {
	signature solana.Signature
	count     int
}

// recordFetchFailure records one failed fetch of sig in the history of addr. It returns true
// when the search counts sig as examined.
func (c *solanaHistoryCursors) recordFetchFailure(addr solana.PublicKey, sig solana.Signature) bool {
	if c.failures == nil {
		c.failures = make(map[solana.PublicKey]solanaFetchFailures, maxHistoryCursors)
	}
	f := c.failures[addr]
	if f.signature != sig {
		f = solanaFetchFailures{signature: sig}
	}
	f.count++
	if f.count == maxSolanaTransactionFetchFailures {
		delete(c.failures, addr)
		return true
	}
	c.failures[addr] = f
	return false
}

// before is the page start for addr. The zero signature starts at the newest entry.
func (c *solanaHistoryCursors) before(addr solana.PublicKey) solana.Signature {
	return c.previous[addr]
}

// save keeps oldest as the next audit's page start for addr. It returns false at the cap.
func (c *solanaHistoryCursors) save(addr solana.PublicKey, oldest solana.Signature) bool {
	if c.next == nil {
		c.next = make(map[solana.PublicKey]solana.Signature, maxHistoryCursors)
	}
	if _, exists := c.next[addr]; !exists && len(c.next) == maxHistoryCursors {
		return false
	}
	c.next[addr] = oldest
	return true
}

// rotate ends an audit. Cursors the audit did not save are dropped.
func (c *solanaHistoryCursors) rotate() {
	c.previous = c.next
	c.next = nil
	for addr := range c.failures {
		if _, kept := c.previous[addr]; !kept {
			delete(c.failures, addr)
		}
	}
}

// solanaOwnPendingTransfer is one tx id of a pending transfer the backend's program
// accounts, with its record and pending accounts.
type solanaOwnPendingTransfer struct {
	pe     *pendingEntry
	record solanaObservationRecord
	// txID seeds the pending accounts below.
	txID solanaTxID
	// Nil at guardian set index zero.
	previousSetPendingPDA *solana.PublicKey
}

// solanaPendingAccountState is the state of the current guardian-set pending account of a
// transfer, from a finalized read, relative to this guardian's signature bit.
type solanaPendingAccountState uint8

const (
	solanaPendingAccountAbsent solanaPendingAccountState = iota + 1
	solanaPendingAccountLacksOwnSignature
	solanaPendingAccountHasOwnSignature
)

type solanaOwnTransferAction uint8

const (
	solanaOwnTransferAwaitQuorum solanaOwnTransferAction = iota + 1
	solanaOwnTransferResubmit
	solanaOwnTransferSearchForCommitLog
)

// decideSolanaOwnTransferAction maps the current guardian-set pending account and the
// NoReplay bit of one transfer to the audit action.
//
// SECURITY: only a commit sets the NoReplay bit. This applies to both submit paths and to
// all guardian set indices. If the bit is set, no resubmission can land. This is true also
// when the current guardian-set pending account still exists as a losing sibling.
func decideSolanaOwnTransferAction(current solanaPendingAccountState, accounted bool) (solanaOwnTransferAction, error) {
	if accounted {
		return solanaOwnTransferSearchForCommitLog, nil
	}
	switch current {
	case solanaPendingAccountAbsent, solanaPendingAccountLacksOwnSignature:
		return solanaOwnTransferResubmit, nil
	case solanaPendingAccountHasOwnSignature:
		return solanaOwnTransferAwaitQuorum, nil
	}
	return 0, fmt.Errorf("unknown pending account state %d", current)
}

// classifySolanaPendingAccount decodes a GetOwnedAccounts result for the current
// guardian-set pending account of a transfer. A prefunded account counts as absent, because
// submit_observations creates over it.
func classifySolanaPendingAccount(account solacctconn.OwnedAccount, contentDigest [32]byte, txID solanaTxID, guardianIndex uint8) (solanaPendingAccountState, error) {
	switch account.State {
	case solacctconn.AccountAbsent, solacctconn.AccountUninitialised:
		return solanaPendingAccountAbsent, nil
	case solacctconn.AccountInitialised:
	default:
		return 0, fmt.Errorf("unknown account state %d", account.State)
	}
	_, signed, err := checkPendingObservationsAccount(account.Data, contentDigest, txID, guardianIndex)
	if err != nil {
		return 0, err
	}
	if signed {
		return solanaPendingAccountHasOwnSignature, nil
	}
	return solanaPendingAccountLacksOwnSignature, nil
}

// commitLogSearchGuardianSetIndices lists, newest first, the guardian set indices whose pending
// accounts can carry the commit of an accounted transfer. The commit can sit at any older set
// that was current while the guardian missed it.
//
// SECURITY: a commit closes its pending account in the same transaction, so a current
// guardian-set pending account that still exists did not commit.
// SECURITY: postcondition: at most pendingAccountCommitLogSearchLimits.maxAddressesPerSearch indices, strictly decreasing.
func commitLogSearchGuardianSetIndices(current uint32, state solanaPendingAccountState) []uint32 {
	indices := make([]uint32, 0, pendingAccountCommitLogSearchLimits.maxAddressesPerSearch)
	if state == solanaPendingAccountAbsent {
		indices = append(indices, current)
	}
	older := min(current, olderGuardianSetsSearchedForCommitLog)
	for step := uint32(1); step <= older; step++ {
		indices = append(indices, current-step)
	}
	return indices
}

// runSolanaAudit reconciles the pending transfer map against the Solana accountant.
func (acct *Accountant) runSolanaAudit(ctx context.Context, b *solanaBackend) {
	if b == nil {
		acct.logger.Error("unable to audit the solana accountant, the backend is not configured")
		return
	}

	gs, index, err := acct.guardianIndex()
	if err != nil {
		acct.logger.Error("unable to audit the solana accountant", zap.String("backend", b.tag), zap.Error(err))
		return
	}
	// SECURITY: the index is inside the 128-bit signature bitmap.
	if index >= pendingObservationsMaxGuardians {
		acct.logger.Error("unable to audit the solana accountant, guardian index is outside the signature bitmap", zap.String("backend", b.tag), zap.Int("guardianIndex", index))
		return
	}
	guardianIndex := uint8(index) // #nosec G115 -- bounded above

	acct.publishSolanaFeePayerBalance(ctx, b)

	own := acct.snapshotSolanaOwnPendingTransfers(b, gs.Index)
	acct.logger.Debug("in AuditPendingTransfers: starting solana audit", zap.Int("numPending", len(own)))
	reconciled := acct.auditSolanaOwnPendingTransfers(ctx, b, gs.Index, guardianIndex, own)
	acct.auditSolanaProgramPendingAccounts(ctx, b, gs.Index, guardianIndex, own, reconciled)
	b.historyCursors.rotate()
	acct.logger.Debug("in AuditPendingTransfers: finished solana audit")
}

func (acct *Accountant) publishSolanaFeePayerBalance(ctx context.Context, b *solanaBackend) {
	feePayer := b.feePayer.PublicKey()
	lamports, err := b.conn.GetBalance(ctx, feePayer)
	if err != nil {
		b.metrics.feePayerErrors.Inc()
		acct.logger.Error("failed to read the solana fee payer balance", zap.String("backend", b.tag), zap.Stringer("feePayer", feePayer), zap.Error(err))
		return
	}

	b.metrics.feePayerLamports.Set(float64(lamports))
	switch solanaFeePayerBalanceLevel(lamports) {
	case solanaFeePayerEmpty:
		b.metrics.feePayerErrors.Inc()
		acct.logger.Error("the solana fee payer is empty", zap.String("backend", b.tag), zap.Stringer("feePayer", feePayer))
	case solanaFeePayerLow:
		acct.logger.Warn("the solana fee payer balance is low", zap.String("backend", b.tag), zap.Stringer("feePayer", feePayer), zap.Uint64("lamports", lamports), zap.Uint64("threshold", solanaFeePayerLowBalanceLamports))
	case solanaFeePayerFunded:
	}
}

// solanaFeePayerLowBalanceLamports is 1 SOL: about 9,500 transactions at the priority fee cap.
const solanaFeePayerLowBalanceLamports = 1_000_000_000

type solanaFeePayerLevel uint8

const (
	solanaFeePayerEmpty solanaFeePayerLevel = iota + 1
	solanaFeePayerLow
	solanaFeePayerFunded
)

func solanaFeePayerBalanceLevel(lamports uint64) solanaFeePayerLevel {
	switch {
	case lamports == 0:
		return solanaFeePayerEmpty
	case lamports < solanaFeePayerLowBalanceLamports:
		return solanaFeePayerLow
	default:
		return solanaFeePayerFunded
	}
}

// snapshotSolanaOwnPendingTransfers keys the pending transfers the backend's program
// accounts by their current guardian-set pending account, one per tx id. It grabs the
// pending transfer lock only for the copy.
func (acct *Accountant) snapshotSolanaOwnPendingTransfers(b *solanaBackend, guardianSetIndex uint32) map[solana.PublicKey]solanaOwnPendingTransfer {
	acct.pendingTransfersLock.Lock()
	entries := make([]*pendingEntry, 0, len(acct.pendingTransfers))
	for _, pe := range acct.pendingTransfers {
		if pe == nil || !b.covers(pe) {
			continue
		}
		entries = append(entries, pe)
	}
	acct.pendingTransfersLock.Unlock()

	out := make(map[solana.PublicKey]solanaOwnPendingTransfer, len(entries))
	for _, pe := range entries {
		// SECURITY: newPendingEntry sets the record of every enabled family on the entries
		// that family accounts.
		record := pe.solanaRecord(b.family)
		if record == nil {
			b.metrics.auditErrors.Inc()
			acct.logger.Error("a transfer lacks its solana observation record", zap.String("backend", b.tag), zap.String("msgId", pe.msgId))
			continue
		}
		if pe.hasBeenPendingForTooLong(b.backend) {
			b.metrics.auditErrors.Inc()
			acct.logger.Error("a transfer has been in the submit pending state for too long", zap.String("msgId", pe.msgId), zap.Stringer("lastUpdateTime", pe.updTime()))
		}

		txIDs, err := pe.solanaTxIDs()
		if err != nil {
			b.metrics.auditErrors.Inc()
			acct.logger.Error("a transfer has a tx id the solana accountant does not accept", zap.String("msgId", pe.msgId), zap.Error(err))
			continue
		}
		for _, txID := range txIDs {
			current, err := solanaPendingPDAAtSet(b, record, guardianSetIndex, txID)
			if err != nil {
				b.metrics.auditErrors.Inc()
				acct.logger.Error("failed to derive the current guardian-set pending account", zap.String("msgId", pe.msgId), zap.Error(err))
				continue
			}
			transfer := solanaOwnPendingTransfer{pe: pe, record: record, txID: txID}
			if guardianSetIndex > 0 {
				previous, err := solanaPendingPDAAtSet(b, record, guardianSetIndex-1, txID)
				if err != nil {
					b.metrics.auditErrors.Inc()
					acct.logger.Error("failed to derive the previous-set pending account", zap.String("msgId", pe.msgId), zap.Error(err))
				} else {
					transfer.previousSetPendingPDA = &previous
				}
			}
			out[current] = transfer
		}
	}
	return out
}

// solanaPendingPDAAtSet derives the pending account of record and txID under one guardian
// set index.
func solanaPendingPDAAtSet(b *solanaBackend, record solanaObservationRecord, guardianSetIndex uint32, txID solanaTxID) (solana.PublicKey, error) {
	chain, emitter, sequence := record.identity()
	return derivePendingObservationsPDA(b.program, chain, emitter, sequence, guardianSetIndex, record.committedDigest(), txID)
}

// auditSolanaOwnPendingTransfers reads the current guardian-set pending account and the
// NoReplay bit of each own transfer, then resubmits, waits, or searches for the commit. It
// returns the current guardian-set pending accounts it reconciled.
func (acct *Accountant) auditSolanaOwnPendingTransfers(ctx context.Context, b *solanaBackend, guardianSetIndex uint32, guardianIndex uint8, own map[solana.PublicKey]solanaOwnPendingTransfer) map[solana.PublicKey]struct{} {
	reconciled := make(map[solana.PublicKey]struct{}, len(own))
	if len(own) == 0 {
		return reconciled
	}

	addrs := make([]solana.PublicKey, 0, len(own))
	for addr := range own {
		addrs = append(addrs, addr)
	}

	accounts, err := b.conn.GetOwnedAccounts(ctx, addrs, b.program, solacctconn.CommitmentFinalized)
	if err != nil {
		acct.logUnresolvedSolanaTransfers(b, addrs, own, err)
		return reconciled
	}
	if len(accounts) != len(addrs) {
		acct.logUnresolvedSolanaTransfers(b, addrs, own, fmt.Errorf("want %d results, got %d", len(addrs), len(accounts)))
		return reconciled
	}

	states := make(map[solana.PublicKey]solanaPendingAccountState, len(addrs))
	classified := make([]solana.PublicKey, 0, len(addrs))
	for idx, addr := range addrs {
		pe := own[addr].pe
		state, err := classifySolanaPendingAccount(accounts[idx], own[addr].record.committedDigest(), own[addr].txID, guardianIndex)
		if err != nil {
			b.metrics.auditErrors.Inc()
			acct.logger.Error("failed to check a solana pending account", zap.String("backend", b.tag), zap.String("msgId", pe.msgId), zap.Stringer("pendingPda", addr), zap.Error(err))
			continue
		}
		states[addr] = state
		classified = append(classified, addr)
	}

	accounted, bucketOf, err := acct.readSolanaNoreplayBits(ctx, b, classified, own)
	if err != nil {
		acct.logUnresolvedSolanaTransfers(b, classified, own, err)
		return reconciled
	}

	commitSearches := 0
	skippedCommitSearches := 0
	// Sibling tx ids of one transfer share one commit, so each transfer searches once.
	commitSearched := make(map[string]struct{}, len(classified))
	// Transfers whose pending accounts held no commit log, grouped by NoReplay bucket in
	// first-seen order.
	missedBuckets := make([]solana.PublicKey, 0, noreplayBucketCommitLogSearchLimits.maxSearchesPerAudit)
	missed := make(map[solana.PublicKey]map[string]solanaOwnPendingTransfer, noreplayBucketCommitLogSearchLimits.maxSearchesPerAudit)
	for _, addr := range classified {
		isAccounted, read := accounted[addr]
		if !read {
			continue
		}
		transfer := own[addr]
		state := states[addr]
		action, err := decideSolanaOwnTransferAction(state, isAccounted)
		if err != nil {
			b.metrics.auditErrors.Inc()
			acct.logger.Error("failed to decide the audit action for a transfer", zap.String("msgId", transfer.pe.msgId), zap.Error(err))
			continue
		}
		reconciled[addr] = struct{}{}

		switch action {
		case solanaOwnTransferAwaitQuorum:
			acct.logger.Debug("the solana accountant is still collecting signatures for a transfer", zap.String("backend", b.tag), zap.String("msgId", transfer.pe.msgId))
		case solanaOwnTransferResubmit:
			acct.resubmitToSolana(ctx, b, transfer.pe, state)
		case solanaOwnTransferSearchForCommitLog:
			if _, done := commitSearched[transfer.pe.msgId]; done {
				continue
			}
			commitSearched[transfer.pe.msgId] = struct{}{}
			if commitSearches == pendingAccountCommitLogSearchLimits.maxSearchesPerAudit {
				skippedCommitSearches++
				continue
			}
			commitSearches++
			if acct.searchPendingAccountsForCommitLog(ctx, b, transfer, acct.commitLogSearchPendingAccounts(b, transfer, guardianSetIndex, state)) {
				continue
			}
			bucket := bucketOf[addr]
			group, exists := missed[bucket]
			if !exists {
				if len(missedBuckets) == noreplayBucketCommitLogSearchLimits.maxSearchesPerAudit {
					acct.logUnresolvedSolanaCommit(b, transfer)
					continue
				}
				group = make(map[string]solanaOwnPendingTransfer)
				missed[bucket] = group
				missedBuckets = append(missedBuckets, bucket)
			}
			group[transfer.pe.msgId] = transfer
		}
	}

	for _, bucket := range missedBuckets {
		group := missed[bucket]
		acct.searchNoreplayBucketForCommitLogs(ctx, b, bucket, group)
		for _, transfer := range group {
			acct.logUnresolvedSolanaCommit(b, transfer)
		}
	}

	if skippedCommitSearches > 0 {
		b.metrics.auditErrors.Inc()
		acct.logger.Error("more accounted transfers than one audit searches, the rest wait for the next audit", zap.String("backend", b.tag), zap.Int("skipped", skippedCommitSearches), zap.Int("limit", pendingAccountCommitLogSearchLimits.maxSearchesPerAudit))
	}
	return reconciled
}

// readSolanaNoreplayBits reads the NoReplay bit of each transfer in addrs, and returns the
// bucket of each. The result omits a transfer whose bucket fails to derive or decode.
func (acct *Accountant) readSolanaNoreplayBits(ctx context.Context, b *solanaBackend, addrs []solana.PublicKey, own map[solana.PublicKey]solanaOwnPendingTransfer) (map[solana.PublicKey]bool, map[solana.PublicKey]solana.PublicKey, error) {
	accounted := make(map[solana.PublicKey]bool, len(addrs))
	if len(addrs) == 0 {
		return accounted, nil, nil
	}

	// Transfers from one emitter share a bucket, so the query reads each bucket once.
	bucketOf := make(map[solana.PublicKey]solana.PublicKey, len(addrs))
	buckets := make([]solana.PublicKey, 0, len(addrs))
	seen := make(map[solana.PublicKey]struct{}, len(addrs))
	for _, addr := range addrs {
		pe := own[addr].pe
		chain, emitter, sequence := own[addr].record.identity()
		bucket, err := deriveNoreplayBucketPDA(b.noreplay, b.authority, chain, emitter, sequence)
		if err != nil {
			b.metrics.auditErrors.Inc()
			acct.logger.Error("failed to derive a noreplay bucket", zap.String("msgId", pe.msgId), zap.Error(err))
			continue
		}
		bucketOf[addr] = bucket
		if _, dup := seen[bucket]; !dup {
			seen[bucket] = struct{}{}
			buckets = append(buckets, bucket)
		}
	}
	if len(buckets) == 0 {
		return accounted, bucketOf, nil
	}

	results, err := b.conn.GetOwnedAccounts(ctx, buckets, b.noreplay, solacctconn.CommitmentFinalized)
	if err != nil {
		return nil, nil, err
	}
	if len(results) != len(buckets) {
		return nil, nil, fmt.Errorf("want %d results, got %d", len(buckets), len(results))
	}
	// An absent or prefunded bucket reads as all bits clear.
	bucketData := make(map[solana.PublicKey][]byte, len(buckets))
	for idx, bucket := range buckets {
		switch results[idx].State {
		case solacctconn.AccountAbsent, solacctconn.AccountUninitialised:
		case solacctconn.AccountInitialised:
			bucketData[bucket] = results[idx].Data
		default:
			return nil, nil, fmt.Errorf("noreplay bucket %s: unknown account state %d", bucket, results[idx].State)
		}
	}

	for addr, bucket := range bucketOf {
		pe := own[addr].pe
		raw, exists := bucketData[bucket]
		if !exists {
			accounted[addr] = false
			continue
		}
		_, _, sequence := own[addr].record.identity()
		marked, err := noreplayBitSet(raw, sequence)
		if err != nil {
			b.metrics.auditErrors.Inc()
			acct.logger.Error("failed to read a noreplay bucket", zap.String("msgId", pe.msgId), zap.Stringer("bucket", bucket), zap.Error(err))
			continue
		}
		accounted[addr] = marked
	}
	return accounted, bucketOf, nil
}

// resubmitToSolana queues a fresh observation for backend b only.
func (acct *Accountant) resubmitToSolana(ctx context.Context, b *solanaBackend, pe *pendingEntry, current solanaPendingAccountState) {
	reason := "the pending account lacks this guardian's signature"
	if current == solanaPendingAccountAbsent {
		reason = "the pending account and the noreplay bit are absent"
	}
	if acct.submitObservation(ctx, pe, b.backend, true) {
		b.metrics.auditErrors.Inc()
		acct.logger.Error("the solana accountant has not recorded this guardian's observation, resubmitting", zap.String("backend", b.tag), zap.String("msgId", pe.msgId), zap.String("reason", reason))
		return
	}
	acct.logger.Info("the solana accountant has not recorded this guardian's observation but it is already pending submission, skipping", zap.String("backend", b.tag), zap.String("msgId", pe.msgId), zap.String("reason", reason))
}

// commitLogSearchPendingAccounts derives the pending accounts of
// commitLogSearchGuardianSetIndices for each tx id of transfer. The commit closes only the
// sibling that reached quorum, so the search covers every sibling. state is the current
// guardian-set account state of transfer.txID; the other siblings include the current set.
// It logs and skips a pending account whose derivation fails.
//
// SECURITY: postcondition: at most pendingAccountCommitLogSearchLimits.maxAddressesPerSearch accounts.
func (acct *Accountant) commitLogSearchPendingAccounts(b *solanaBackend, transfer solanaOwnPendingTransfer, guardianSetIndex uint32, state solanaPendingAccountState) []solana.PublicKey {
	txIDs, err := transfer.pe.solanaTxIDs()
	if err != nil {
		b.metrics.auditErrors.Inc()
		acct.logger.Error("a transfer has a tx id the solana accountant does not accept", zap.String("msgId", transfer.pe.msgId), zap.Error(err))
		return nil
	}
	pdas := make([]solana.PublicKey, 0, pendingAccountCommitLogSearchLimits.maxAddressesPerSearch)
	for _, txID := range txIDs {
		txState := solanaPendingAccountAbsent
		if txID == transfer.txID {
			txState = state
		}
		for _, index := range commitLogSearchGuardianSetIndices(guardianSetIndex, txState) {
			pda, err := solanaPendingPDAAtSet(b, transfer.record, index, txID)
			if err != nil {
				b.metrics.auditErrors.Inc()
				acct.logger.Error("failed to derive a pending account for a commit log search", zap.String("msgId", transfer.pe.msgId), zap.Uint32("guardianSetIndex", index), zap.Error(err))
				continue
			}
			pdas = append(pdas, pda)
		}
	}
	if len(pdas) > pendingAccountCommitLogSearchLimits.maxAddressesPerSearch {
		b.metrics.auditErrors.Inc()
		acct.logger.Error("a commit log search exceeds its address limit", zap.String("msgId", transfer.pe.msgId), zap.Int("got", len(pdas)), zap.Int("limit", pendingAccountCommitLogSearchLimits.maxAddressesPerSearch))
		return pdas[:pendingAccountCommitLogSearchLimits.maxAddressesPerSearch]
	}
	return pdas
}

// searchPendingAccountsForCommitLog finds the commit log of an accounted transfer in the
// transaction histories of its pending accounts, then applies it. It returns whether it
// found the log.
func (acct *Accountant) searchPendingAccountsForCommitLog(ctx context.Context, b *solanaBackend, transfer solanaOwnPendingTransfer, pdas []solana.PublicKey) bool {
	chain, emitter, sequence := transfer.record.identity()
	for _, pda := range pdas {
		found := acct.visitAddressTransactions(ctx, b, pda, pendingAccountCommitLogSearchLimits.maxTransactionFetchesPerAddress, time.Time{}, func(sig solana.Signature, tx *solacctconn.TransactionResult) bool {
			commits, err := parseSolanaCommitLogs(tx.LogMessages, b.program)
			if err != nil {
				b.metrics.malformedLogs.Inc()
				acct.logger.Error("failed to parse transaction logs while searching for a commit", zap.String("msgId", transfer.pe.msgId), zap.Stringer("signature", sig), zap.Error(err))
			}
			for idx := range commits {
				if commits[idx].Chain != chain || commits[idx].Emitter != emitter || commits[idx].Sequence != sequence {
					continue
				}
				acct.processSolanaCommitEvent(&commits[idx], sig, b)
				return true
			}
			return false
		})
		if found {
			return true
		}
	}
	return false
}

// logUnresolvedSolanaCommit logs an accounted transfer whose commit no search found.
func (acct *Accountant) logUnresolvedSolanaCommit(b *solanaBackend, transfer solanaOwnPendingTransfer) {
	b.metrics.auditErrors.Inc()
	acct.logger.Error("a transfer is marked accounted but its commit digest is unresolved, will retry next audit", zap.String("msgId", transfer.pe.msgId))
}

// searchNoreplayBucketForCommitLogs searches the transaction history of one NoReplay bucket
// for the commit logs of group: accounted transfers whose pending accounts held no commit log.
// It applies each commit log it finds and removes its transfer from group.
//
// SECURITY: a commit log matches on chain, emitter and sequence only. processCommittedDigest
// still compares the digest and drops a mismatch.
// SECURITY: precondition len(group) > 0. The search stops below the oldest message timestamp
// of group, less noreplayBucketSearchTimestampMargin.
func (acct *Accountant) searchNoreplayBucketForCommitLogs(ctx context.Context, b *solanaBackend, bucket solana.PublicKey, group map[string]solanaOwnPendingTransfer) {
	if len(group) == 0 {
		return
	}
	var floor time.Time
	for _, transfer := range group {
		if ts := transfer.pe.msg.Timestamp; floor.IsZero() || ts.Before(floor) {
			floor = ts
		}
	}
	floor = floor.Add(-noreplayBucketSearchTimestampMargin)

	acct.visitAddressTransactions(ctx, b, bucket, noreplayBucketCommitLogSearchLimits.maxTransactionFetchesPerAddress, floor, func(sig solana.Signature, tx *solacctconn.TransactionResult) bool {
		commits, err := parseSolanaCommitLogs(tx.LogMessages, b.program)
		if err != nil {
			b.metrics.malformedLogs.Inc()
			acct.logger.Error("failed to parse transaction logs while scanning a noreplay bucket", zap.Stringer("bucket", bucket), zap.Stringer("signature", sig), zap.Error(err))
		}
		for idx := range commits {
			msgId := TransferKey{EmitterChain: uint16(commits[idx].Chain), EmitterAddress: commits[idx].Emitter, Sequence: commits[idx].Sequence}.String()
			if _, inGroup := group[msgId]; !inGroup {
				continue
			}
			acct.processSolanaCommitEvent(&commits[idx], sig, b)
			delete(group, msgId)
		}
		return len(group) == 0
	})
}

// visitAddressTransactions calls visit on the successful transactions that mention addr,
// newest first, until visit returns true. It returns whether visit did. It fetches at most
// limit transactions from one history page, starting below the cursor of addr.
//
// The cursor moves to the oldest entry examined. A failed entry counts as examined without a
// fetch. A page that ends the history, with every entry examined, clears the cursor. A
// non-zero floor ends the walk and clears the cursor at the first entry with a known block
// time before it.
func (acct *Accountant) visitAddressTransactions(ctx context.Context, b *solanaBackend, addr solana.PublicKey, limit int, floor time.Time, visit func(solana.Signature, *solacctconn.TransactionResult) bool) bool {
	entries, err := b.conn.GetSignaturesForAddress(ctx, addr, b.historyCursors.before(addr), getSignaturesForAddressPageLength)
	if err != nil {
		b.metrics.auditErrors.Inc()
		acct.logger.Error("failed to read the signatures of a pending account", zap.Stringer("pendingPda", addr), zap.Error(err))
		return false
	}
	if len(entries) > getSignaturesForAddressPageLength {
		b.metrics.auditErrors.Inc()
		acct.logger.Error("the signatures of a pending account exceed one page", zap.Stringer("pendingPda", addr), zap.Int("got", len(entries)), zap.Int("limit", getSignaturesForAddressPageLength))
		return false
	}

	var oldest solana.Signature
	examined := 0
	fetched := 0
	for _, entry := range entries {
		if !floor.IsZero() && !entry.BlockTime.IsZero() && entry.BlockTime.Before(floor) {
			b.metrics.auditErrors.Inc()
			acct.logger.Error("a history search reached its timestamp floor, the next audit restarts at the newest entry", zap.Stringer("address", addr), zap.Time("floor", floor))
			return false
		}
		if entry.Failed {
			oldest = entry.Signature
			examined++
			continue
		}
		if fetched == limit {
			break
		}
		fetched++
		tx, err := b.conn.GetTransaction(ctx, entry.Signature)
		if err != nil {
			b.metrics.auditErrors.Inc()
			acct.logger.Error("failed to read a transaction of a pending account", zap.Stringer("pendingPda", addr), zap.Stringer("signature", entry.Signature), zap.Error(err))
			if !b.historyCursors.recordFetchFailure(addr, entry.Signature) {
				// The next audit retries this entry.
				break
			}
			acct.logger.Error("skipping a history entry after repeated fetch failures", zap.Stringer("address", addr), zap.Stringer("signature", entry.Signature), zap.Int("failures", maxSolanaTransactionFetchFailures))
			oldest = entry.Signature
			examined++
			continue
		}
		oldest = entry.Signature
		examined++
		if tx == nil || tx.Failed {
			continue
		}
		if visit(entry.Signature, tx) {
			return true
		}
	}

	if examined == len(entries) && len(entries) < getSignaturesForAddressPageLength {
		return false
	}
	if oldest.IsZero() {
		oldest = b.historyCursors.before(addr)
	}
	if !b.historyCursors.save(addr, oldest) {
		b.metrics.auditErrors.Inc()
		acct.logger.Error("the solana audit history cursors are full, the search restarts at the newest entry", zap.Stringer("pendingPda", addr), zap.Int("limit", maxHistoryCursors))
	}
	return false
}

// readSolanaPendingAccounts reads the program's pending accounts at guardianSetIndex. A
// response past the RPC size limit falls back to tx id partitions. The partitioned read starts
// at programAuditPartition and adds whole partitions up to the read cap. A first partition
// past the cap is read alone.
//
// SECURITY: a partition past the size limit is skipped, so a flood of accounts in one
// partition cannot hold the others. Honest tx ids spread across all partitions.
func (acct *Accountant) readSolanaPendingAccounts(ctx context.Context, b *solanaBackend, guardianSetIndex uint32) ([]solacctconn.ProgramAccount, error) {
	accounts, err := b.conn.GetProgramAccountsByTag(ctx, b.program, pendingObservationsTag, pendingObservationsLen, guardianSetIndex, solacctconn.AllTxIDs)
	if !errors.Is(err, solacctconn.ErrResponseTooLarge) {
		return accounts, err
	}
	b.metrics.auditErrors.Inc()
	acct.logger.Error("the solana pending accounts exceed one rpc response, reading them by tx id partition", zap.String("backend", b.tag), zap.Uint8("startPartition", b.programAuditPartition))

	accounts = nil
	for range txIDPartitions {
		if len(accounts) >= maxPendingAccountsReadPerAudit {
			break
		}
		partition := b.programAuditPartition
		b.programAuditPartition++ // wraps at txIDPartitions
		part, err := b.conn.GetProgramAccountsByTag(ctx, b.program, pendingObservationsTag, pendingObservationsLen, guardianSetIndex, solacctconn.TxIDsStartingWith(partition))
		if err != nil {
			b.metrics.auditErrors.Inc()
			acct.logger.Error("failed to read a tx id partition of the solana pending accounts", zap.String("backend", b.tag), zap.Uint8("partition", partition), zap.Error(err))
			continue
		}
		// The next audit starts at a partition that would pass the read cap, so every account
		// read is examined.
		if len(accounts) > 0 && len(accounts)+len(part) > maxPendingAccountsReadPerAudit {
			b.programAuditPartition = partition
			break
		}
		accounts = append(accounts, part...)
	}
	return accounts, nil
}

// auditSolanaProgramPendingAccounts reads the program's pending accounts. It acts on each
// account whose bitmap bit for this guardian is clear. It resubmits an own transfer that the
// own-transfer pass did not reconcile. It requests a reobservation of the stored tx id of any
// other account: an unknown transfer, or a sibling tx id of an own transfer.
func (acct *Accountant) auditSolanaProgramPendingAccounts(ctx context.Context, b *solanaBackend, guardianSetIndex uint32, guardianIndex uint8, own map[solana.PublicKey]solanaOwnPendingTransfer, reconciled map[solana.PublicKey]struct{}) {
	accounts, err := acct.readSolanaPendingAccounts(ctx, b, guardianSetIndex)
	if err != nil {
		b.metrics.auditErrors.Inc()
		acct.logger.Error("failed to read the solana pending accounts", zap.String("backend", b.tag), zap.Error(err))
		return
	}
	if len(accounts) > maxPendingAccountsReadPerAudit {
		b.metrics.auditErrors.Inc()
		acct.logger.Error("the solana accountant reported more pending accounts than one audit reads, the next audit continues after the last one read", zap.String("backend", b.tag), zap.Int("total", len(accounts)), zap.Int("limit", maxPendingAccountsReadPerAudit))
	}

	// SECURITY: the pass starts after programAuditCursor and wraps, so accounts with low
	// addresses cannot hold the read cap or the search limit in every audit.
	slices.SortFunc(accounts, func(a, b solacctconn.ProgramAccount) int { return bytes.Compare(a.Address[:], b.Address[:]) })
	start, _ := slices.BinarySearchFunc(accounts, b.programAuditCursor, func(account solacctconn.ProgramAccount, cursor solana.PublicKey) int {
		if bytes.Compare(account.Address[:], cursor[:]) <= 0 {
			return -1
		}
		return 1
	})
	examine := min(len(accounts), maxPendingAccountsReadPerAudit)
	var lastExamined, lastRequested solana.PublicKey

	// During the 24-hour grace period an own transfer can also have a previous-set pending
	// account. The guardian resubmits at the current guardian set instead.
	previousSetPDAs := make(map[solana.PublicKey]struct{}, len(own))
	for _, transfer := range own {
		if transfer.previousSetPendingPDA != nil {
			previousSetPDAs[*transfer.previousSetPendingPDA] = struct{}{}
		}
	}

	reobservationRequests := 0
	skippedReobservationRequests := 0
	for step := range examine {
		account := &accounts[(start+step)%len(accounts)]
		lastExamined = account.Address
		obs, err := parsePendingObservationsAccount(account.Data)
		if err != nil {
			b.metrics.auditErrors.Inc()
			acct.logger.Error("failed to check a solana pending account", zap.String("backend", b.tag), zap.Stringer("pendingPda", account.Address), zap.Error(err))
			continue
		}
		signed, err := obs.hasSignature(guardianIndex)
		if err != nil {
			b.metrics.auditErrors.Inc()
			acct.logger.Error("failed to check a solana pending account", zap.String("backend", b.tag), zap.Stringer("pendingPda", account.Address), zap.Error(err))
			continue
		}
		if signed {
			continue
		}

		if transfer, isOwn := own[account.Address]; isOwn {
			if _, done := reconciled[account.Address]; done {
				continue
			}
			// SECURITY: the set index is a PDA seed, so a mismatch means the account is not
			// the one derived for the transfer.
			if obs.GuardianSetIndex != guardianSetIndex {
				b.metrics.auditErrors.Inc()
				acct.logger.Error("a current guardian-set pending account reports another guardian set index", zap.String("msgId", transfer.pe.msgId), zap.Uint32("accountSetIndex", obs.GuardianSetIndex), zap.Uint32("currentGuardianSetIndex", guardianSetIndex))
				continue
			}
			acct.resubmitToSolana(ctx, b, transfer.pe, solanaPendingAccountLacksOwnSignature)
			continue
		}

		if _, isPreviousSet := previousSetPDAs[account.Address]; isPreviousSet {
			acct.logger.Debug("skipping the previous-set pending account of an own transfer", zap.Stringer("pendingPda", account.Address))
			continue
		}

		if reobservationRequests == maxReobservationRequestsPerAudit {
			skippedReobservationRequests++
			continue
		}
		reobservationRequests++
		lastRequested = account.Address
		// SECURITY: the stored chain and tx id only drive a reobservation request. The watcher
		// checks the transaction on chain again before it enters the signing pipeline.
		// The audit snapshot stays fixed while the audit runs. A transfer that SubmitObservation
		// adds after the snapshot, before its signature finalizes, gets a spurious request.
		acct.handleMissingObservation(MissingObservation{ChainId: uint16(obs.Chain), TxHash: obs.TxID.Bytes()})
	}

	switch {
	case skippedReobservationRequests > 0:
		b.programAuditCursor = lastRequested
	case len(accounts) > examine:
		b.programAuditCursor = lastExamined
	default:
		b.programAuditCursor = solana.PublicKey{}
	}

	if skippedReobservationRequests > 0 {
		b.metrics.auditErrors.Inc()
		acct.logger.Error("the solana accountant has more unknown pending accounts than one audit requests, the rest wait for the next audit", zap.String("backend", b.tag), zap.Int("skipped", skippedReobservationRequests), zap.Int("limit", maxReobservationRequestsPerAudit))
	}
}

// logUnresolvedSolanaTransfers logs a failed query and every transfer in addrs whose
// status it leaves unknown. The program pending account pass still runs.
func (acct *Accountant) logUnresolvedSolanaTransfers(b *solanaBackend, addrs []solana.PublicKey, own map[solana.PublicKey]solanaOwnPendingTransfer, err error) {
	b.metrics.auditErrors.Inc()
	acct.logger.Error("unable to audit the own solana transfers, a query failed", zap.String("backend", b.tag), zap.Error(err))
	for _, addr := range addrs {
		if transfer, exists := own[addr]; exists {
			acct.logger.Error("unsure of the status of a pending transfer due to a query error", zap.String("msgId", transfer.pe.msgId))
		}
	}
}
