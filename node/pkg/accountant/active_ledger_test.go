package accountant

import (
	"context"
	"testing"
	"time"

	"github.com/certusone/wormhole/node/pkg/common"
	"github.com/prometheus/client_golang/prometheus/testutil"
	"github.com/stretchr/testify/assert"
	"github.com/stretchr/testify/require"
)

// activeLedgerFamily is one program family with a wormchain and a Solana ledger enabled.
type activeLedgerFamily struct {
	name          string
	opts          solanaTestOpts
	transfer      func(t *testing.T, sequence uint64) *common.MessagePublication
	isNTT         bool
	solanaBackend accountantBackend
	solanaFamily  solanaProgramFamily
	solanaTag     string
	wormchainTag  string
}

func activeLedgerFamilies() []activeLedgerFamily {
	return []activeLedgerFamily{
		{
			name:          "wtt",
			opts:          solanaTestOpts{wormchainContract: "0xdeadbeef", enforce: true},
			transfer:      solanaTestTransfer,
			solanaBackend: backendSolana,
			solanaFamily:  solanaFamilyWTT,
			solanaTag:     "solana-accountant",
			wormchainTag:  "accountant",
		},
		{
			name: "ntt",
			opts: solanaTestOpts{nttContract: "0xdeadbeef", enforce: true, solanaNtt: true},
			transfer: func(t *testing.T, sequence uint64) *common.MessagePublication {
				return solanaTestNttTransfer(t, sequence, false)
			},
			isNTT:         true,
			solanaBackend: backendSolanaNTT,
			solanaFamily:  solanaFamilyNTT,
			solanaTag:     "solana-ntt-accountant",
			wormchainTag:  "ntt-accountant",
		},
	}
}

// withActiveLedger returns the family options with ledger active for the family.
func (f activeLedgerFamily) withActiveLedger(ledger Ledger) solanaTestOpts {
	opts := f.opts
	if f.isNTT {
		opts.activeLedgers.NTT = ledger
	} else {
		opts.activeLedgers.WTT = ledger
	}
	return opts
}

func submitTestPendingTransfer(t *testing.T, acct *Accountant, msg *common.MessagePublication) *pendingEntry {
	t.Helper()
	shouldPublish, err := acct.SubmitObservation(msg)
	require.NoError(t, err)
	require.False(t, shouldPublish)
	pe := acct.pendingTransfers[msg.MessageIDString()]
	require.NotNil(t, pe)
	return pe
}

func reportSolanaDigest(acct *Accountant, f activeLedgerFamily, pe *pendingEntry, digest [32]byte) bool {
	acct.pendingTransfersLock.Lock()
	defer acct.pendingTransfersLock.Unlock()
	return acct.processCommittedDigest(pe.msgId, digest, f.solanaFamily, "test")
}

var conflictingSolanaDigest = [32]byte{0xde, 0xad}

func TestMirrorCommitDoesNotRelease(t *testing.T) {
	for _, f := range activeLedgerFamilies() {
		t.Run(f.name, func(t *testing.T) {
			ctx := context.Background()
			acct, _, msgChan := newSolanaTestAccountant(t, ctx, f.opts)
			pe := submitTestPendingTransfer(t, acct, f.transfer(t, 41))

			assert.True(t, reportSolanaDigest(acct, f, pe, pe.vaaDigest))
			assert.Empty(t, msgChan, "a mirror commit does not release")
			require.Contains(t, acct.pendingTransfers, pe.msgId)
			assert.Equal(t, backendOutcomeCommitted, pe.backendOutcome(f.solanaBackend))
			assert.False(t, pe.isReleasedByActiveLedger())

			acct.handleCommittedTransfer(pe.msgId)
			require.Len(t, msgChan, 1)
			assert.Equal(t, pe.msg, <-msgChan)
			assert.NotContains(t, acct.pendingTransfers, pe.msgId)
		})
	}
}

func TestSolanaActiveReleasesOnlyOnSolanaCommit(t *testing.T) {
	for _, f := range activeLedgerFamilies() {
		t.Run(f.name, func(t *testing.T) {
			ctx := context.Background()
			acct, _, msgChan := newSolanaTestAccountant(t, ctx, f.withActiveLedger(LedgerSolana))
			pe := submitTestPendingTransfer(t, acct, f.transfer(t, 42))

			acct.handleCommittedTransfer(pe.msgId)
			assert.Empty(t, msgChan, "a wormchain mirror commit does not release")
			require.Contains(t, acct.pendingTransfers, pe.msgId)

			assert.True(t, reportSolanaDigest(acct, f, pe, pe.vaaDigest))
			require.Len(t, msgChan, 1)
			assert.NotContains(t, acct.pendingTransfers, pe.msgId)
		})
	}
}

func TestActiveCommitKeepsEntryForMirror(t *testing.T) {
	for _, f := range activeLedgerFamilies() {
		t.Run(f.name, func(t *testing.T) {
			ctx := context.Background()
			acct, _, msgChan := newSolanaTestAccountant(t, ctx, f.opts)
			pe := submitTestPendingTransfer(t, acct, f.transfer(t, 43))
			syncedBefore := testutil.ToFloat64(mirrorSynced.WithLabelValues(f.solanaTag))

			acct.handleCommittedTransfer(pe.msgId)
			require.Len(t, msgChan, 1)
			<-msgChan
			require.Contains(t, acct.pendingTransfers, pe.msgId, "the mirror still needs the transfer")
			assert.True(t, pe.isReleasedByActiveLedger())

			msgs := []*common.MessagePublication{pe.msg}
			assert.Len(t, acct.removeCompleted(msgs, f.solanaBackend), 1)
			assert.Empty(t, acct.removeCompleted(msgs, backendWormchain))
			solana := acct.solana
			if f.isNTT {
				solana = acct.solanaNtt
			}
			assert.Len(t, acct.snapshotSolanaOwnPendingTransfers(solana, 0), 1)
			assert.Empty(t, acct.createAuditMap(f.isNTT))

			assert.True(t, reportSolanaDigest(acct, f, pe, pe.vaaDigest))
			assert.Empty(t, msgChan, "the mirror commit does not publish again")
			assert.NotContains(t, acct.pendingTransfers, pe.msgId)
			assert.Equal(t, 1.0, testutil.ToFloat64(mirrorSynced.WithLabelValues(f.solanaTag))-syncedBefore)
		})
	}
}

func TestMirrorRejectionStopsMirrorSubmissions(t *testing.T) {
	for _, f := range activeLedgerFamilies() {
		t.Run(f.name, func(t *testing.T) {
			ctx := context.Background()
			acct, _, msgChan := newSolanaTestAccountant(t, ctx, f.opts)
			pe := submitTestPendingTransfer(t, acct, f.transfer(t, 44))
			divergencesBefore := testutil.ToFloat64(mirrorDivergences.WithLabelValues(f.solanaTag))

			assert.False(t, reportSolanaDigest(acct, f, pe, conflictingSolanaDigest))
			assert.Equal(t, 1.0, testutil.ToFloat64(mirrorDivergences.WithLabelValues(f.solanaTag))-divergencesBefore)
			assert.Equal(t, backendOutcomeRejected, pe.backendOutcome(f.solanaBackend))
			require.Contains(t, acct.pendingTransfers, pe.msgId, "a mirror rejection does not drop the transfer")
			assert.False(t, acct.submitObservation(ctx, pe, f.solanaBackend, false))

			acct.handleCommittedTransfer(pe.msgId)
			require.Len(t, msgChan, 1)
			assert.NotContains(t, acct.pendingTransfers, pe.msgId)
		})
	}
}

func TestActiveRejectionDropsEntry(t *testing.T) {
	for _, f := range activeLedgerFamilies() {
		t.Run(f.name, func(t *testing.T) {
			ctx := context.Background()
			acct, _, msgChan := newSolanaTestAccountant(t, ctx, f.opts)
			pe := submitTestPendingTransfer(t, acct, f.transfer(t, 45))

			acct.handleTransferError(pe.msgId, "insufficient balance", "test")
			assert.Empty(t, msgChan)
			assert.NotContains(t, acct.pendingTransfers, pe.msgId)
		})
	}
}

func TestActiveRejectionAfterMirrorCommitCountsDivergence(t *testing.T) {
	for _, f := range activeLedgerFamilies() {
		t.Run(f.name, func(t *testing.T) {
			ctx := context.Background()
			acct, _, msgChan := newSolanaTestAccountant(t, ctx, f.opts)
			pe := submitTestPendingTransfer(t, acct, f.transfer(t, 46))
			divergencesBefore := testutil.ToFloat64(mirrorDivergences.WithLabelValues(f.solanaTag))

			assert.True(t, reportSolanaDigest(acct, f, pe, pe.vaaDigest))
			acct.handleTransferError(pe.msgId, "insufficient balance", "test")
			assert.Empty(t, msgChan)
			assert.NotContains(t, acct.pendingTransfers, pe.msgId)
			assert.Equal(t, 1.0, testutil.ToFloat64(mirrorDivergences.WithLabelValues(f.solanaTag))-divergencesBefore)
		})
	}
}

func TestBackendCommitIsIdempotent(t *testing.T) {
	ctx := context.Background()
	f := activeLedgerFamilies()[0]
	acct, _, msgChan := newSolanaTestAccountant(t, ctx, f.opts)
	pe := submitTestPendingTransfer(t, acct, f.transfer(t, 47))

	for range 3 {
		acct.handleCommittedTransfer(pe.msgId)
	}
	assert.Len(t, msgChan, 1)
	assert.Equal(t, 0.0, testutil.ToFloat64(transfersOutstanding))
	assert.Equal(t, 1.0, testutil.ToFloat64(mirrorPendingTransfers))
}

func TestReobservationOfReleasedEntryPublishes(t *testing.T) {
	ctx := context.Background()
	f := activeLedgerFamilies()[0]
	acct, _, _ := newSolanaTestAccountant(t, ctx, f.opts)
	msg := f.transfer(t, 48)
	pe := submitTestPendingTransfer(t, acct, msg)
	acct.handleCommittedTransfer(pe.msgId)

	shouldPublish, err := acct.SubmitObservation(msg)
	require.NoError(t, err)
	assert.True(t, shouldPublish)
}

func TestExpireStalledMirrors(t *testing.T) {
	tests := []struct {
		name        string
		releasedAgo time.Duration
		wantExpired bool
	}{
		{name: "past the deadline", releasedAgo: 25 * time.Hour, wantExpired: true},
		{name: "within the deadline", releasedAgo: 23 * time.Hour},
	}
	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			ctx := context.Background()
			f := activeLedgerFamilies()[0]
			acct, _, _ := newSolanaTestAccountant(t, ctx, f.opts)
			pe := submitTestPendingTransfer(t, acct, f.transfer(t, 49))
			acct.handleCommittedTransfer(pe.msgId)
			pe.stateLock.Lock()
			pe.state.releasedAt = time.Now().Add(-tt.releasedAgo)
			pe.stateLock.Unlock()
			timeoutsBefore := testutil.ToFloat64(mirrorTimeouts.WithLabelValues(f.solanaTag))

			acct.expireStalledMirrors()

			if tt.wantExpired {
				assert.NotContains(t, acct.pendingTransfers, pe.msgId)
				assert.Equal(t, backendOutcomeMirrorTimedOut, pe.backendOutcome(f.solanaBackend))
				assert.Equal(t, 1.0, testutil.ToFloat64(mirrorTimeouts.WithLabelValues(f.solanaTag))-timeoutsBefore)
			} else {
				assert.Contains(t, acct.pendingTransfers, pe.msgId)
				assert.Equal(t, 0.0, testutil.ToFloat64(mirrorTimeouts.WithLabelValues(f.solanaTag))-timeoutsBefore)
			}
		})
	}
}

func TestResolveActiveLedger(t *testing.T) {
	tests := []struct {
		name             string
		requested        Ledger
		wormchainEnabled bool
		solanaEnabled    bool
		want             Ledger
	}{
		{name: "unset, wormchain only", wormchainEnabled: true, want: LedgerWormchain},
		{name: "unset, solana only", solanaEnabled: true, want: LedgerSolana},
		{name: "unset, both", wormchainEnabled: true, solanaEnabled: true, want: LedgerWormchain},
		{name: "unset, neither", want: LedgerUnset},
		{name: "explicit solana, both", requested: LedgerSolana, wormchainEnabled: true, solanaEnabled: true, want: LedgerSolana},
		{name: "explicit wormchain, both", requested: LedgerWormchain, wormchainEnabled: true, solanaEnabled: true, want: LedgerWormchain},
	}
	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			assert.Equal(t, tt.want, resolveActiveLedger(tt.requested, tt.wormchainEnabled, tt.solanaEnabled))
		})
	}
}

func TestStartRejectsActiveLedgerThatIsNotEnabled(t *testing.T) {
	tests := []struct {
		name    string
		opts    solanaTestOpts
		wantErr bool
	}{
		{name: "wtt solana without solana", opts: solanaTestOpts{wormchainContract: "0xdeadbeef", disableSolana: true, activeLedgers: ActiveLedgers{WTT: LedgerSolana}}, wantErr: true},
		{name: "wtt wormchain without wormchain", opts: solanaTestOpts{activeLedgers: ActiveLedgers{WTT: LedgerWormchain}}, wantErr: true},
		{name: "ntt wormchain uses the ntt contract", opts: solanaTestOpts{wormchainContract: "0xdeadbeef", solanaNtt: true, activeLedgers: ActiveLedgers{NTT: LedgerWormchain}}, wantErr: true},
		{name: "ntt wormchain with the ntt contract", opts: solanaTestOpts{nttContract: "0xdeadbeef", solanaNtt: true, activeLedgers: ActiveLedgers{NTT: LedgerWormchain}}},
		{name: "families are independent", opts: solanaTestOpts{wormchainContract: "0xdeadbeef", nttContract: "0xdeadbeef", solanaNtt: true, activeLedgers: ActiveLedgers{WTT: LedgerSolana, NTT: LedgerWormchain}}},
	}
	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			ctx := context.Background()
			acct, _, _ := newUnstartedSolanaTestAccountant(t, ctx, tt.opts, nil)
			err := acct.Start(ctx)
			if tt.wantErr {
				assert.Error(t, err)
			} else {
				assert.NoError(t, err)
			}
		})
	}
}

func TestParseLedger(t *testing.T) {
	tests := []struct {
		flagValue string
		want      Ledger
		wantErr   bool
	}{
		{flagValue: "", want: LedgerUnset},
		{flagValue: "wormchain", want: LedgerWormchain},
		{flagValue: "solana", want: LedgerSolana},
		{flagValue: "Solana", wantErr: true},
		{flagValue: "ethereum", wantErr: true},
	}
	for _, tt := range tests {
		t.Run(tt.flagValue, func(t *testing.T) {
			got, err := ParseLedger(tt.flagValue)
			if tt.wantErr {
				assert.Error(t, err)
				return
			}
			require.NoError(t, err)
			assert.Equal(t, tt.want, got)
			if tt.want != LedgerUnset {
				assert.Equal(t, tt.flagValue, got.String())
			}
		})
	}
}
