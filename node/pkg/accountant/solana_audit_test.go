package accountant

import (
	"bytes"
	"context"
	"encoding/binary"
	"errors"
	"testing"

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

	obsvReq := make(chan *gossipv1.ObservationRequest, 10)
	acct, conn, msgChan := newSolanaTestAccountantWithObsvReq(t, ctx, solanaTestOpts{enforce: true}, obsvReq)
	conn.Balance = 1_000_000

	msg := solanaTestTransfer(t, 31)
	_, err := acct.SubmitObservation(msg)
	require.NoError(t, err)
	pe := acct.pendingTransfers[msg.MessageIDString()]
	require.NotNil(t, pe)
	require.NotNil(t, pe.solanaFields)

	b := acct.solana
	pending, err := solanaPendingPDAAtSet(b, pe, 0)
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

	current, err := solanaPendingPDAAtSet(f.acct.solana, f.pe, 1)
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
	pda, err := derivePendingObservationsPDA(program, fields.Chain, fields.Emitter, fields.Sequence, 0, fields.contentDigest)
	require.NoError(t, err)
	return fields, pda
}

// solanaProgramAccountFor builds a getProgramAccounts result row.
func solanaProgramAccountFor(t *testing.T, addr solana.PublicKey, chain vaa.ChainID, guardianSetIndex uint32, digest [32]byte, signedBy []uint8) solacctconn.ProgramAccount {
	t.Helper()
	return solacctconn.ProgramAccount{
		Address: addr,
		Data:    solanaPendingAccountData(t, chain, guardianSetIndex, digest, solana.PublicKey{0x01}, signedBy),
	}
}

// solanaSubmitTransaction is a transaction with one submit_observations instruction per
// record, each carrying txHash.
func solanaSubmitTransaction(t *testing.T, program solana.PublicKey, txIDBytes []byte, records ...solanaObservationFields) *solacctconn.TransactionResult {
	t.Helper()
	txID, err := newSolanaTxID(txIDBytes)
	require.NoError(t, err)
	tx := &solacctconn.TransactionResult{}
	for idx := range records {
		data, err := encodeSubmitObservationsIxData(0, 0, make([]byte, submitSignatureLen), txID, &records[idx])
		require.NoError(t, err)
		tx.Instructions = append(tx.Instructions, solacctconn.Instruction{ProgramID: program, Data: data})
	}
	return tx
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
		{name: "funded", lamports: 5_000_000, wantGauge: 5_000_000},
		{name: "empty", lamports: 0, wantGauge: 0, wantErrors: 1},
		{name: "query error", err: errors.New("rpc down"), wantGauge: 7, wantErrors: 1},
	}

	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			acct, conn, _ := newSolanaTestAccountant(t, ctx, solanaTestOpts{enforce: true})
			conn.Balance, conn.BalanceErr = tt.lamports, tt.err
			solanaFeePayerLamports.Set(7)
			errorsBefore := testutil.ToFloat64(solanaFeePayerErrors)

			acct.publishSolanaFeePayerBalance(ctx, acct.solana)
			require.Len(t, conn.GetBalanceCalls, 1)
			assert.Equal(t, acct.solana.feePayer.PublicKey(), conn.GetBalanceCalls[0])
			assert.Equal(t, tt.wantGauge, testutil.ToFloat64(solanaFeePayerLamports))
			assert.Equal(t, tt.wantErrors, testutil.ToFloat64(solanaFeePayerErrors)-errorsBefore)
		})
	}
}

func TestDecideSolanaOwnTransferAction(t *testing.T) {
	_, err := decideSolanaOwnTransferAction(solanaPendingAccountHasOwnSignature+1, false)
	require.Error(t, err)
}

func TestSolanaCommitSearchPDAs(t *testing.T) {
	current := solana.PublicKey{0x01}
	previous := solana.PublicKey{0x02}

	tests := []struct {
		name     string
		state    solanaPendingAccountState
		previous *solana.PublicKey
		want     []solana.PublicKey
	}{
		{name: "current absent with a previous set", state: solanaPendingAccountAbsent, previous: &previous, want: []solana.PublicKey{current, previous}},
		{name: "current absent at set zero", state: solanaPendingAccountAbsent, want: []solana.PublicKey{current}},
		{name: "current present with a previous set", state: solanaPendingAccountHasOwnSignature, previous: &previous, want: []solana.PublicKey{previous}},
		{name: "current present at set zero", state: solanaPendingAccountLacksOwnSignature, want: []solana.PublicKey{}},
	}

	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			got := solanaCommitSearchPDAs(current, tt.state, solanaOwnPendingTransfer{previousSetPendingPDA: tt.previous})
			assert.Equal(t, tt.want, got)
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
		wantSearches       int
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
			name: "unknown pending account uses the instruction that derives it",
			setup: func(t *testing.T, f *solanaAuditFixture) {
				fields, pda := solanaUnknownTransfer(t, f.acct.solana.program, vaa.ChainIDEthereum, 7)
				other, _ := solanaUnknownTransfer(t, f.acct.solana.program, vaa.ChainIDEthereum, 8)
				otherTx := solanaSubmitTransaction(t, f.acct.solana.program, bytes.Repeat([]byte{0x01}, hashTxIDLen), other)
				tx := solanaSubmitTransaction(t, f.acct.solana.program, recoveryTxID, fields)
				tx.Instructions = append(otherTx.Instructions, tx.Instructions...)

				f.conn.ProgramAccounts = []solacctconn.ProgramAccount{
					solanaProgramAccountFor(t, pda, fields.Chain, 0, fields.contentDigest, nil),
				}
				f.conn.SetSignaturesForAddress(pda, []solana.Signature{{2}})
				f.conn.SetTransaction(solana.Signature{2}, tx)
			},
			wantSearches:       1,
			wantReobserveChain: vaa.ChainIDEthereum,
			wantReobserveTxID:  recoveryTxID,
		},
		{
			name: "unknown pending account with a 64-byte tx id is reobserved with the full id",
			setup: func(t *testing.T, f *solanaAuditFixture) {
				fields, pda := solanaUnknownTransfer(t, f.acct.solana.program, vaa.ChainIDSolana, 7)
				f.conn.ProgramAccounts = []solacctconn.ProgramAccount{
					solanaProgramAccountFor(t, pda, fields.Chain, 0, fields.contentDigest, nil),
				}
				f.conn.SetSignaturesForAddress(pda, []solana.Signature{{2}})
				f.conn.SetTransaction(solana.Signature{2}, solanaSubmitTransaction(t, f.acct.solana.program, recoverySignatureTxID, fields))
			},
			wantSearches:       1,
			wantReobserveChain: vaa.ChainIDSolana,
			wantReobserveTxID:  recoverySignatureTxID,
		},
		{
			name: "unknown pending account without an accountant instruction is not reobserved",
			setup: func(t *testing.T, f *solanaAuditFixture) {
				fields, pda := solanaUnknownTransfer(t, f.acct.solana.program, vaa.ChainIDEthereum, 7)
				f.conn.ProgramAccounts = []solacctconn.ProgramAccount{
					solanaProgramAccountFor(t, pda, fields.Chain, 0, fields.contentDigest, nil),
				}
				f.conn.SetSignaturesForAddress(pda, []solana.Signature{{2}})
				f.conn.SetTransaction(solana.Signature{2}, &solacctconn.TransactionResult{
					Instructions: []solacctconn.Instruction{{ProgramID: foreignProgram(), Data: []byte{0x00}}},
				})
			},
			wantSearches:    1,
			wantAuditErrors: 1,
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
			assert.Len(t, f.conn.GetSignaturesForAddressCalls, tt.wantSearches)
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

func TestAuditSolanaProgramPendingAccountsBoundsAccountsRead(t *testing.T) {
	ctx := context.Background()
	f := newSolanaAuditFixture(t, ctx)
	f.conn.SetAccount(f.pending, f.pendingAccount(t, []uint8{0}))

	// Only the account past the bound needs a reobservation search.
	accounts := make([]solacctconn.ProgramAccount, maxSolanaProgramPendingAccountsPerAudit+1)
	for idx := range accounts {
		var addr solana.PublicKey
		binary.LittleEndian.PutUint32(addr[:4], uint32(idx)) // #nosec G115 -- idx < 10_001
		accounts[idx] = solanaProgramAccountFor(t, addr, vaa.ChainIDEthereum, 0, [32]byte{0x12}, []uint8{0})
	}
	accounts[maxSolanaProgramPendingAccountsPerAudit] = solanaProgramAccountFor(t, solana.PublicKey{0xFF}, vaa.ChainIDEthereum, 0, [32]byte{0x12}, nil)
	f.conn.ProgramAccounts = accounts

	f.acct.runSolanaAudit(ctx, f.acct.solana)
	assert.Empty(t, f.conn.GetSignaturesForAddressCalls)
}

func TestAuditSolanaProgramPendingAccountsBoundsReobservationSearches(t *testing.T) {
	ctx := context.Background()
	f := newSolanaAuditFixture(t, ctx)
	f.conn.SetAccount(f.pending, f.pendingAccount(t, []uint8{0}))

	accounts := make([]solacctconn.ProgramAccount, maxSolanaReobservationSearchesPerAudit+1)
	for idx := range accounts {
		var addr solana.PublicKey
		binary.LittleEndian.PutUint32(addr[:4], uint32(idx)) // #nosec G115 -- idx < 101
		accounts[idx] = solanaProgramAccountFor(t, addr, vaa.ChainIDEthereum, 0, [32]byte{0x34}, nil)
	}
	f.conn.ProgramAccounts = accounts

	f.acct.runSolanaAudit(ctx, f.acct.solana)
	assert.Len(t, f.conn.GetSignaturesForAddressCalls, maxSolanaReobservationSearchesPerAudit)
	assert.Empty(t, f.obsvReq)
}

func TestAuditSolanaOwnPendingTransfersBoundsCommitSearches(t *testing.T) {
	ctx := context.Background()
	f := newSolanaAuditFixture(t, ctx)

	// Every transfer is accounted and has no current guardian-set pending account at set index 0.
	// Thus each commit search reads one signature list.
	marked := make([]uint64, 0, maxSolanaCommitSearchesPerAudit+1)
	marked = append(marked, f.pe.solanaFields.Sequence)
	for idx := range maxSolanaCommitSearchesPerAudit {
		sequence := uint64(100 + idx) // #nosec G115 -- idx < 100
		_, err := f.acct.SubmitObservation(solanaTestTransfer(t, sequence))
		require.NoError(t, err)
		marked = append(marked, sequence)
	}
	// Sequences below 1024 share the fixture bucket.
	f.conn.SetAccount(f.bucket, &solacctconn.OwnedAccount{State: solacctconn.AccountInitialised, Data: solanaNoreplayBucket(t, marked...)})
	require.Len(t, f.acct.pendingTransfers, maxSolanaCommitSearchesPerAudit+1)

	f.acct.runSolanaAudit(ctx, f.acct.solana)
	assert.Len(t, f.conn.GetSignaturesForAddressCalls, maxSolanaCommitSearchesPerAudit)
	assert.Empty(t, f.acct.solana.subChan)
}
