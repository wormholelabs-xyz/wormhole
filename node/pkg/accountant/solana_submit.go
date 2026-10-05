// Submission worker for the svm/accountant program. It signs each pending observation
// with the guardian key. It builds one submit_observations transaction per observation.
// The configured Solana keypair pays the fee. The worker sends each transaction and
// confirms it.

package accountant

import (
	"context"
	"errors"
	"fmt"
	"math"
	"time"

	"github.com/certusone/wormhole/node/pkg/common"
	"github.com/certusone/wormhole/node/pkg/solacctconn"
	ethCommon "github.com/ethereum/go-ethereum/common"
	"github.com/gagliardetto/solana-go"
	computebudget "github.com/gagliardetto/solana-go/programs/compute-budget"
	"github.com/wormhole-foundation/wormhole/sdk/vaa"

	"go.uber.org/zap"
)

const (
	// Twice MAX_QUORUM_BRANCH_CU, mollusk submit_observations.rs.
	solanaSubmitComputeUnitLimit = 150_000

	// Round two covers the recorded-payer race and a stale blockhash. A preflight
	// PayerMismatch carries the recorded payer, which round two uses directly.
	maxSolanaSubmitRounds = 2

	// Status polls before the audit takes over a round.
	maxSolanaConfirmPolls = 300

	// submit_observations.rs.
	submitObservationsAccountCount = 11
)

// solanaTxDisposition is what the worker does with one transaction result.
type solanaTxDisposition uint8

const (
	solanaTxFailed solanaTxDisposition = iota
	solanaTxAlreadyDone
	solanaTxRetryNextRound
	solanaTxFeePayerCannotPay
)

// solanaGuardianIdentity is the guardian set and index one batch submits under.
type solanaGuardianIdentity struct {
	guardianSetIndex uint32
	guardianIndex    uint8
	guardianSetPDA   solana.PublicKey // Core Bridge GuardianSet at guardianSetIndex
}

// solanaSubmission is one observation in flight.
type solanaSubmission struct {
	msgId  string
	msg    *common.MessagePublication
	fields *solanaObservationFields
	txID   solanaTxID

	pendingPDA           solana.PublicKey
	noreplayBucket       solana.PublicKey
	sourceBalance        solana.PublicKey
	destBalance          solana.PublicKey
	chainRegistrationPDA solana.PublicKey

	// The payer recorded in the pending PDA, or the fee payer when the PDA is absent. The
	// program checks it on the quorum-closing call.
	rentRecipient solana.PublicKey
	// The recorded payer from the ACCPAYR entry of a PayerMismatch preflight. The next round
	// uses it in place of a pending PDA read, then clears it.
	loggedPayer *solana.PublicKey

	// Set on the first build and reused by the retry round.
	guardianSignature []byte

	txSignature          solana.Signature
	lastValidBlockHeight uint64
}

// solanaBaseWorker is the entry point for the Token Bridge Solana submission worker.
func (acct *Accountant) solanaBaseWorker(ctx context.Context) error {
	if acct.solana == nil {
		return errors.New("acctsolworker: the solana backend is not configured")
	}
	return acct.solanaWorker(ctx, acct.solana)
}

// solanaWorker drains the backend's submission channel until the context ends.
func (acct *Accountant) solanaWorker(ctx context.Context, b *solanaBackend) error {
	for {
		select {
		case <-ctx.Done():
			return nil
		default:
			if err := acct.handleSolanaBatch(ctx, b); err != nil && ctx.Err() == nil {
				return err
			}
		}
	}
}

// solanaObservationSigningDigest is keccak256(prefix ‖ tx_id ‖ fields), matching
// operational-core hash.rs observation_signing_digest.
func solanaObservationSigningDigest(prefix []byte, txID solanaTxID, fields *solanaObservationFields) (ethCommon.Hash, error) {
	if !txID.valid() {
		return ethCommon.Hash{}, errors.New("observation signing digest: no tx id")
	}
	if fields == nil {
		return ethCommon.Hash{}, errors.New("observation signing digest: no observation fields")
	}
	packed, err := fields.pack()
	if err != nil {
		return ethCommon.Hash{}, fmt.Errorf("observation signing digest: %w", err)
	}
	return vaa.MessageSigningDigest(prefix, append(txID.Bytes(), packed...))
}

// encodeSubmitObservationsIxData is the inverse of parseSubmitObservationsIxData.
//
// SECURITY: preconditions are a submitSignatureLen signature, a constructed tx id and a
// non-nil fields record.
func encodeSubmitObservationsIxData(guardianSetIndex uint32, guardianIndex uint8, signature []byte, txID solanaTxID, fields *solanaObservationFields) ([]byte, error) {
	if len(signature) != submitSignatureLen {
		return nil, fmt.Errorf("submit_observations instruction data: signature is %d bytes, want %d", len(signature), submitSignatureLen)
	}
	if !txID.valid() {
		return nil, errors.New("submit_observations instruction data: no tx id")
	}
	if fields == nil {
		return nil, errors.New("submit_observations instruction data: no observation fields")
	}

	return encodeWire(&submitObservationsInstructionWire{
		Discriminator: submitObservationsDiscriminator,
		Data: submitObservationsIxDataWire{
			GuardianSetIndex: guardianSetIndex,
			GuardianIndex:    guardianIndex,
			Signature:        [submitSignatureLen]byte(signature),
			TxIDLen:          txID.length,
			TxID:             txID.padded,
			Fields:           fields.wire(),
		},
	})
}

// deriveSolanaSubmission resolves every account one observation touches.
func (b *solanaBackend) deriveSolanaSubmission(guardianSetIndex uint32, msg *common.MessagePublication, fields *solanaObservationFields) (*solanaSubmission, error) {
	if fields == nil {
		return nil, errors.New("solana submission: no observation fields")
	}
	// SECURITY: the cached content digest seeds the pending PDA. A stale digest derives the
	// address of a different observation.
	digest, err := fields.computeContentDigest()
	if err != nil {
		return nil, fmt.Errorf("solana submission: %w", err)
	}
	if fields.contentDigest != digest {
		return nil, errors.New("solana submission: the content digest does not match the fields")
	}

	// The program accepts only a 32-byte or 64-byte tx id.
	txID, err := newSolanaTxID(msg.TxID)
	if err != nil {
		return nil, fmt.Errorf("solana submission: %w", err)
	}

	sub := &solanaSubmission{
		msgId:  msg.MessageIDString(),
		msg:    msg,
		fields: fields,
		txID:   txID,
	}

	if sub.pendingPDA, err = derivePendingObservationsPDA(b.program, fields.Chain, fields.Emitter, fields.Sequence, guardianSetIndex, fields.contentDigest); err != nil {
		return nil, err
	}
	if sub.noreplayBucket, err = deriveNoreplayBucketPDA(b.noreplay, b.authority, fields.Chain, fields.Emitter, fields.Sequence); err != nil {
		return nil, err
	}
	if sub.chainRegistrationPDA, err = deriveChainRegistrationPDA(b.program, fields.Chain); err != nil {
		return nil, err
	}

	// The program reads the balance slots only for a transfer. For other actions, the
	// on-chain tests fill them with the authority PDA.
	if !vaa.IsTransfer([]byte{fields.Action}) {
		sub.sourceBalance = b.authority
		sub.destBalance = b.authority
		return sub, nil
	}

	if sub.sourceBalance, err = deriveBalanceAccountPDA(b.program, fields.Chain, fields.TokenChain, fields.TokenAddress); err != nil {
		return nil, err
	}
	if sub.destBalance, err = deriveBalanceAccountPDA(b.program, fields.RecipientChain, fields.TokenChain, fields.TokenAddress); err != nil {
		return nil, err
	}
	return sub, nil
}

// readBatch reads up to submitObservationBatchSize messages within batchTimeout and drops
// those no longer pending.
func (acct *Accountant) readBatch(ctx context.Context, subChan chan *common.MessagePublication, tag string) ([]*common.MessagePublication, error) {
	readCtx, cancel := context.WithTimeout(ctx, batchTimeout)
	defer cancel()

	msgs, err := common.ReadFromChannelWithTimeout[*common.MessagePublication](readCtx, subChan, acct.submitObservationBatchSize)
	if err != nil && !errors.Is(err, context.DeadlineExceeded) {
		return nil, fmt.Errorf("failed to read messages from channel for %s: %w", tag, err)
	}
	if len(msgs) == 0 {
		return nil, nil
	}
	return acct.removeCompleted(msgs), nil
}

// guardianIndex returns the live guardian set and this guardian's index in it. Callers
// bound the index for their encoding.
func (acct *Accountant) guardianIndex() (*common.GuardianSet, int, error) {
	gs := acct.gst.Get()
	if gs == nil {
		return nil, 0, errors.New("failed to get the guardian set")
	}
	index, found := gs.KeyIndex(acct.guardianAddr)
	if !found {
		return nil, 0, errors.New("this guardian is not in the current guardian set")
	}
	if index < 0 {
		return nil, 0, fmt.Errorf("negative guardian index %d", index)
	}
	return gs, index, nil
}

// handleSolanaBatch reads one batch from the backend channel and submits each observation
// as its own transaction.
func (acct *Accountant) handleSolanaBatch(ctx context.Context, b *solanaBackend) error {
	msgs, err := acct.readBatch(ctx, b.subChan, b.tag)
	if err != nil {
		return err
	}
	if len(msgs) == 0 {
		return nil
	}

	// Every exit below leaves the batch retryable by the audit.
	defer acct.clearSubmitPendingFlags(msgs, backendSolana)

	// confirmSolanaSubmissions reads the status of every transaction of the batch in one call.
	if len(msgs) > solacctconn.MaxStatusesPerCall {
		solanaSubmitFailures.Add(float64(len(msgs) - solacctconn.MaxStatusesPerCall))
		acct.logger.Warn("the solana batch is past the status read limit, the audit will retry the rest",
			zap.String("backend", b.tag),
			zap.Int("numMsgs", len(msgs)),
			zap.Int("limit", solacctconn.MaxStatusesPerCall),
		)
		msgs = msgs[:solacctconn.MaxStatusesPerCall]
	}

	gs, index, err := acct.guardianIndex()
	if err != nil {
		acct.failSolanaBatch(b, msgs, err)
		return nil
	}
	// SECURITY: the index is inside the 128-bit signature bitmap.
	if index >= pendingObservationsMaxGuardians {
		acct.failSolanaBatch(b, msgs, fmt.Errorf("guardian index %d is outside the %d-bit bitmap", index, pendingObservationsMaxGuardians))
		return nil
	}
	guardianSetPDA, err := deriveGuardianSetPDA(b.coreBridge, gs.Index)
	if err != nil {
		acct.failSolanaBatch(b, msgs, err)
		return nil
	}
	guardian := solanaGuardianIdentity{
		guardianSetIndex: gs.Index,
		guardianIndex:    uint8(index), // #nosec G115 -- bounded above
		guardianSetPDA:   guardianSetPDA,
	}

	work := acct.deriveSolanaSubmissions(b, msgs, gs.Index)
	for round := 0; round < maxSolanaSubmitRounds && len(work) != 0; round++ {
		work = acct.submitSolanaRound(ctx, b, guardian, work)
	}

	for _, sub := range work {
		solanaSubmitFailures.Inc()
		acct.logger.Error("gave up submitting an observation to the solana accountant, the audit will retry", zap.String("backend", b.tag), zap.String("msgId", sub.msgId))
	}
	return nil
}

// failSolanaBatch counts and logs a failure that stops the whole batch.
func (acct *Accountant) failSolanaBatch(b *solanaBackend, msgs []*common.MessagePublication, err error) {
	solanaSubmitFailures.Add(float64(len(msgs)))
	acct.logger.Error("failed to submit a batch to the solana accountant, the audit will retry", zap.String("backend", b.tag), zap.Int("numMsgs", len(msgs)), zap.Error(err))
}

// deriveSolanaSubmissions resolves each message to its accounts, dropping transfers the
// watcher already released. It grabs the pending transfer lock.
func (acct *Accountant) deriveSolanaSubmissions(b *solanaBackend, msgs []*common.MessagePublication, guardianSetIndex uint32) []*solanaSubmission {
	type pendingObservation struct {
		msg    *common.MessagePublication
		fields *solanaObservationFields
	}

	pending := make([]pendingObservation, 0, len(msgs))
	acct.pendingTransfersLock.Lock()
	for _, msg := range msgs {
		pe, exists := acct.pendingTransfers[msg.MessageIDString()]
		if !exists {
			acct.logger.Debug("skipping a solana observation, the transfer is no longer pending", zap.String("backend", b.tag), zap.String("msgId", msg.MessageIDString()))
			continue
		}
		pending = append(pending, pendingObservation{msg: msg, fields: pe.solanaFields})
	}
	acct.pendingTransfersLock.Unlock()

	out := make([]*solanaSubmission, 0, len(pending))
	for _, obs := range pending {
		sub, err := b.deriveSolanaSubmission(guardianSetIndex, obs.msg, obs.fields)
		if err != nil {
			solanaSubmitFailures.Inc()
			acct.logger.Error("failed to derive the accounts for a solana observation", zap.String("backend", b.tag), zap.String("msgId", obs.msg.MessageIDString()), zap.Error(err))
			continue
		}
		out = append(out, sub)
	}
	return out
}

// submitSolanaRound sends and confirms one transaction per observation. It returns the
// observations that can still land after a fresh read. These are the recorded-payer race
// and a stale blockhash.
func (acct *Accountant) submitSolanaRound(ctx context.Context, b *solanaBackend, guardian solanaGuardianIdentity, work []*solanaSubmission) []*solanaSubmission {
	ready := acct.resolveSolanaRentRecipients(ctx, b, guardian.guardianIndex, work)
	if len(ready) == 0 {
		return nil
	}

	blockhash, err := b.conn.GetLatestBlockhash(ctx)
	if err != nil {
		solanaSubmitFailures.Add(float64(len(ready)))
		acct.logger.Error("failed to read a solana blockhash", zap.String("backend", b.tag), zap.Int("numMsgs", len(ready)), zap.Error(err))
		return nil
	}

	sent := make([]*solanaSubmission, 0, len(ready))
	retry := make([]*solanaSubmission, 0, len(ready))
	for _, sub := range ready {
		tx, err := acct.buildSolanaSubmitTx(ctx, b, guardian, sub, blockhash.Hash)
		if err != nil {
			solanaSubmitFailures.Inc()
			acct.logger.Error("failed to build a solana observation transaction", zap.String("backend", b.tag), zap.String("msgId", sub.msgId), zap.Error(err))
			continue
		}

		sig, err := b.conn.SendTransaction(ctx, tx)
		if err != nil {
			if acct.handleSolanaTxError(b, sub, err, "send") == solanaTxRetryNextRound {
				acct.recordSolanaLoggedPayer(b, sub, err)
				retry = append(retry, sub)
			}
			continue
		}

		sub.txSignature = sig
		sub.lastValidBlockHeight = blockhash.LastValidBlockHeight
		sent = append(sent, sub)
	}

	return append(retry, acct.confirmSolanaSubmissions(ctx, b, sent)...)
}

// recordSolanaLoggedPayer keeps the recorded payer from the ACCPAYR entry of a preflight
// PayerMismatch. A parse failure leaves the next round to read the pending PDA.
func (acct *Accountant) recordSolanaLoggedPayer(b *solanaBackend, sub *solanaSubmission, err error) {
	var txErr *solacctconn.TxError
	if !errors.As(err, &txErr) || !txErr.HasCustomCode || txErr.CustomCode != solanaErrPayerMismatch || txErr.Logs == nil {
		return
	}
	payer, parseErr := parseSolanaPayerLog(txErr.Logs, b.program, sub.pendingPDA)
	if parseErr != nil {
		acct.logger.Warn("failed to read the recorded payer from a solana preflight, the next round reads the pending account", zap.String("backend", b.tag), zap.String("msgId", sub.msgId), zap.Error(parseErr))
		return
	}
	sub.loggedPayer = &payer
}

// resolveSolanaRentRecipients reads each pending PDA and returns the observations still
// worth sending. If the PDA is absent, the fee payer is the rent recipient. If the PDA is
// present, it records its own payer. If this guardian's bit is set, the work is done.
// An observation with a logged payer takes it directly and joins the result unread.
//
// The read uses confirmed commitment, the same as preflight. Thus a PDA that another
// guardian created in the previous round is visible before it finalizes.
func (acct *Accountant) resolveSolanaRentRecipients(ctx context.Context, b *solanaBackend, guardianIndex uint8, work []*solanaSubmission) []*solanaSubmission {
	if len(work) == 0 {
		return nil
	}

	ready := make([]*solanaSubmission, 0, len(work))
	unread := make([]*solanaSubmission, 0, len(work))
	for _, sub := range work {
		// SECURITY: the PayerMismatch rolled back this guardian's bit, so the signed check
		// below cannot apply. A parallel quorum gives AlreadyAccounted, a done result.
		if sub.loggedPayer != nil {
			sub.rentRecipient = *sub.loggedPayer
			sub.loggedPayer = nil
			ready = append(ready, sub)
			continue
		}
		unread = append(unread, sub)
	}
	if len(unread) == 0 {
		return ready
	}

	addrs := make([]solana.PublicKey, len(unread))
	for idx, sub := range unread {
		addrs[idx] = sub.pendingPDA
	}

	accounts, err := b.conn.GetOwnedAccounts(ctx, addrs, b.program, solacctconn.CommitmentConfirmed)
	if err != nil {
		solanaSubmitFailures.Add(float64(len(unread)))
		acct.logger.Error("failed to read the solana pending accounts", zap.String("backend", b.tag), zap.Int("numMsgs", len(unread)), zap.Error(err))
		return ready
	}
	if len(accounts) != len(unread) {
		solanaSubmitFailures.Add(float64(len(unread)))
		acct.logger.Error("the solana pending account read returned the wrong number of results", zap.String("backend", b.tag), zap.Int("want", len(unread)), zap.Int("got", len(accounts)))
		return ready
	}

	feePayer := b.feePayer.PublicKey()
	for idx, sub := range unread {
		account := accounts[idx]
		switch account.State {
		case solacctconn.AccountAbsent:
			sub.rentRecipient = feePayer
			ready = append(ready, sub)
			continue
		case solacctconn.AccountUninitialised:
			// SECURITY: create_pending_pda records the submitter as payer over a prefund.
			acct.logger.Warn("a solana pending account is prefunded, creating it", zap.String("backend", b.tag), zap.String("msgId", sub.msgId), zap.Stringer("pendingPda", sub.pendingPDA))
			sub.rentRecipient = feePayer
			ready = append(ready, sub)
			continue
		case solacctconn.AccountInitialised:
		default:
			solanaSubmitFailures.Inc()
			acct.logger.Error("a solana pending account read returned an unknown state", zap.String("backend", b.tag), zap.String("msgId", sub.msgId), zap.Uint8("state", uint8(account.State)))
			continue
		}

		obs, signed, err := checkPendingObservationsAccount(account.Data, sub.fields.contentDigest, guardianIndex)
		if err != nil {
			solanaSubmitFailures.Inc()
			acct.logger.Error("failed to check a solana pending account", zap.String("backend", b.tag), zap.String("msgId", sub.msgId), zap.Stringer("pendingPda", sub.pendingPDA), zap.Error(err))
			continue
		}
		if signed {
			acct.logger.Debug("the solana accountant already holds this guardian's observation", zap.String("backend", b.tag), zap.String("msgId", sub.msgId))
			continue
		}

		sub.rentRecipient = obs.Payer
		ready = append(ready, sub)
	}
	return ready
}

// buildSolanaSubmitTx builds the legacy transaction that carries the observation. It signs
// the observation on first use. The runtime requires the compute budget instructions first.
func (acct *Accountant) buildSolanaSubmitTx(ctx context.Context, b *solanaBackend, guardian solanaGuardianIdentity, sub *solanaSubmission, blockhash solana.Hash) (*solana.Transaction, error) {
	if sub.guardianSignature == nil {
		digest, err := solanaObservationSigningDigest(b.prefix, sub.txID, sub.fields)
		if err != nil {
			return nil, err
		}
		signature, err := acct.guardianSigner.Sign(ctx, digest.Bytes())
		if err != nil {
			return nil, fmt.Errorf("failed to sign the observation: %w", err)
		}
		sub.guardianSignature = signature
	}
	data, err := encodeSubmitObservationsIxData(guardian.guardianSetIndex, guardian.guardianIndex, sub.guardianSignature, sub.txID, sub.fields)
	if err != nil {
		return nil, err
	}

	// Order and writability match the account list in submit_observations.rs.
	feePayer := b.feePayer.PublicKey()
	accounts := [submitObservationsAccountCount]*solana.AccountMeta{
		solana.NewAccountMeta(feePayer, true, true),
		solana.NewAccountMeta(sub.pendingPDA, true, false),
		solana.NewAccountMeta(guardian.guardianSetPDA, false, false),
		solana.NewAccountMeta(sub.noreplayBucket, true, false),
		solana.NewAccountMeta(solana.SystemProgramID, false, false),
		solana.NewAccountMeta(b.noreplay, false, false),
		solana.NewAccountMeta(b.authority, false, false),
		solana.NewAccountMeta(sub.sourceBalance, true, false),
		solana.NewAccountMeta(sub.destBalance, true, false),
		solana.NewAccountMeta(sub.rentRecipient, true, false),
		solana.NewAccountMeta(sub.chainRegistrationPDA, false, false),
	}

	instructions := make([]solana.Instruction, 0, 3)
	instructions = append(instructions, computebudget.NewSetComputeUnitLimitInstruction(solanaSubmitComputeUnitLimit).Build())
	if b.priorityFee > 0 {
		instructions = append(instructions, computebudget.NewSetComputeUnitPriceInstruction(b.priorityFee).Build())
	}
	instructions = append(instructions, solana.NewInstruction(b.program, accounts[:], data))

	tx, err := solana.NewTransaction(instructions, blockhash, solana.TransactionPayer(feePayer))
	if err != nil {
		return nil, fmt.Errorf("failed to build the transaction: %w", err)
	}
	if _, err := tx.Sign(func(key solana.PublicKey) *solana.PrivateKey {
		if key.Equals(feePayer) {
			return &b.feePayer
		}
		return nil
	}); err != nil {
		return nil, fmt.Errorf("failed to sign the transaction: %w", err)
	}
	return tx, nil
}

// confirmSolanaSubmissions polls signature statuses until every transaction resolves, the
// blockhash of the oldest expires, or the poll budget runs out. It returns the submissions
// to retry next round.
func (acct *Accountant) confirmSolanaSubmissions(ctx context.Context, b *solanaBackend, sent []*solanaSubmission) []*solanaSubmission {
	if len(sent) == 0 {
		return nil
	}

	outstanding := make(map[solana.Signature]*solanaSubmission, len(sent))
	minLastValidBlockHeight := uint64(math.MaxUint64)
	for _, sub := range sent {
		outstanding[sub.txSignature] = sub
		if sub.lastValidBlockHeight < minLastValidBlockHeight {
			minLastValidBlockHeight = sub.lastValidBlockHeight
		}
	}

	retry := make([]*solanaSubmission, 0, len(sent))
	for poll := 0; poll < maxSolanaConfirmPolls && len(outstanding) != 0; poll++ {
		select {
		case <-ctx.Done():
			acct.logger.Warn("stopped confirming solana transactions, the context ended", zap.String("backend", b.tag), zap.Int("outstanding", len(outstanding)))
			return retry
		default:
		}

		// The loop reads the height before the statuses. Thus a signature that is still unknown
		// after a height past its expiry cannot land.
		height, heightErr := b.conn.GetBlockHeight(ctx)
		if heightErr != nil {
			acct.logger.Warn("failed to read the solana block height", zap.String("backend", b.tag), zap.Error(heightErr))
		}

		sigs := make([]solana.Signature, 0, len(outstanding))
		for sig := range outstanding {
			sigs = append(sigs, sig)
		}

		statuses, err := b.conn.GetSignatureStatuses(ctx, sigs)
		if err != nil {
			solanaSubmitFailures.Add(float64(len(outstanding)))
			acct.logger.Error("failed to read solana signature statuses", zap.String("backend", b.tag), zap.Int("outstanding", len(outstanding)), zap.Error(err))
			return retry
		}
		if len(statuses) != len(sigs) {
			solanaSubmitFailures.Add(float64(len(outstanding)))
			acct.logger.Error("the solana signature status read returned the wrong number of results", zap.String("backend", b.tag), zap.Int("want", len(sigs)), zap.Int("got", len(statuses)))
			return retry
		}

		unseen := make([]solana.Signature, 0, len(sigs))
		for idx, status := range statuses {
			sig := sigs[idx]
			sub := outstanding[sig]
			if status == nil {
				unseen = append(unseen, sig)
				continue
			}
			if status.Err != nil {
				delete(outstanding, sig)
				if acct.handleSolanaTxError(b, sub, status.Err, "confirm") == solanaTxRetryNextRound {
					retry = append(retry, sub)
				}
				continue
			}
			if !status.Confirmed {
				continue
			}

			delete(outstanding, sig)
			solanaTransfersSubmitted.Inc()
			acct.logger.Info("submitted an observation to the solana accountant", zap.String("backend", b.tag), zap.String("msgId", sub.msgId), zap.Stringer("signature", sub.txSignature))
		}
		if len(outstanding) == 0 {
			break
		}

		// A processed transaction can still confirm, so the loop drops only unseen transactions.
		if heightErr == nil && height > minLastValidBlockHeight {
			for _, sig := range unseen {
				sub := outstanding[sig]
				solanaSubmitFailures.Inc()
				acct.logger.Error("a solana observation was dropped, the audit will retry",
					zap.String("backend", b.tag),
					zap.String("msgId", sub.msgId),
					zap.Stringer("signature", sub.txSignature),
					zap.Uint64("blockHeight", height),
					zap.Uint64("lastValidBlockHeight", sub.lastValidBlockHeight),
				)
				delete(outstanding, sig)
			}
			if len(outstanding) == 0 {
				break
			}
		}

		select {
		case <-ctx.Done():
			acct.logger.Warn("stopped confirming solana transactions, the context ended", zap.String("backend", b.tag), zap.Int("outstanding", len(outstanding)))
			return retry
		case <-time.After(solanaConfirmPollInterval):
		}
	}

	for _, sub := range outstanding {
		solanaSubmitFailures.Inc()
		acct.logger.Error("a solana observation did not confirm in time, the audit will retry", zap.String("backend", b.tag), zap.String("msgId", sub.msgId), zap.Stringer("signature", sub.txSignature))
	}
	return retry
}

// handleSolanaTxError counts and logs one transaction error and returns its disposition.
func (acct *Accountant) handleSolanaTxError(b *solanaBackend, sub *solanaSubmission, txErr error, phase string) solanaTxDisposition {
	disposition, reason := classifySolanaTxError(txErr)

	fields := []zap.Field{
		zap.String("backend", b.tag),
		zap.String("msgId", sub.msgId),
		zap.String("phase", phase),
		zap.String("reason", reason),
		zap.Error(txErr),
	}

	switch disposition {
	case solanaTxAlreadyDone:
		acct.logger.Info("a solana observation did not apply", fields...)
	case solanaTxRetryNextRound:
		acct.logger.Warn("retrying a solana observation", fields...)
	case solanaTxFeePayerCannotPay:
		solanaFeePayerErrors.Inc()
		acct.logger.Error("the solana fee payer cannot pay", append(fields, zap.Stringer("feePayer", b.feePayer.PublicKey()))...)
	case solanaTxFailed:
		solanaSubmitFailures.Inc()
		acct.logger.Error("a solana observation failed", fields...)
	}
	return disposition
}

// classifySolanaTxError maps a transaction error to its disposition and a reason for the
// log.
func classifySolanaTxError(err error) (solanaTxDisposition, string) {
	var txErr *solacctconn.TxError
	if !errors.As(err, &txErr) {
		return solanaTxFailed, "transaction error"
	}

	if txErr.HasCustomCode {
		switch txErr.CustomCode {
		case solanaErrAlreadySigned:
			return solanaTxAlreadyDone, "this guardian already signed"
		case solanaErrAlreadyAccounted:
			return solanaTxAlreadyDone, "the transfer reached quorum without this observation"
		case solanaErrPayerMismatch:
			return solanaTxRetryNextRound, "the recorded payer changed"
		case solanaErrExpiredGuardianSet:
			return solanaTxFailed, "the guardian set expired"
		case solanaErrInvalidGuardianIndex:
			return solanaTxFailed, "the guardian index is outside the set"
		case solanaErrUnregisteredEmitter:
			return solanaTxFailed, "the emitter is not registered"
		case solanaErrMissingChainRegistration:
			return solanaTxFailed, "the chain is not registered"
		case solanaErrInvalidSignature:
			return solanaTxFailed, "the program rejected the signature"
		}
		return solanaTxFailed, fmt.Sprintf("custom program error %d", txErr.CustomCode)
	}

	switch txErr.Kind {
	case solacctconn.TxErrBlockhashNotFound:
		return solanaTxRetryNextRound, "the blockhash expired"
	case solacctconn.TxErrAlreadyProcessed:
		return solanaTxAlreadyDone, "the transaction was already processed"
	case solacctconn.TxErrAccountNotFound:
		return solanaTxFeePayerCannotPay, "the fee payer account does not exist"
	case solacctconn.TxErrInsufficientFundsForFee:
		return solanaTxFeePayerCannotPay, "the fee payer cannot cover the fee"
	}
	return solanaTxFailed, "transaction error"
}
