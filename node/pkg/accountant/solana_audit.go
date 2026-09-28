// Audit of the pending transfer map against the svm/accountant program, in both
// directions: from the guardian's own pending transfers, then from the program's pending
// accounts. Reads are at finalized commitment while the submission worker confirms at
// confirmed, so a resubmission that the program answers with AlreadySigned is expected.

package accountant

import (
	"context"
	"fmt"

	"github.com/certusone/wormhole/node/pkg/solacctconn"
	"github.com/gagliardetto/solana-go"

	"go.uber.org/zap"
)

const (
	maxSolanaProgramPendingAccountsPerAudit = 10_000

	// Each search costs up to 2 * (1 + maxSolanaCommitSearchSignatures) RPC calls.
	maxSolanaCommitSearchesPerAudit = 100
	// Signatures read per pending account in a commit search.
	maxSolanaCommitSearchSignatures = 20

	// Each reobservation search costs up to 1 + maxSolanaReobservationSearchSignatures RPC calls.
	maxSolanaReobservationSearchesPerAudit = 100
	// Signatures read per unknown pending account in a reobservation search.
	maxSolanaReobservationSearchSignatures = 10
)

// solanaOwnPendingTransfer is a Token Bridge pending transfer with its pending accounts.
type solanaOwnPendingTransfer struct {
	pe *pendingEntry
	// Nil at guardian set index zero.
	previousSetPendingPDA *solana.PublicKey
}

// solanaPendingAccountState is the live-set pending account of a transfer, as this
// guardian sees it.
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
	solanaOwnTransferSearchCommit
)

// decideSolanaOwnTransferAction maps the live-set pending account and the NoReplay bit of
// one transfer to the audit action.
//
// SECURITY: the NoReplay bit is set only at commit, on either submit path and at any
// guardian set index. A set bit means no resubmission can land, even when the live-set
// pending account still exists as a losing sibling.
func decideSolanaOwnTransferAction(live solanaPendingAccountState, accounted bool) (solanaOwnTransferAction, error) {
	if accounted {
		return solanaOwnTransferSearchCommit, nil
	}
	switch live {
	case solanaPendingAccountAbsent, solanaPendingAccountLacksOwnSignature:
		return solanaOwnTransferResubmit, nil
	case solanaPendingAccountHasOwnSignature:
		return solanaOwnTransferAwaitQuorum, nil
	}
	return 0, fmt.Errorf("unknown pending account state %d", live)
}

// classifySolanaPendingAccount decodes a GetMultipleAccounts result for the live-set
// pending account of a transfer.
func classifySolanaPendingAccount(account *solacctconn.AccountResult, contentDigest [32]byte, guardianIndex uint8) (solanaPendingAccountState, error) {
	if account == nil {
		return solanaPendingAccountAbsent, nil
	}
	_, signed, err := checkPendingObservationsAccount(account.Data, contentDigest, guardianIndex)
	if err != nil {
		return 0, err
	}
	if signed {
		return solanaPendingAccountHasOwnSignature, nil
	}
	return solanaPendingAccountLacksOwnSignature, nil
}

// solanaCommitSearchPDAs lists the pending accounts whose transactions can carry the
// commit of an accounted transfer.
//
// SECURITY: a commit closes its pending account in the same transaction, so a live-set
// pending account that still exists did not commit.
func solanaCommitSearchPDAs(livePDA solana.PublicKey, live solanaPendingAccountState, transfer solanaOwnPendingTransfer) []solana.PublicKey {
	pdas := make([]solana.PublicKey, 0, 2)
	if live == solanaPendingAccountAbsent {
		pdas = append(pdas, livePDA)
	}
	if transfer.previousSetPendingPDA != nil {
		pdas = append(pdas, *transfer.previousSetPendingPDA)
	}
	return pdas
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
	reconciled := acct.auditSolanaOwnPendingTransfers(ctx, b, guardianIndex, own)
	acct.auditSolanaProgramPendingAccounts(ctx, b, gs.Index, guardianIndex, own, reconciled)
	acct.logger.Debug("in AuditPendingTransfers: finished solana audit")
}

func (acct *Accountant) publishSolanaFeePayerBalance(ctx context.Context, b *solanaBackend) {
	feePayer := b.feePayer.PublicKey()
	lamports, err := b.conn.GetBalance(ctx, feePayer)
	if err != nil {
		solanaFeePayerErrors.Inc()
		acct.logger.Error("failed to read the solana fee payer balance", zap.String("backend", b.tag), zap.Stringer("feePayer", feePayer), zap.Error(err))
		return
	}

	solanaFeePayerLamports.Set(float64(lamports))
	if lamports == 0 {
		solanaFeePayerErrors.Inc()
		acct.logger.Error("the solana fee payer is empty", zap.String("backend", b.tag), zap.Stringer("feePayer", feePayer))
	}
}

// snapshotSolanaOwnPendingTransfers keys the Token Bridge pending transfers by their
// live-set pending account. It grabs the pending transfer lock only for the copy.
func (acct *Accountant) snapshotSolanaOwnPendingTransfers(b *solanaBackend, guardianSetIndex uint32) map[solana.PublicKey]solanaOwnPendingTransfer {
	acct.pendingTransfersLock.Lock()
	entries := make([]*pendingEntry, 0, len(acct.pendingTransfers))
	for _, pe := range acct.pendingTransfers {
		if pe == nil || pe.isNTT {
			continue
		}
		entries = append(entries, pe)
	}
	acct.pendingTransfersLock.Unlock()

	out := make(map[solana.PublicKey]solanaOwnPendingTransfer, len(entries))
	for _, pe := range entries {
		// SECURITY: newPendingEntry sets solanaFields on every Token Bridge entry while
		// Solana is enabled.
		if pe.solanaFields == nil {
			solanaAuditErrors.Inc()
			acct.logger.Error("a token bridge transfer lacks its solana observation record", zap.String("msgId", pe.msgId))
			continue
		}
		if pe.hasBeenPendingForTooLong(backendSolana) {
			solanaAuditErrors.Inc()
			acct.logger.Error("a transfer has been in the submit pending state for too long", zap.String("msgId", pe.msgId), zap.Stringer("lastUpdateTime", pe.updTime()))
		}

		live, err := solanaPendingPDAAtSet(b, pe, guardianSetIndex)
		if err != nil {
			solanaAuditErrors.Inc()
			acct.logger.Error("failed to derive the live-set pending account", zap.String("msgId", pe.msgId), zap.Error(err))
			continue
		}
		transfer := solanaOwnPendingTransfer{pe: pe}
		if guardianSetIndex > 0 {
			previous, err := solanaPendingPDAAtSet(b, pe, guardianSetIndex-1)
			if err != nil {
				solanaAuditErrors.Inc()
				acct.logger.Error("failed to derive the previous-set pending account", zap.String("msgId", pe.msgId), zap.Error(err))
			} else {
				transfer.previousSetPendingPDA = &previous
			}
		}
		out[live] = transfer
	}
	return out
}

// solanaPendingPDAAtSet derives the pending account of pe under one guardian set index.
//
// SECURITY: precondition: pe.solanaFields is set.
func solanaPendingPDAAtSet(b *solanaBackend, pe *pendingEntry, guardianSetIndex uint32) (solana.PublicKey, error) {
	f := pe.solanaFields
	return derivePendingObservationsPDA(b.program, f.Chain, f.Emitter, f.Sequence, guardianSetIndex, f.contentDigest)
}

// auditSolanaOwnPendingTransfers reads the live-set pending account and the NoReplay bit
// of each own transfer, then resubmits, waits, or searches for the commit. It returns the
// live-set pending accounts it reconciled.
func (acct *Accountant) auditSolanaOwnPendingTransfers(ctx context.Context, b *solanaBackend, guardianIndex uint8, own map[solana.PublicKey]solanaOwnPendingTransfer) map[solana.PublicKey]struct{} {
	reconciled := make(map[solana.PublicKey]struct{}, len(own))
	if len(own) == 0 {
		return reconciled
	}

	addrs := make([]solana.PublicKey, 0, len(own))
	for addr := range own {
		addrs = append(addrs, addr)
	}

	accounts, err := b.conn.GetMultipleAccounts(ctx, addrs, solacctconn.CommitmentFinalized)
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
		state, err := classifySolanaPendingAccount(accounts[idx], pe.solanaFields.contentDigest, guardianIndex)
		if err != nil {
			solanaAuditErrors.Inc()
			acct.logger.Error("failed to check a solana pending account", zap.String("backend", b.tag), zap.String("msgId", pe.msgId), zap.Stringer("pendingPda", addr), zap.Error(err))
			continue
		}
		states[addr] = state
		classified = append(classified, addr)
	}

	accounted, err := acct.readSolanaNoreplayBits(ctx, b, classified, own)
	if err != nil {
		acct.logUnresolvedSolanaTransfers(b, classified, own, err)
		return reconciled
	}

	commitSearches := 0
	skippedCommitSearches := 0
	for _, addr := range classified {
		isAccounted, read := accounted[addr]
		if !read {
			continue
		}
		transfer := own[addr]
		state := states[addr]
		action, err := decideSolanaOwnTransferAction(state, isAccounted)
		if err != nil {
			solanaAuditErrors.Inc()
			acct.logger.Error("failed to decide the audit action for a transfer", zap.String("msgId", transfer.pe.msgId), zap.Error(err))
			continue
		}
		reconciled[addr] = struct{}{}

		switch action {
		case solanaOwnTransferAwaitQuorum:
			acct.logger.Debug("the solana accountant is still collecting signatures for a transfer", zap.String("backend", b.tag), zap.String("msgId", transfer.pe.msgId))
		case solanaOwnTransferResubmit:
			acct.resubmitToSolana(ctx, b, transfer.pe, state)
		case solanaOwnTransferSearchCommit:
			if commitSearches == maxSolanaCommitSearchesPerAudit {
				skippedCommitSearches++
				continue
			}
			commitSearches++
			acct.searchSolanaCommit(ctx, b, transfer, solanaCommitSearchPDAs(addr, state, transfer))
		}
	}

	if skippedCommitSearches > 0 {
		solanaAuditErrors.Inc()
		acct.logger.Error("more accounted transfers than one audit searches, the rest wait for the next audit", zap.String("backend", b.tag), zap.Int("skipped", skippedCommitSearches), zap.Int("limit", maxSolanaCommitSearchesPerAudit))
	}
	return reconciled
}

// readSolanaNoreplayBits reads the NoReplay bit of each transfer in addrs. A transfer
// whose bucket fails to derive or decode is left out of the result.
func (acct *Accountant) readSolanaNoreplayBits(ctx context.Context, b *solanaBackend, addrs []solana.PublicKey, own map[solana.PublicKey]solanaOwnPendingTransfer) (map[solana.PublicKey]bool, error) {
	accounted := make(map[solana.PublicKey]bool, len(addrs))
	if len(addrs) == 0 {
		return accounted, nil
	}

	// Transfers from one emitter share a bucket, so the query is deduped.
	bucketOf := make(map[solana.PublicKey]solana.PublicKey, len(addrs))
	buckets := make([]solana.PublicKey, 0, len(addrs))
	seen := make(map[solana.PublicKey]struct{}, len(addrs))
	for _, addr := range addrs {
		pe := own[addr].pe
		f := pe.solanaFields
		bucket, err := deriveNoreplayBucketPDA(b.noreplay, b.authority, f.Chain, f.Emitter, f.Sequence)
		if err != nil {
			solanaAuditErrors.Inc()
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
		return accounted, nil
	}

	results, err := b.conn.GetMultipleAccounts(ctx, buckets, solacctconn.CommitmentFinalized)
	if err != nil {
		return nil, err
	}
	if len(results) != len(buckets) {
		return nil, fmt.Errorf("want %d results, got %d", len(buckets), len(results))
	}
	bucketData := make(map[solana.PublicKey][]byte, len(buckets))
	for idx, bucket := range buckets {
		if results[idx] != nil {
			bucketData[bucket] = results[idx].Data
		}
	}

	for addr, bucket := range bucketOf {
		pe := own[addr].pe
		raw, exists := bucketData[bucket]
		if !exists {
			accounted[addr] = false
			continue
		}
		marked, err := noreplayBitSet(raw, pe.solanaFields.Sequence)
		if err != nil {
			solanaAuditErrors.Inc()
			acct.logger.Error("failed to read a noreplay bucket", zap.String("msgId", pe.msgId), zap.Stringer("bucket", bucket), zap.Error(err))
			continue
		}
		accounted[addr] = marked
	}
	return accounted, nil
}

// resubmitToSolana queues a fresh observation for the Solana backend only.
func (acct *Accountant) resubmitToSolana(ctx context.Context, b *solanaBackend, pe *pendingEntry, live solanaPendingAccountState) {
	reason := "the pending account lacks this guardian's signature"
	if live == solanaPendingAccountAbsent {
		reason = "the pending account and the noreplay bit are absent"
	}
	if acct.submitObservation(ctx, pe, backendSolana, true) {
		solanaAuditErrors.Inc()
		acct.logger.Error("the solana accountant has not recorded this guardian's observation, resubmitting", zap.String("backend", b.tag), zap.String("msgId", pe.msgId), zap.String("reason", reason))
		return
	}
	acct.logger.Info("the solana accountant has not recorded this guardian's observation but it is already pending submission, skipping", zap.String("backend", b.tag), zap.String("msgId", pe.msgId), zap.String("reason", reason))
}

// searchSolanaCommit finds the commit log of an accounted transfer in the transactions of
// its pending accounts and applies it. The pending account is in few transactions; the
// shared NoReplay bucket is in thousands.
func (acct *Accountant) searchSolanaCommit(ctx context.Context, b *solanaBackend, transfer solanaOwnPendingTransfer, pdas []solana.PublicKey) {
	f := transfer.pe.solanaFields
	for _, pda := range pdas {
		found := acct.visitSolanaTransactions(ctx, b, pda, maxSolanaCommitSearchSignatures, func(sig solana.Signature, tx *solacctconn.TransactionResult) bool {
			commits, err := parseSolanaCommitLogs(tx.LogMessages, b.program)
			if err != nil {
				solanaMalformedLogs.Inc()
				acct.logger.Error("failed to parse transaction logs while searching for a commit", zap.String("msgId", transfer.pe.msgId), zap.Stringer("signature", sig), zap.Error(err))
			}
			for idx := range commits {
				if commits[idx].Chain != f.Chain || commits[idx].Emitter != f.Emitter || commits[idx].Sequence != f.Sequence {
					continue
				}
				acct.processSolanaCommitEvent(&commits[idx], sig, b.tag)
				return true
			}
			return false
		})
		if found {
			return
		}
	}

	solanaAuditErrors.Inc()
	acct.logger.Error("a transfer is marked accounted but its commit digest is unresolved, will retry next audit", zap.String("msgId", transfer.pe.msgId), zap.Int("searchedPendingAccounts", len(pdas)))
}

// visitSolanaTransactions calls visit on the successful transactions that mention addr,
// newest first, until visit returns true. It returns whether visit did.
func (acct *Accountant) visitSolanaTransactions(ctx context.Context, b *solanaBackend, addr solana.PublicKey, limit int, visit func(solana.Signature, *solacctconn.TransactionResult) bool) bool {
	sigs, err := b.conn.GetSignaturesForAddress(ctx, addr, limit)
	if err != nil {
		solanaAuditErrors.Inc()
		acct.logger.Error("failed to read the signatures of a pending account", zap.Stringer("pendingPda", addr), zap.Error(err))
		return false
	}

	for _, sig := range sigs {
		tx, err := b.conn.GetTransaction(ctx, sig)
		if err != nil {
			solanaAuditErrors.Inc()
			acct.logger.Error("failed to read a transaction of a pending account", zap.Stringer("pendingPda", addr), zap.Stringer("signature", sig), zap.Error(err))
			continue
		}
		if tx == nil || tx.Failed {
			continue
		}
		if visit(sig, tx) {
			return true
		}
	}
	return false
}

// auditSolanaProgramPendingAccounts reads the program's pending accounts and acts on each
// one whose bitmap bit for this guardian is clear: an own transfer the own-transfer pass
// did not reconcile is resubmitted, an unknown one is reobserved.
func (acct *Accountant) auditSolanaProgramPendingAccounts(ctx context.Context, b *solanaBackend, guardianSetIndex uint32, guardianIndex uint8, own map[solana.PublicKey]solanaOwnPendingTransfer, reconciled map[solana.PublicKey]struct{}) {
	accounts, err := b.conn.GetProgramAccountsByTag(ctx, b.program, pendingObservationsTag, pendingObservationsLen)
	if err != nil {
		solanaAuditErrors.Inc()
		acct.logger.Error("failed to read the solana pending accounts", zap.String("backend", b.tag), zap.Error(err))
		return
	}
	if len(accounts) > maxSolanaProgramPendingAccountsPerAudit {
		solanaAuditErrors.Inc()
		acct.logger.Error("the solana accountant reported more pending accounts than one audit reads", zap.String("backend", b.tag), zap.Int("total", len(accounts)), zap.Int("limit", maxSolanaProgramPendingAccountsPerAudit))
		accounts = accounts[:maxSolanaProgramPendingAccountsPerAudit]
	}

	// During the 24-hour grace period an own transfer can also have a previous-set pending
	// account. The guardian resubmits at the live set instead.
	previousSetPDAs := make(map[solana.PublicKey]struct{}, len(own))
	for _, transfer := range own {
		if transfer.previousSetPendingPDA != nil {
			previousSetPDAs[*transfer.previousSetPendingPDA] = struct{}{}
		}
	}

	reobservationSearches := 0
	skippedReobservationSearches := 0
	for idx := range accounts {
		account := &accounts[idx]
		obs, err := parsePendingObservationsAccount(account.Data)
		if err != nil {
			solanaAuditErrors.Inc()
			acct.logger.Error("failed to check a solana pending account", zap.String("backend", b.tag), zap.Stringer("pendingPda", account.Address), zap.Error(err))
			continue
		}
		signed, err := obs.hasSignature(guardianIndex)
		if err != nil {
			solanaAuditErrors.Inc()
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
				solanaAuditErrors.Inc()
				acct.logger.Error("a live-set pending account reports another guardian set index", zap.String("msgId", transfer.pe.msgId), zap.Uint32("accountSetIndex", obs.GuardianSetIndex), zap.Uint32("liveSetIndex", guardianSetIndex))
				continue
			}
			acct.resubmitToSolana(ctx, b, transfer.pe, solanaPendingAccountLacksOwnSignature)
			continue
		}

		if _, isPreviousSet := previousSetPDAs[account.Address]; isPreviousSet {
			acct.logger.Debug("skipping the previous-set pending account of an own transfer", zap.Stringer("pendingPda", account.Address))
			continue
		}

		if reobservationSearches == maxSolanaReobservationSearchesPerAudit {
			skippedReobservationSearches++
			continue
		}
		reobservationSearches++
		acct.reobserveUnknownSolanaPendingAccount(ctx, b, account.Address)
	}

	if skippedReobservationSearches > 0 {
		solanaAuditErrors.Inc()
		acct.logger.Error("the solana accountant has more unknown pending accounts than one audit searches, the rest wait for the next audit", zap.String("backend", b.tag), zap.Int("skipped", skippedReobservationSearches), zap.Int("limit", maxSolanaReobservationSearchesPerAudit))
	}
}

// reobserveUnknownSolanaPendingAccount finds the submit_observations instruction that
// created or signed pda and asks the local watcher to reobserve its source transaction.
//
// SECURITY: the recovered chain and transaction id only drive a reobservation request,
// which re-verifies the transaction on chain before it enters the signing pipeline. An
// instruction counts only when its fields derive pda, since one transaction can carry
// observations of several transfers.
func (acct *Accountant) reobserveUnknownSolanaPendingAccount(ctx context.Context, b *solanaBackend, pda solana.PublicKey) {
	found := acct.visitSolanaTransactions(ctx, b, pda, maxSolanaReobservationSearchSignatures, func(sig solana.Signature, tx *solacctconn.TransactionResult) bool {
		for _, ix := range tx.Instructions {
			if ix.ProgramID != b.program {
				continue
			}
			if len(ix.Data) == 0 || ix.Data[0] != submitObservationsDiscriminator {
				continue
			}

			parsed, err := parseSubmitObservationsIxData(ix.Data)
			if err != nil {
				solanaAuditErrors.Inc()
				acct.logger.Error("failed to decode a submit_observations instruction while searching for a reobservation", zap.Stringer("signature", sig), zap.Error(err))
				continue
			}
			derived, err := derivePendingObservationsPDA(b.program, parsed.Chain, parsed.Emitter, parsed.Sequence, parsed.GuardianSetIndex, parsed.contentDigest)
			if err != nil {
				solanaAuditErrors.Inc()
				acct.logger.Error("failed to derive the pending account of a submit_observations instruction", zap.Stringer("signature", sig), zap.Error(err))
				continue
			}
			if derived != pda {
				continue
			}

			acct.handleMissingObservation(MissingObservation{ChainId: uint16(parsed.Chain), TxHash: parsed.TxID.Bytes()})
			return true
		}
		return false
	})
	if found {
		return
	}

	solanaAuditErrors.Inc()
	acct.logger.Error("failed to find the source transaction of an unknown pending account", zap.Stringer("pendingPda", pda))
}

// logUnresolvedSolanaTransfers logs a failed query and every transfer in addrs whose
// status it leaves unknown. The program pending account pass still runs.
func (acct *Accountant) logUnresolvedSolanaTransfers(b *solanaBackend, addrs []solana.PublicKey, own map[solana.PublicKey]solanaOwnPendingTransfer, err error) {
	solanaAuditErrors.Inc()
	acct.logger.Error("unable to audit the own solana transfers, a query failed", zap.String("backend", b.tag), zap.Error(err))
	for _, addr := range addrs {
		if transfer, exists := own[addr]; exists {
			acct.logger.Error("unsure of the status of a pending transfer due to a query error", zap.String("msgId", transfer.pe.msgId))
		}
	}
}
