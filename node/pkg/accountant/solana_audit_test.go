package accountant

import (
	"bytes"
	"context"
	"encoding/binary"
	"errors"
	"math"
	"slices"
	"testing"
	"time"

	"github.com/certusone/wormhole/node/pkg/common"
	gossipv1 "github.com/certusone/wormhole/node/pkg/proto/gossip/v1"
	"github.com/certusone/wormhole/node/pkg/solacctconn"
	"github.com/gagliardetto/solana-go"
	"github.com/prometheus/client_golang/prometheus/testutil"
	"github.com/stretchr/testify/assert"
	"github.com/stretchr/testify/require"
	"github.com/wormhole-foundation/wormhole/sdk/vaa"
)

// solanaNoreplayBucket builds a bucket account, with the bit for each sequence set.
func solanaNoreplayBucket(t *testing.T, marked ...uint64) []byte {
	t.Helper()
	var wire noreplayBucketWire
	for _, sequence := range marked {
		index, mask := noreplayBitLocation(sequence)
		wire.Bitmap[index] |= mask
	}
	data, err := encodeWire(&wire)
	require.NoError(t, err)
	return data
}

// solanaAuditFixture is one accountant with a single pending Solana transfer.
type solanaAuditFixture struct {
	acct    *Accountant
	conn    *MockAccountantSolanaConn
	obsvReq chan *gossipv1.ObservationRequest
	msgChan chan *common.MessagePublication
	msg     *common.MessagePublication
	pe      *pendingEntry

	pending solana.PublicKey
	bucket  solana.PublicKey
}

func newSolanaAuditFixture(t *testing.T, ctx context.Context) *solanaAuditFixture {
	t.Helper()

	obsvReq := make(chan *gossipv1.ObservationRequest, 2*maxReobservationRequestsPerAudit)
	acct, conn, msgChan := newSolanaTestAccountantWithObsvReq(t, ctx, solanaTestOpts{enforce: true}, obsvReq)
	conn.Balance = 1_000_000

	msg := solanaTestTransfer(t, 31)
	_, err := acct.SubmitObservation(msg)
	require.NoError(t, err)
	pe := acct.pendingTransfers[msg.MessageIDString()]
	require.NotNil(t, pe)
	require.NotNil(t, pe.solanaFields)

	b := acct.solana
	pending, err := solanaPendingPDAAtSet(b, pe.solanaFields, 0, mustSolanaTxIDBytes(t, msg.TxID))
	require.NoError(t, err)
	bucket, err := deriveNoreplayBucketPDA(b.noreplay, b.authority, pe.solanaFields.Chain, pe.solanaFields.Emitter, pe.solanaFields.Sequence)
	require.NoError(t, err)

	return &solanaAuditFixture{
		acct: acct, conn: conn, obsvReq: obsvReq, msgChan: msgChan,
		msg: msg, pe: pe, pending: pending, bucket: bucket,
	}
}

// pendingAccount is the current guardian-set pending account of the fixture transfer at set
// index 0.
func (f *solanaAuditFixture) pendingAccount(t *testing.T, signedBy []uint8) *solacctconn.OwnedAccount {
	t.Helper()
	return &solacctconn.OwnedAccount{
		State: solacctconn.AccountInitialised,
		Data:  solanaPendingAccountData(t, f.pe.solanaFields.Chain, 0, f.pe.solanaFields.contentDigest, f.acct.solana.feePayer.PublicKey(), signedBy),
	}
}

// markAccounted sets the NoReplay bit of the fixture transfer.
func (f *solanaAuditFixture) markAccounted(t *testing.T) {
	t.Helper()
	f.conn.SetAccount(f.bucket, &solacctconn.OwnedAccount{State: solacctconn.AccountInitialised, Data: solanaNoreplayBucket(t, f.pe.solanaFields.Sequence)})
}

// commitTransaction is a transaction whose logs carry one commit for the fixture transfer.
func (f *solanaAuditFixture) commitTransaction(digest [32]byte) *solacctconn.TransactionResult {
	commit := newSolanaCommitEvent(f.pe.solanaFields.Chain, f.pe.solanaFields.Emitter, f.pe.solanaFields.Sequence, digest, 0)
	return &solacctconn.TransactionResult{LogMessages: commitLogs(f.acct.solana.program, commit)}
}

// moveToGuardianSetOne makes set index 1 current and returns the fixture transfer's current
// guardian-set and previous-set pending accounts.
func (f *solanaAuditFixture) moveToGuardianSetOne(t *testing.T) (current solana.PublicKey, previous solana.PublicKey) {
	t.Helper()
	gs := f.acct.gst.Get()
	f.acct.gst.Set(&common.GuardianSet{Index: 1, Keys: gs.Keys})

	current, err := solanaPendingPDAAtSet(f.acct.solana, f.pe.solanaFields, 1, mustSolanaTxIDBytes(t, f.msg.TxID))
	require.NoError(t, err)
	require.NotEqual(t, current, f.pending)
	return current, f.pending
}

// solanaUnknownTransfer is a transfer outside the pending map with its pending account at
// set index 0.
func solanaUnknownTransfer(t *testing.T, program solana.PublicKey, chain vaa.ChainID, sequence uint64) (solanaObservationFields, solana.PublicKey) {
	t.Helper()
	fields := *fixtureTransferFields(t)
	fields.Chain = chain
	fields.Sequence = sequence
	require.NoError(t, fields.setContentDigest())
	pda, err := derivePendingObservationsPDA(program, fields.Chain, fields.Emitter, fields.Sequence, 0, fields.contentDigest, solanaTestTxID(t))
	require.NoError(t, err)
	return fields, pda
}

// solanaProgramAccountFor builds a getProgramAccounts result row with solanaTestTxID.
func solanaProgramAccountFor(t *testing.T, addr solana.PublicKey, chain vaa.ChainID, guardianSetIndex uint32, digest [32]byte, signedBy []uint8) solacctconn.ProgramAccount {
	t.Helper()
	return solanaProgramAccountForTxID(t, addr, chain, guardianSetIndex, digest, solanaTestTxID(t), signedBy)
}

func solanaProgramAccountForTxID(t *testing.T, addr solana.PublicKey, chain vaa.ChainID, guardianSetIndex uint32, digest [32]byte, txID solanaTxID, signedBy []uint8) solacctconn.ProgramAccount {
	t.Helper()
	return solacctconn.ProgramAccount{
		Address: addr,
		Data:    solanaPendingAccountDataWithTxID(t, chain, guardianSetIndex, digest, solana.PublicKey{0x01}, txID, signedBy),
	}
}

func TestPublishSolanaFeePayerBalance(t *testing.T) {
	ctx := context.Background()

	tests := []struct {
		name       string
		lamports   uint64
		err        error
		wantGauge  float64
		wantErrors float64
	}{
		{name: "funded", lamports: 2_000_000_000, wantGauge: 2_000_000_000},
		{name: "low", lamports: 5_000_000, wantGauge: 5_000_000},
		{name: "empty", lamports: 0, wantGauge: 0, wantErrors: 1},
		{name: "query error", err: errors.New("rpc down"), wantGauge: 7, wantErrors: 1},
	}

	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			acct, conn, _ := newSolanaTestAccountant(t, ctx, solanaTestOpts{enforce: true})
			conn.Balance, conn.BalanceErr = tt.lamports, tt.err
			solanaFeePayerLamports.WithLabelValues("wtt").Set(7)
			errorsBefore := testutil.ToFloat64(solanaFeePayerErrors.WithLabelValues("wtt"))

			acct.publishSolanaFeePayerBalance(ctx, acct.solana)
			require.Len(t, conn.GetBalanceCalls, 1)
			assert.Equal(t, acct.solana.feePayer.PublicKey(), conn.GetBalanceCalls[0])
			assert.Equal(t, tt.wantGauge, testutil.ToFloat64(solanaFeePayerLamports.WithLabelValues("wtt")))
			assert.Equal(t, tt.wantErrors, testutil.ToFloat64(solanaFeePayerErrors.WithLabelValues("wtt"))-errorsBefore)
		})
	}
}

// A transaction that never resolves must not pin the history search of its address.
func TestSolanaHistorySearchSkipsPersistentFetchFailure(t *testing.T) {
	ctx := context.Background()
	f := newSolanaAuditFixture(t, ctx)
	f.markAccounted(t)
	// {4} has no transaction, so every fetch fails. {5} is older and holds the commit.
	f.conn.SetSignaturesForAddress(f.pending, []solana.Signature{{4}, {5}})
	f.conn.SetTransaction(solana.Signature{5}, f.commitTransaction(f.pe.solanaFields.contentDigest))

	for audit := 1; audit < maxSolanaTransactionFetchFailures; audit++ {
		f.acct.runSolanaAudit(ctx, f.acct.solana)
		require.Empty(t, f.msgChan, "audit %d released before the failure limit", audit)
	}

	f.acct.runSolanaAudit(ctx, f.acct.solana)
	require.Len(t, f.msgChan, 1)
	assert.Equal(t, f.msg.MessageIDString(), (<-f.msgChan).MessageIDString())
}

func TestSolanaHistoryCursorsFetchFailures(t *testing.T) {
	addr, other := solana.PublicKey{1}, solana.PublicKey{2}
	sig, newer := solana.Signature{4}, solana.Signature{5}
	var c solanaHistoryCursors

	for i := 1; i < maxSolanaTransactionFetchFailures; i++ {
		require.False(t, c.recordFetchFailure(addr, sig), "failure %d", i)
	}
	assert.False(t, c.recordFetchFailure(other, sig), "counts are per address")
	assert.False(t, c.recordFetchFailure(addr, newer), "a new signature restarts the count")
	for i := 2; i < maxSolanaTransactionFetchFailures; i++ {
		require.False(t, c.recordFetchFailure(addr, newer), "failure %d", i)
	}
	assert.True(t, c.recordFetchFailure(addr, newer))
}

func TestSolanaFeePayerBalanceLevel(t *testing.T) {
	tests := []struct {
		lamports uint64
		want     solanaFeePayerLevel
	}{
		{lamports: 0, want: solanaFeePayerEmpty},
		{lamports: 1, want: solanaFeePayerLow},
		{lamports: solanaFeePayerLowBalanceLamports - 1, want: solanaFeePayerLow},
		{lamports: solanaFeePayerLowBalanceLamports, want: solanaFeePayerFunded},
		{lamports: math.MaxUint64, want: solanaFeePayerFunded},
	}
	for _, tt := range tests {
		assert.Equal(t, tt.want, solanaFeePayerBalanceLevel(tt.lamports), "lamports %d", tt.lamports)
	}
}

func TestDecideSolanaOwnTransferAction(t *testing.T) {
	_, err := decideSolanaOwnTransferAction(solanaPendingAccountHasOwnSignature+1, false)
	require.Error(t, err)
}

func TestSolanaCommitSearchSetIndices(t *testing.T) {
	const capped = olderGuardianSetsSearchedForCommitLog + 5
	cappedWant := make([]uint32, 0, olderGuardianSetsSearchedForCommitLog+1)
	for index := uint32(capped); index >= capped-olderGuardianSetsSearchedForCommitLog; index-- {
		cappedWant = append(cappedWant, index)
	}

	tests := []struct {
		name    string
		current uint32
		state   solanaPendingAccountState
		want    []uint32
	}{
		{name: "set 0, current absent", current: 0, state: solanaPendingAccountAbsent, want: []uint32{0}},
		{name: "set 0, current present", current: 0, state: solanaPendingAccountLacksOwnSignature, want: []uint32{}},
		{name: "set 1, current absent", current: 1, state: solanaPendingAccountAbsent, want: []uint32{1, 0}},
		{name: "set 1, current present", current: 1, state: solanaPendingAccountHasOwnSignature, want: []uint32{0}},
		{name: "set 3, current absent", current: 3, state: solanaPendingAccountAbsent, want: []uint32{3, 2, 1, 0}},
		{name: "set 3, current present", current: 3, state: solanaPendingAccountLacksOwnSignature, want: []uint32{2, 1, 0}},
		{name: "lookback cap", current: capped, state: solanaPendingAccountAbsent, want: cappedWant},
	}

	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			assert.Equal(t, tt.want, commitLogSearchGuardianSetIndices(tt.current, tt.state))
		})
	}
}

func TestSnapshotSolanaOwnPendingTransfers(t *testing.T) {
	ctx := context.Background()
	f := newSolanaAuditFixture(t, ctx)

	t.Run("ntt entries are skipped", func(t *testing.T) {
		f.pe.isNTT = true
		t.Cleanup(func() { f.pe.isNTT = false })
		assert.Empty(t, f.acct.snapshotSolanaOwnPendingTransfers(f.acct.solana, 0))
	})

	t.Run("entries without a solana record are skipped", func(t *testing.T) {
		fields := f.pe.solanaFields
		f.pe.solanaFields = nil
		t.Cleanup(func() { f.pe.solanaFields = fields })
		assert.Empty(t, f.acct.snapshotSolanaOwnPendingTransfers(f.acct.solana, 0))
	})
}

func TestAuditSolanaOwnPendingTransfers(t *testing.T) {
	tests := []struct {
		name            string
		setup           func(t *testing.T, f *solanaAuditFixture)
		wantResubmitted bool
		wantPublished   int
		wantPending     int
	}{
		{
			name: "present with own signature waits",
			setup: func(t *testing.T, f *solanaAuditFixture) {
				f.conn.SetAccount(f.pending, f.pendingAccount(t, []uint8{0}))
			},
			wantPending: 1,
		},
		{
			name: "present without own signature resubmits",
			setup: func(t *testing.T, f *solanaAuditFixture) {
				f.conn.SetAccount(f.pending, f.pendingAccount(t, []uint8{1}))
			},
			wantResubmitted: true,
			wantPending:     1,
		},
		{
			name: "present without own signature but accounted searches instead of resubmitting",
			setup: func(t *testing.T, f *solanaAuditFixture) {
				f.conn.SetAccount(f.pending, f.pendingAccount(t, []uint8{1}))
				f.markAccounted(t)
			},
			wantPending: 1,
		},
		{
			name: "present with the wrong content digest is skipped",
			setup: func(t *testing.T, f *solanaAuditFixture) {
				f.conn.SetAccount(f.pending, &solacctconn.OwnedAccount{
					State: solacctconn.AccountInitialised,
					Data:  solanaPendingAccountData(t, f.pe.solanaFields.Chain, 0, [32]byte{0xEE}, solana.PublicKey{1}, nil),
				})
			},
			wantPending: 1,
		},
		{
			name: "absent and accounted with a content digest match publishes",
			setup: func(t *testing.T, f *solanaAuditFixture) {
				f.markAccounted(t)
				f.conn.SetSignaturesForAddress(f.pending, []solana.Signature{{1}})
				f.conn.SetTransaction(solana.Signature{1}, f.commitTransaction(f.pe.solanaFields.contentDigest))
			},
			wantPublished: 1,
		},
		{
			name: "absent and accounted without a commit transaction retries",
			setup: func(t *testing.T, f *solanaAuditFixture) {
				f.markAccounted(t)
				f.conn.SetSignaturesForAddress(f.pending, nil)
			},
			wantPending: 1,
		},
		{
			name: "absent and accounted with a failed commit transaction retries",
			setup: func(t *testing.T, f *solanaAuditFixture) {
				f.markAccounted(t)
				f.conn.SetSignaturesForAddress(f.pending, []solana.Signature{{1}})
				failed := f.commitTransaction(f.pe.solanaFields.contentDigest)
				failed.Failed = true
				f.conn.SetTransaction(solana.Signature{1}, failed)
			},
			wantPending: 1,
		},
		{
			name: "absent with the noreplay bit clear resubmits",
			setup: func(t *testing.T, f *solanaAuditFixture) {
				f.conn.SetAccount(f.bucket, &solacctconn.OwnedAccount{State: solacctconn.AccountInitialised, Data: solanaNoreplayBucket(t)})
			},
			wantResubmitted: true,
			wantPending:     1,
		},
		{
			name:            "absent with no bucket account resubmits",
			setup:           func(t *testing.T, f *solanaAuditFixture) {},
			wantResubmitted: true,
			wantPending:     1,
		},
		{
			name: "absent with an undecodable bucket is skipped",
			setup: func(t *testing.T, f *solanaAuditFixture) {
				f.conn.SetAccount(f.bucket, &solacctconn.OwnedAccount{State: solacctconn.AccountInitialised, Data: []byte{0x00}})
			},
			wantPending: 1,
		},
	}

	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			ctx := context.Background()
			f := newSolanaAuditFixture(t, ctx)
			tt.setup(t, f)

			f.acct.runSolanaAudit(ctx, f.acct.solana)

			assert.Len(t, f.acct.pendingTransfers, tt.wantPending)
			assert.Len(t, f.msgChan, tt.wantPublished)
			if tt.wantResubmitted {
				assert.Len(t, f.acct.solana.subChan, 1)
				assert.True(t, f.pe.submitPending(backendSolana))
				return
			}
			assert.Empty(t, f.acct.solana.subChan)
		})
	}
}

// TestSolanaCommitSearchFindsVaaPathCommitInNoreplayBucket puts the commit only in the
// NoReplay bucket history, as a submit_vaas or backfill commit leaves it.
func TestSolanaCommitSearchFindsVaaPathCommitInNoreplayBucket(t *testing.T) {
	commitSig := solana.Signature{0xC1}
	tests := []struct {
		name string
		// ownSigned leaves a losing sibling with this guardian's bit at the current set.
		ownSigned     bool
		digest        func(f *solanaAuditFixture) [32]byte
		sequence      func(f *solanaAuditFixture) uint64
		blockTimeAge  time.Duration
		wantPublished int
		wantPending   int
	}{
		{name: "current absent, vaa digest", wantPublished: 1},
		{name: "losing sibling, vaa digest", ownSigned: true, wantPublished: 1},
		{name: "current absent, other digest", digest: func(*solanaAuditFixture) [32]byte { return [32]byte{0xEE} }},
		{name: "other sequence in the bucket", sequence: func(f *solanaAuditFixture) uint64 { return f.pe.solanaFields.Sequence + 1 }, wantPending: 1},
		{name: "commit below the timestamp floor", blockTimeAge: noreplayBucketSearchTimestampMargin + time.Hour, wantPending: 1},
	}

	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			ctx := context.Background()
			f := newSolanaAuditFixture(t, ctx)
			f.markAccounted(t)
			if tt.ownSigned {
				f.conn.SetAccount(f.pending, f.pendingAccount(t, []uint8{0}))
			}

			digest := f.pe.vaaDigest
			if tt.digest != nil {
				digest = tt.digest(f)
			}
			sequence := f.pe.solanaFields.Sequence
			if tt.sequence != nil {
				sequence = tt.sequence(f)
			}
			commit := newSolanaCommitEvent(f.pe.solanaFields.Chain, f.pe.solanaFields.Emitter, sequence, digest, 0)
			f.conn.SetTransaction(commitSig, &solacctconn.TransactionResult{LogMessages: commitLogs(f.acct.solana.program, commit)})
			entry := solacctconn.SignatureEntry{Signature: commitSig}
			if tt.blockTimeAge != 0 {
				entry.BlockTime = f.msg.Timestamp.Add(-tt.blockTimeAge)
			}
			f.conn.SetSignatureEntries(f.bucket, []solacctconn.SignatureEntry{entry})

			f.acct.runSolanaAudit(ctx, f.acct.solana)

			assert.Len(t, f.msgChan, tt.wantPublished)
			assert.Len(t, f.acct.pendingTransfers, tt.wantPending)
			calls := f.conn.GetSignaturesForAddressCalls
			require.NotEmpty(t, calls)
			assert.Equal(t, f.bucket, calls[len(calls)-1].Addr)
		})
	}
}

// TestSolanaCommitSearchPagesPastSignatureSpam puts the commit below N spam entries in the
// pending account history. Each audit resumes below the oldest entry the last one examined.
func TestSolanaCommitSearchPagesPastSignatureSpam(t *testing.T) {
	commitSig := solana.Signature{0xC0}
	tests := []struct {
		name      string
		spam      int
		failed    bool
		wantAudit int
	}{
		{name: "no spam", spam: 0, wantAudit: 1},
		{name: "spam fills all but one fetch", spam: 19, wantAudit: 1},
		{name: "spam fills one audit", spam: 20, wantAudit: 2},
		{name: "spam fills two audits and part of a third", spam: 45, wantAudit: 3},
		{name: "failed spam costs no fetch", spam: 45, failed: true, wantAudit: 1},
	}

	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			ctx := context.Background()
			f := newSolanaAuditFixture(t, ctx)
			f.markAccounted(t)

			entries := make([]solacctconn.SignatureEntry, 0, tt.spam+1)
			for idx := range tt.spam {
				sig := solana.Signature{0xA0, byte(idx)}
				entries = append(entries, solacctconn.SignatureEntry{Signature: sig, Failed: tt.failed})
				f.conn.SetTransaction(sig, &solacctconn.TransactionResult{})
			}
			entries = append(entries, solacctconn.SignatureEntry{Signature: commitSig})
			f.conn.SetSignatureEntries(f.pending, entries)
			f.conn.SetTransaction(commitSig, f.commitTransaction(f.pe.solanaFields.contentDigest))

			published := 0
			for audit := 1; audit <= tt.spam/pendingAccountCommitLogSearchLimits.maxTransactionFetchesPerAddress+2 && published == 0; audit++ {
				fetchesBefore := len(f.conn.GetTransactionCalls)
				f.acct.runSolanaAudit(ctx, f.acct.solana)
				assert.LessOrEqual(t, len(f.conn.GetTransactionCalls)-fetchesBefore, pendingAccountCommitLogSearchLimits.maxTransactionFetchesPerAddress, "audit %d fetches", audit)
				if len(f.acct.pendingTransfers) == 0 {
					published = audit
				}
			}

			assert.Equal(t, tt.wantAudit, published)
			assert.Len(t, f.msgChan, 1)
			// A missed audit also scans the NoReplay bucket. Count the pending account reads.
			pendingCalls := make([]MockGetSignaturesForAddressCall, 0, tt.wantAudit)
			for _, call := range f.conn.GetSignaturesForAddressCalls {
				if call.Addr == f.pending {
					pendingCalls = append(pendingCalls, call)
				}
			}
			require.Len(t, pendingCalls, tt.wantAudit)
			for idx, call := range pendingCalls {
				assert.Equal(t, getSignaturesForAddressPageLength, call.Limit)
				if idx == 0 {
					assert.Equal(t, solana.Signature{}, call.Before)
					continue
				}
				oldestExamined := entries[idx*pendingAccountCommitLogSearchLimits.maxTransactionFetchesPerAddress-1].Signature
				assert.Equal(t, oldestExamined, call.Before, "list call %d resumes below the last audit", idx)
			}
			if tt.failed {
				assert.Equal(t, []solana.Signature{commitSig}, f.conn.GetTransactionCalls)
			}
		})
	}
}

// TestAuditSolanaOwnPendingTransfersAccountedWhileCurrentPendingAccountExists covers a commit
// at the previous set after the audit resubmitted at the current guardian set.
func TestAuditSolanaOwnPendingTransfersAccountedWhileCurrentPendingAccountExists(t *testing.T) {
	ctx := context.Background()
	f := newSolanaAuditFixture(t, ctx)
	current, previous := f.moveToGuardianSetOne(t)

	f.conn.SetAccount(current, &solacctconn.OwnedAccount{
		State: solacctconn.AccountInitialised,
		Data:  solanaPendingAccountData(t, f.pe.solanaFields.Chain, 1, f.pe.solanaFields.contentDigest, solana.PublicKey{1}, []uint8{0}),
	})
	f.markAccounted(t)
	f.conn.SetSignaturesForAddress(previous, []solana.Signature{{3}})
	f.conn.SetTransaction(solana.Signature{3}, f.commitTransaction(f.pe.solanaFields.contentDigest))

	f.acct.runSolanaAudit(ctx, f.acct.solana)

	assert.Empty(t, f.acct.pendingTransfers)
	assert.Len(t, f.msgChan, 1)
	require.Len(t, f.conn.GetSignaturesForAddressCalls, 1)
	assert.Equal(t, previous, f.conn.GetSignaturesForAddressCalls[0].Addr)
	assert.Equal(t, []MockGetOwnedAccountsCall{
		{Owner: f.acct.solana.program, Commitment: solacctconn.CommitmentFinalized},
		{Owner: f.acct.solana.noreplay, Commitment: solacctconn.CommitmentFinalized},
	}, f.conn.GetOwnedAccountsCalls)
}

func TestAuditSolanaProgramPendingAccounts(t *testing.T) {
	recoveryTxID := bytes.Repeat([]byte{0x5A}, hashTxIDLen)
	recoverySignatureTxID := bytes.Repeat([]byte{0x5B}, signatureTxIDLen)

	tests := []struct {
		name               string
		setup              func(t *testing.T, f *solanaAuditFixture)
		wantResubmitted    bool
		wantReobserveChain vaa.ChainID
		wantReobserveTxID  []byte
		wantAuditErrors    float64
	}{
		{
			name: "own transfer the own-transfer pass left unresolved resubmits",
			setup: func(t *testing.T, f *solanaAuditFixture) {
				f.conn.GetOwnedAccountsErr = errors.New("rpc down")
				f.conn.ProgramAccounts = []solacctconn.ProgramAccount{
					solanaProgramAccountFor(t, f.pending, f.pe.solanaFields.Chain, 0, f.pe.solanaFields.contentDigest, nil),
				}
			},
			wantResubmitted: true,
			wantAuditErrors: 2,
		},
		{
			name: "own transfer the own-transfer pass reconciled is skipped",
			setup: func(t *testing.T, f *solanaAuditFixture) {
				f.conn.ProgramAccounts = []solacctconn.ProgramAccount{
					solanaProgramAccountFor(t, f.pending, f.pe.solanaFields.Chain, 0, f.pe.solanaFields.contentDigest, nil),
				}
			},
		},
		{
			name: "own transfer whose account reports another set index is skipped",
			setup: func(t *testing.T, f *solanaAuditFixture) {
				f.conn.GetOwnedAccountsErr = errors.New("rpc down")
				f.conn.ProgramAccounts = []solacctconn.ProgramAccount{
					solanaProgramAccountFor(t, f.pending, f.pe.solanaFields.Chain, 9, f.pe.solanaFields.contentDigest, nil),
				}
			},
			wantAuditErrors: 2,
		},
		{
			name: "previous-set account of an own transfer is skipped",
			setup: func(t *testing.T, f *solanaAuditFixture) {
				current, previous := f.moveToGuardianSetOne(t)
				f.conn.SetAccount(current, &solacctconn.OwnedAccount{
					State: solacctconn.AccountInitialised,
					Data:  solanaPendingAccountData(t, f.pe.solanaFields.Chain, 1, f.pe.solanaFields.contentDigest, solana.PublicKey{1}, []uint8{0}),
				})
				f.conn.ProgramAccounts = []solacctconn.ProgramAccount{
					solanaProgramAccountFor(t, previous, f.pe.solanaFields.Chain, 0, f.pe.solanaFields.contentDigest, nil),
				}
			},
		},
		{
			name: "unknown pending account is reobserved with its stored tx id",
			setup: func(t *testing.T, f *solanaAuditFixture) {
				fields, _ := solanaUnknownTransfer(t, f.acct.solana.program, vaa.ChainIDEthereum, 7)
				txID := mustSolanaTxIDBytes(t, recoveryTxID)
				pda, err := derivePendingObservationsPDA(f.acct.solana.program, fields.Chain, fields.Emitter, fields.Sequence, 0, fields.contentDigest, txID)
				require.NoError(t, err)
				f.conn.ProgramAccounts = []solacctconn.ProgramAccount{
					solanaProgramAccountForTxID(t, pda, fields.Chain, 0, fields.contentDigest, txID, nil),
				}
			},
			wantReobserveChain: vaa.ChainIDEthereum,
			wantReobserveTxID:  recoveryTxID,
		},
		{
			name: "unknown pending account with a 64-byte tx id is reobserved with the full id",
			setup: func(t *testing.T, f *solanaAuditFixture) {
				fields, _ := solanaUnknownTransfer(t, f.acct.solana.program, vaa.ChainIDSolana, 7)
				txID := mustSolanaTxIDBytes(t, recoverySignatureTxID)
				pda, err := derivePendingObservationsPDA(f.acct.solana.program, fields.Chain, fields.Emitter, fields.Sequence, 0, fields.contentDigest, txID)
				require.NoError(t, err)
				f.conn.ProgramAccounts = []solacctconn.ProgramAccount{
					solanaProgramAccountForTxID(t, pda, fields.Chain, 0, fields.contentDigest, txID, nil),
				}
			},
			wantReobserveChain: vaa.ChainIDSolana,
			wantReobserveTxID:  recoverySignatureTxID,
		},
		{
			name: "sibling tx id of an own transfer is reobserved",
			setup: func(t *testing.T, f *solanaAuditFixture) {
				fields := f.pe.solanaFields
				txID := mustSolanaTxIDBytes(t, recoverySignatureTxID)
				sibling, err := derivePendingObservationsPDA(f.acct.solana.program, fields.Chain, fields.Emitter, fields.Sequence, 0, fields.contentDigest, txID)
				require.NoError(t, err)
				require.NotEqual(t, f.pending, sibling)
				f.conn.ProgramAccounts = []solacctconn.ProgramAccount{
					solanaProgramAccountForTxID(t, sibling, fields.Chain, 0, fields.contentDigest, txID, []uint8{1}),
				}
			},
			wantReobserveChain: vaa.ChainIDEthereum,
			wantReobserveTxID:  recoverySignatureTxID,
		},
		{
			name: "undecodable pending account is skipped",
			setup: func(t *testing.T, f *solanaAuditFixture) {
				f.conn.ProgramAccounts = []solacctconn.ProgramAccount{{Address: solana.PublicKey{0x77}, Data: []byte{0x01}}}
			},
			wantAuditErrors: 1,
		},
		{
			name: "program account query error ends the pass",
			setup: func(t *testing.T, f *solanaAuditFixture) {
				f.conn.ProgramAccountsErr = errors.New("rpc down")
			},
			wantAuditErrors: 1,
		},
	}

	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			ctx := context.Background()
			f := newSolanaAuditFixture(t, ctx)
			f.conn.SetAccount(f.pending, f.pendingAccount(t, []uint8{0}))
			tt.setup(t, f)
			errorsBefore := testutil.ToFloat64(solanaAuditErrors)

			f.acct.runSolanaAudit(ctx, f.acct.solana)

			assert.Equal(t, tt.wantAuditErrors, testutil.ToFloat64(solanaAuditErrors)-errorsBefore)
			assert.Empty(t, f.conn.GetSignaturesForAddressCalls)
			if tt.wantReobserveTxID != nil {
				require.Len(t, f.obsvReq, 1)
				req := <-f.obsvReq
				assert.Equal(t, uint32(tt.wantReobserveChain), req.ChainId)
				assert.Equal(t, tt.wantReobserveTxID, req.TxHash)
			} else {
				assert.Empty(t, f.obsvReq)
			}
			if tt.wantResubmitted {
				assert.Len(t, f.acct.solana.subChan, 1)
				return
			}
			assert.Empty(t, f.acct.solana.subChan)
		})
	}
}

func TestRunSolanaAuditSkipsAGuardianOutsideTheSet(t *testing.T) {
	ctx := context.Background()
	f := newSolanaAuditFixture(t, ctx)

	f.acct.gst.Set(&common.GuardianSet{Index: 0, Keys: nil})
	f.acct.runSolanaAudit(ctx, f.acct.solana)
	assert.Empty(t, f.conn.GetBalanceCalls)
}

// TestAuditSolanaProgramPendingAccountsVisitsEveryAccount rotates the program-account pass,
// so every unknown account gets a reobservation request within a bounded number of audits.
func TestAuditSolanaProgramPendingAccountsVisitsEveryAccount(t *testing.T) {
	address := func(idx int) solana.PublicKey {
		var addr solana.PublicKey
		binary.BigEndian.PutUint32(addr[:4], uint32(idx)) // #nosec G115 -- idx <= 10_001
		addr[31] = 0x01
		return addr
	}
	tests := []struct {
		name     string
		total    int
		unknownN func(idx int) bool
		// reverse hands the accounts over in descending address order.
		reverse bool
	}{
		{name: "more unknown accounts than one audit requests", total: 250, unknownN: func(int) bool { return true }, reverse: true},
		{
			name:     "unknown account past the read cap",
			total:    maxPendingAccountsReadPerAudit + 1,
			unknownN: func(idx int) bool { return idx == maxPendingAccountsReadPerAudit },
		},
	}

	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			ctx := context.Background()
			f := newSolanaAuditFixture(t, ctx)
			f.conn.SetAccount(f.pending, f.pendingAccount(t, []uint8{0}))

			// Each account stores its own address as its tx id, so a request names its account.
			accounts := make([]solacctconn.ProgramAccount, tt.total)
			unknown := make(map[solana.PublicKey]struct{})
			for idx := range accounts {
				var signedBy []uint8
				if tt.unknownN(idx) {
					unknown[address(idx)] = struct{}{}
				} else {
					signedBy = []uint8{0}
				}
				addr := address(idx)
				accounts[idx] = solanaProgramAccountForTxID(t, addr, vaa.ChainIDEthereum, 0, [32]byte{0x12}, mustSolanaTxIDBytes(t, addr[:]), signedBy)
			}
			if tt.reverse {
				slices.Reverse(accounts)
			}
			f.conn.ProgramAccounts = accounts

			readAudits := (tt.total + maxPendingAccountsReadPerAudit - 1) / maxPendingAccountsReadPerAudit
			requestAudits := (len(unknown) + maxReobservationRequestsPerAudit - 1) / maxReobservationRequestsPerAudit
			audits := max(readAudits, requestAudits) + 1
			requested := make(map[solana.PublicKey]struct{})
			for audit := 1; audit <= audits; audit++ {
				f.acct.runSolanaAudit(ctx, f.acct.solana)
				assert.LessOrEqual(t, len(f.obsvReq), maxReobservationRequestsPerAudit, "audit %d requests", audit)
				for len(f.obsvReq) > 0 {
					requested[solana.PublicKeyFromBytes((<-f.obsvReq).TxHash)] = struct{}{}
				}
			}
			assert.Equal(t, unknown, requested)
			assert.Empty(t, f.conn.GetSignaturesForAddressCalls)
		})
	}
}

// Partition 0 fills the read cap, partition 1 is past the response limit, partition 2 holds
// one unknown account.
func TestAuditSolanaProgramPendingAccountsReadsTxIDPartitions(t *testing.T) {
	ctx := context.Background()
	f := newSolanaAuditFixture(t, ctx)
	f.conn.SetAccount(f.pending, f.pendingAccount(t, []uint8{0}))

	account := func(partition uint8, idx int, signedBy []uint8) solacctconn.ProgramAccount {
		var addr solana.PublicKey
		addr[0] = partition
		binary.BigEndian.PutUint32(addr[1:5], uint32(idx)) // #nosec G115 -- idx <= 10_000
		addr[31] = 0x01
		acc := solanaProgramAccountForTxID(t, addr, vaa.ChainIDEthereum, 0, [32]byte{0x56}, mustSolanaTxIDBytes(t, addr[:]), signedBy)
		require.Equal(t, partition, acc.Data[solacctconn.PendingTxIDOffset])
		return acc
	}
	accounts := make([]solacctconn.ProgramAccount, 0, 2*maxPendingAccountsReadPerAudit+2)
	for idx := range maxPendingAccountsReadPerAudit {
		accounts = append(accounts, account(0, idx, []uint8{0}))
	}
	for idx := range maxPendingAccountsReadPerAudit + 1 {
		accounts = append(accounts, account(1, idx, nil))
	}
	unknown := account(2, 0, nil)
	accounts = append(accounts, unknown)
	f.conn.ProgramAccounts = accounts
	f.conn.ProgramAccountsMaxResults = maxPendingAccountsReadPerAudit

	f.acct.runSolanaAudit(ctx, f.acct.solana)
	assert.Equal(t, []solacctconn.TxIDPartition{solacctconn.AllTxIDs, solacctconn.TxIDsStartingWith(0)}, f.conn.ProgramAccountsPartitions)
	assert.Empty(t, f.obsvReq)

	f.conn.ProgramAccountsPartitions = nil
	f.acct.runSolanaAudit(ctx, f.acct.solana)
	require.Len(t, f.conn.ProgramAccountsPartitions, 1+256)
	assert.Equal(t, solacctconn.TxIDsStartingWith(1), f.conn.ProgramAccountsPartitions[1])
	require.Len(t, f.obsvReq, 1)
	assert.Equal(t, unknown.Address.Bytes(), (<-f.obsvReq).TxHash)
}

func TestAuditSolanaProgramPendingAccountsBoundsReobservationRequests(t *testing.T) {
	ctx := context.Background()
	f := newSolanaAuditFixture(t, ctx)
	f.conn.SetAccount(f.pending, f.pendingAccount(t, []uint8{0}))

	accounts := make([]solacctconn.ProgramAccount, maxReobservationRequestsPerAudit+1)
	for idx := range accounts {
		var addr solana.PublicKey
		binary.LittleEndian.PutUint32(addr[:4], uint32(idx)) // #nosec G115 -- idx < 101
		accounts[idx] = solanaProgramAccountFor(t, addr, vaa.ChainIDEthereum, 0, [32]byte{0x34}, nil)
	}
	f.conn.ProgramAccounts = accounts

	f.acct.runSolanaAudit(ctx, f.acct.solana)
	assert.Len(t, f.obsvReq, maxReobservationRequestsPerAudit)
}

func TestAuditSolanaOwnPendingTransfersBoundsCommitSearches(t *testing.T) {
	ctx := context.Background()
	f := newSolanaAuditFixture(t, ctx)

	// Every transfer is accounted and has no current guardian-set pending account at set index 0.
	// Thus each commit log search reads one signature list.
	marked := make([]uint64, 0, pendingAccountCommitLogSearchLimits.maxSearchesPerAudit+1)
	marked = append(marked, f.pe.solanaFields.Sequence)
	for idx := range pendingAccountCommitLogSearchLimits.maxSearchesPerAudit {
		sequence := uint64(100 + idx) // #nosec G115 -- idx < 100
		_, err := f.acct.SubmitObservation(solanaTestTransfer(t, sequence))
		require.NoError(t, err)
		marked = append(marked, sequence)
	}
	// Sequences below 1024 share the fixture bucket.
	f.conn.SetAccount(f.bucket, &solacctconn.OwnedAccount{State: solacctconn.AccountInitialised, Data: solanaNoreplayBucket(t, marked...)})
	require.Len(t, f.acct.pendingTransfers, pendingAccountCommitLogSearchLimits.maxSearchesPerAudit+1)

	f.acct.runSolanaAudit(ctx, f.acct.solana)
	bucketCalls := 0
	for _, call := range f.conn.GetSignaturesForAddressCalls {
		if call.Addr == f.bucket {
			bucketCalls++
		}
	}
	// The missed transfers share one bucket, so one NoReplay bucket search follows the pending
	// account searches.
	assert.Equal(t, 1, bucketCalls)
	assert.Len(t, f.conn.GetSignaturesForAddressCalls, pendingAccountCommitLogSearchLimits.maxSearchesPerAudit+bucketCalls)
	assert.Empty(t, f.acct.solana.subChan)
}
