package accountant

import (
	"bytes"
	"context"
	"encoding/hex"
	"errors"
	"testing"
	"time"

	"github.com/certusone/wormhole/node/pkg/common"
	"github.com/certusone/wormhole/node/pkg/solacctconn"
	ethCommon "github.com/ethereum/go-ethereum/common"
	ethCrypto "github.com/ethereum/go-ethereum/crypto"
	"github.com/gagliardetto/solana-go"
	"github.com/stretchr/testify/assert"
	"github.com/stretchr/testify/require"
	"github.com/wormhole-foundation/wormhole/sdk/vaa"
	"go.uber.org/zap"
	"go.uber.org/zap/zaptest/observer"
)

// mustSolanaTxID builds a solanaTxID from hex.
func mustSolanaTxID(t *testing.T, s string) solanaTxID {
	t.Helper()
	txID, err := newSolanaTxID(mustHexDecode(t, s))
	require.NoError(t, err)
	return txID
}

// fixtureTransferFields is the decoded 143-byte record of the transfer fixture.
func fixtureTransferFields(t *testing.T) *solanaObservationFields {
	t.Helper()
	fields, err := unpackObservationFields(mustHexDecode(t, fixtureTransferFieldsHex))
	require.NoError(t, err)
	return &fields
}

func TestEncodeSubmitObservationsIxDataRoundTrip(t *testing.T) {
	for _, fixture := range []string{fixtureSubmitObservationsIxDataHex, fixtureSubmitObservationsSignatureTxIDIxDataHex} {
		want := mustHexDecode(t, fixture)
		parsed, err := parseSubmitObservationsIxData(want)
		require.NoError(t, err)

		got, err := encodeSubmitObservationsIxData(parsed.GuardianSetIndex, parsed.GuardianIndex, parsed.Signature[:], parsed.TxID, &parsed.solanaObservationFields)
		require.NoError(t, err)
		require.Len(t, got, submitObservationsInstructionLen)
		assert.Equal(t, want, got)
	}
}

func TestEncodeSubmitObservationsIxDataRejects(t *testing.T) {
	fields := fixtureTransferFields(t)

	txID := mustSolanaTxID(t, fixtureSubmitObservationsTxIDHex)

	tests := []struct {
		name      string
		signature []byte
		txID      solanaTxID
		fields    *solanaObservationFields
	}{
		{name: "short signature", signature: make([]byte, submitSignatureLen-1), txID: txID, fields: fields},
		{name: "no tx id", signature: make([]byte, submitSignatureLen), fields: fields},
		{name: "no fields", signature: make([]byte, submitSignatureLen), txID: txID},
	}

	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			got, err := encodeSubmitObservationsIxData(1, 2, tt.signature, tt.txID, tt.fields)
			require.Error(t, err)
			assert.Nil(t, got)
		})
	}
}

// TestSolanaObservationSigningDigest pins the Go digest to the program's own.
func TestSolanaObservationSigningDigest(t *testing.T) {
	fields := fixtureTransferFields(t)
	txID := mustSolanaTxID(t, fixtureSubmitObservationsTxIDHex)

	tests := []struct {
		name       string
		txID       solanaTxID
		wantDigest string
	}{
		{name: "32-byte tx id", txID: txID, wantDigest: fixtureTransferSigningDigestHex},
		{name: "64-byte tx id", txID: mustSolanaTxID(t, fixtureSubmitObservationsSignatureTxIDHex), wantDigest: fixtureTransferSignatureTxIDSigningDigestHex},
	}
	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			digest, err := solanaObservationSigningDigest(SubmitObservationPrefix, tt.txID, fields)
			require.NoError(t, err)
			assert.Equal(t, tt.wantDigest, hex.EncodeToString(digest.Bytes()))
		})
	}

	_, err := solanaObservationSigningDigest(SubmitObservationPrefix, txID, nil)
	require.Error(t, err)
	_, err = solanaObservationSigningDigest(SubmitObservationPrefix, solanaTxID{}, fields)
	require.Error(t, err)
}

// setSolanaConfirmPollInterval sets the confirmation poll interval for one test.
func setSolanaConfirmPollInterval(t *testing.T, interval time.Duration) {
	t.Helper()
	previous := solanaConfirmPollInterval
	solanaConfirmPollInterval = interval
	t.Cleanup(func() { solanaConfirmPollInterval = previous })
}

// customTxError is a program error as the conn reports it.
func customTxError(code uint32) *solacctconn.TxError {
	return &solacctconn.TxError{CustomCode: code, HasCustomCode: true}
}

func TestClassifySolanaTxError(t *testing.T) {
	tests := []struct {
		name            string
		txErr           error
		wantDisposition solanaTxDisposition
		// wantReason, when set, is a substring of the reason.
		wantReason string
	}{
		{name: "not the first instruction", txErr: customTxError(solanaErrInstructionNotFirst), wantDisposition: solanaTxFailed, wantReason: "transaction build defect"},
		{name: "cpi invocation", txErr: customTxError(solanaErrCpiInvocation), wantDisposition: solanaTxFailed, wantReason: "transaction build defect"},
		{name: "already signed", txErr: customTxError(solanaErrAlreadySigned), wantDisposition: solanaTxAlreadyDone},
		{name: "payer mismatch", txErr: customTxError(solanaErrPayerMismatch), wantDisposition: solanaTxRetryNextRound},
		{name: "invalid signature", txErr: customTxError(solanaErrInvalidSignature), wantDisposition: solanaTxFailed},
		{name: "unmapped custom code", txErr: customTxError(29), wantDisposition: solanaTxFailed},
		{name: "blockhash not found", txErr: &solacctconn.TxError{Kind: solacctconn.TxErrBlockhashNotFound}, wantDisposition: solanaTxRetryNextRound},
		{name: "already processed", txErr: &solacctconn.TxError{Kind: solacctconn.TxErrAlreadyProcessed}, wantDisposition: solanaTxRetryNextRound},
		{name: "insufficient funds for fee", txErr: &solacctconn.TxError{Kind: solacctconn.TxErrInsufficientFundsForFee}, wantDisposition: solanaTxFeePayerCannotPay},
		{name: "other transaction error", txErr: &solacctconn.TxError{Kind: solacctconn.TxErrOther}, wantDisposition: solanaTxFailed},
		{name: "transport error", txErr: errors.New("connection reset"), wantDisposition: solanaTxFailed},
	}

	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			disposition, reason := classifySolanaTxError(tt.txErr)
			assert.Equal(t, tt.wantDisposition, disposition)
			assert.NotEmpty(t, reason)
			assert.Contains(t, reason, tt.wantReason)
		})
	}
}

// solanaPendingAccountData builds a PendingObservationsLayout account for a submission.
func solanaPendingAccountData(t *testing.T, chain vaa.ChainID, guardianSetIndex uint32, contentDigest [32]byte, payer solana.PublicKey, signedBy []uint8) []byte {
	t.Helper()
	return solanaPendingAccountDataWithTxID(t, chain, guardianSetIndex, contentDigest, payer, solanaTestTxID(t), signedBy)
}

// solanaTestTxID is the tx id of solanaTestTransfer.
func solanaTestTxID(t *testing.T) solanaTxID {
	t.Helper()
	return mustSolanaTxIDBytes(t, solanaTestTransfer(t, 0).TxID)
}

func mustSolanaTxIDBytes(t *testing.T, id []byte) solanaTxID {
	t.Helper()
	txID, err := newSolanaTxID(id)
	require.NoError(t, err)
	return txID
}

func solanaPendingAccountDataWithTxID(t *testing.T, chain vaa.ChainID, guardianSetIndex uint32, contentDigest [32]byte, payer solana.PublicKey, txID solanaTxID, signedBy []uint8) []byte {
	t.Helper()
	require.True(t, txID.valid())
	wire := pendingObservationsWire{
		Tag:              pendingObservationsTag,
		TxIDLen:          txID.length,
		Chain:            uint16(chain),
		GuardianSetIndex: guardianSetIndex,
		ContentDigest:    contentDigest,
		Payer:            payer,
		TxID:             txID.padded,
	}
	for _, index := range signedBy {
		wire.Signatures[index/pendingObservationsBitsPerWord] |= 1 << (index % pendingObservationsBitsPerWord)
	}
	data, err := encodeWire(&wire)
	require.NoError(t, err)
	return data
}

// solanaBatchFixture is one accountant with a single pending transfer queued for Solana.
type solanaBatchFixture struct {
	acct *Accountant
	conn *MockAccountantSolanaConn
	msg  *common.MessagePublication
	pe   *pendingEntry
	sub  *solanaSubmission
}

func newSolanaBatchFixture(t *testing.T, ctx context.Context) *solanaBatchFixture {
	t.Helper()

	acct, conn, _ := newSolanaTestAccountant(t, ctx, solanaTestOpts{enforce: true})
	setSolanaConfirmPollInterval(t, 0)
	// One message per batch, so the reader returns without waiting out batchTimeout.
	acct.submitObservationBatchSize = 1

	msg := solanaTestTransfer(t, 77)
	_, err := acct.SubmitObservation(msg)
	require.NoError(t, err)
	pe := acct.pendingTransfers[msg.MessageIDString()]

	sub, err := acct.solana.deriveSolanaSubmission(0, msg, pe.solanaFields)
	require.NoError(t, err)

	conn.LatestBlockhash = solacctconn.Blockhash{Hash: solana.Hash{9}, LastValidBlockHeight: 1000}
	conn.BlockHeight = 900
	conn.DefaultSignatureStatus = &solacctconn.SignatureStatus{Confirmed: true}

	return &solanaBatchFixture{acct: acct, conn: conn, msg: msg, pe: pe, sub: sub}
}

// queue puts the message on the Solana submission channel with the pending flag set.
func (f *solanaBatchFixture) queue(t *testing.T) {
	t.Helper()
	f.pe.setSubmitPending(backendSolana, true)
	f.acct.solana.subChan <- f.msg
}

func TestDeriveSolanaSubmission(t *testing.T) {
	ctx := context.Background()
	acct, _, _ := newSolanaTestAccountant(t, ctx, solanaTestOpts{enforce: true})
	b := acct.solana

	msg := solanaTestTransfer(t, 5)
	_, err := acct.SubmitObservation(msg)
	require.NoError(t, err)
	fields := acct.pendingTransfers[msg.MessageIDString()].solanaFields
	require.NotNil(t, fields)

	t.Run("transfer derives both balance accounts", func(t *testing.T) {
		sub, err := b.deriveSolanaSubmission(3, msg, fields)
		require.NoError(t, err)

		wantPending, err := derivePendingObservationsPDA(b.program, fields.Chain, fields.Emitter, fields.Sequence, 3, fields.contentDigest, mustSolanaTxIDBytes(t, msg.TxID))
		require.NoError(t, err)
		assert.Equal(t, wantPending, sub.pendingPDA)

		wantSource, err := deriveBalanceAccountPDA(b.program, fields.Chain, fields.TokenChain, fields.TokenAddress)
		require.NoError(t, err)
		wantDest, err := deriveBalanceAccountPDA(b.program, fields.RecipientChain, fields.TokenChain, fields.TokenAddress)
		require.NoError(t, err)
		assert.Equal(t, wantSource, sub.sourceBalance)
		assert.Equal(t, wantDest, sub.destBalance)
		assert.NotEqual(t, sub.sourceBalance, sub.destBalance)

		wantBucket, err := deriveNoreplayBucketPDA(b.noreplay, b.authority, fields.Chain, fields.Emitter, fields.Sequence)
		require.NoError(t, err)
		assert.Equal(t, wantBucket, sub.noreplayBucket)
	})

	t.Run("tx id lengths", func(t *testing.T) {
		tests := []struct {
			name    string
			length  int
			wantErr bool
		}{
			{name: "32 bytes", length: hashTxIDLen},
			{name: "64 bytes", length: signatureTxIDLen},
			{name: "33 bytes", length: hashTxIDLen + 1, wantErr: true},
		}
		for _, tt := range tests {
			t.Run(tt.name, func(t *testing.T) {
				withTxID := *msg
				withTxID.TxID = bytes.Repeat([]byte{0x5C}, tt.length)
				sub, err := b.deriveSolanaSubmission(0, &withTxID, fields)
				if tt.wantErr {
					require.Error(t, err)
					assert.Nil(t, sub)
					return
				}
				require.NoError(t, err)
				assert.Equal(t, withTxID.TxID, sub.txID.Bytes())
			})
		}
	})

	t.Run("a stale content digest is rejected", func(t *testing.T) {
		stale := *fields
		stale.Action = 0x02
		sub, err := b.deriveSolanaSubmission(0, msg, &stale)
		require.Error(t, err)
		assert.Nil(t, sub)
	})

	t.Run("no fields is rejected", func(t *testing.T) {
		sub, err := b.deriveSolanaSubmission(0, msg, nil)
		require.Error(t, err)
		assert.Nil(t, sub)
	})
}

func TestBuildSolanaSubmitTx(t *testing.T) {
	ctx := context.Background()
	f := newSolanaBatchFixture(t, ctx)
	b := f.acct.solana
	f.sub.rentRecipient = solana.PublicKey{0xAB}
	guardianSet, err := deriveGuardianSetPDA(b.coreBridge, 0)
	require.NoError(t, err)
	guardian := solanaGuardianIdentity{guardianSetPDA: guardianSet}

	t.Run("v1 transaction with the accountant instruction alone", func(t *testing.T) {
		tx, err := f.acct.buildSolanaSubmitTx(ctx, b, guardian, f.sub, solana.Hash{7})
		require.NoError(t, err)
		require.Equal(t, solana.MessageVersionV1, tx.Message.GetVersion())
		// The program requires instruction index 0.
		require.Len(t, tx.Message.Instructions, 1)
		require.Len(t, tx.Signatures, 1)

		config := tx.Message.TransactionConfig
		require.NotNil(t, config.ComputeUnitLimit)
		assert.Equal(t, uint32(solanaSubmitComputeUnitLimit), *config.ComputeUnitLimit)
		require.NotNil(t, config.LoadedAccountsDataSizeLimit)
		assert.Equal(t, uint32(solanaSubmitLoadedAccountsDataSizeLimit), *config.LoadedAccountsDataSizeLimit)
		require.NotNil(t, config.PriorityFee)
		assert.Equal(t, uint64(0), *config.PriorityFee)

		wire, err := tx.MarshalBinary()
		require.NoError(t, err)
		assert.Equal(t, byte(0x81), wire[0], "v1 version prefix")
		assert.LessOrEqual(t, len(wire), solana.MaxTransactionSizeV1)
		decoded, err := solana.TransactionFromBytes(wire)
		require.NoError(t, err)
		assert.Equal(t, solana.MessageVersionV1, decoded.Message.GetVersion())
		require.NoError(t, decoded.VerifySignatures())

		program, err := tx.Message.Program(tx.Message.Instructions[0].ProgramIDIndex)
		require.NoError(t, err)
		assert.Equal(t, b.program, program)

		parsed, err := parseSubmitObservationsIxData(tx.Message.Instructions[0].Data)
		require.NoError(t, err)
		// secp256k1_recover takes a recovery id of 0 or 1, not 27 or 28.
		assert.Contains(t, []uint8{0, 1}, parsed.Signature[64])

		// The program recovers the guardian from the signature over its own digest.
		digest, err := solanaObservationSigningDigest(b.prefix, f.sub.txID, f.sub.record)
		require.NoError(t, err)
		pub, err := ethCrypto.SigToPub(digest.Bytes(), parsed.Signature[:])
		require.NoError(t, err)
		assert.Equal(t, f.acct.guardianAddr, ethCrypto.PubkeyToAddress(*pub))

		// Account list of submit_observations.rs.
		want := [submitObservationsAccountCount]struct {
			key      solana.PublicKey
			writable bool
			signer   bool
		}{
			{b.feePayer.PublicKey(), true, true},
			{f.sub.pendingPDA, true, false},
			{guardianSet, false, false},
			{f.sub.noreplayBucket, true, false},
			{solana.SystemProgramID, false, false},
			{b.noreplay, false, false},
			{b.authority, false, false},
			{f.sub.sourceBalance, true, false},
			{f.sub.destBalance, true, false},
			{f.sub.rentRecipient, true, false},
			{f.sub.chainRegistrationPDA, false, false},
			{solana.SysVarInstructionsPubkey, false, false},
		}
		accounts, err := tx.Message.Instructions[0].ResolveInstructionAccounts(&tx.Message)
		require.NoError(t, err)
		require.Len(t, accounts, submitObservationsAccountCount)
		for idx, w := range want {
			assert.Equal(t, w.key, accounts[idx].PublicKey, "account %d", idx)
			assert.Equal(t, w.writable, accounts[idx].IsWritable, "account %d writable", idx)
			assert.Equal(t, w.signer, accounts[idx].IsSigner, "account %d signer", idx)
		}
	})

	t.Run("priority fee goes in the v1 config", func(t *testing.T) {
		b.priorityFee = 25
		t.Cleanup(func() { b.priorityFee = 0 })

		tx, err := f.acct.buildSolanaSubmitTx(ctx, b, guardian, f.sub, solana.Hash{7})
		require.NoError(t, err)
		require.Len(t, tx.Message.Instructions, 1)
		require.NotNil(t, tx.Message.TransactionConfig.PriorityFee)
		assert.Equal(t, uint64(25), *tx.Message.TransactionConfig.PriorityFee)
	})
}

// TestHandleSolanaBatch drives one queued observation through each worker outcome.
func TestHandleSolanaBatch(t *testing.T) {
	tests := []struct {
		name string
		// setup runs after the fixture build and before the batch handler.
		setup func(t *testing.T, f *solanaBatchFixture)
		// deleteEntry drops the pending entry before the batch handler runs.
		deleteEntry bool
		noGuardian  bool
		wantSent    int
		wantPending int
	}{
		{
			name: "fresh pending pda confirms",
			setup: func(t *testing.T, f *solanaBatchFixture) {
				f.conn.SetAccount(f.sub.pendingPDA, nil)
			},
			wantSent:    1,
			wantPending: 1,
		},
		{
			name: "prefunded pending pda confirms",
			setup: func(t *testing.T, f *solanaBatchFixture) {
				f.conn.SetAccount(f.sub.pendingPDA, &solacctconn.OwnedAccount{State: solacctconn.AccountUninitialised})
			},
			wantSent:    1,
			wantPending: 1,
		},
		{
			name: "own bit set sends nothing",
			setup: func(t *testing.T, f *solanaBatchFixture) {
				f.conn.SetAccount(f.sub.pendingPDA, &solacctconn.OwnedAccount{
					State: solacctconn.AccountInitialised,
					Data:  solanaPendingAccountData(t, f.sub.wttFields(t).Chain, 0, f.sub.wttFields(t).contentDigest, f.acct.solana.feePayer.PublicKey(), []uint8{0}),
				})
			},
			wantPending: 1,
		},
		{
			name: "pending account with the wrong digest is skipped",
			setup: func(t *testing.T, f *solanaBatchFixture) {
				f.conn.SetAccount(f.sub.pendingPDA, &solacctconn.OwnedAccount{
					State: solacctconn.AccountInitialised,
					Data:  solanaPendingAccountData(t, f.sub.wttFields(t).Chain, 0, [32]byte{0xEE}, solana.PublicKey{0xAB}, nil),
				})
			},
			wantPending: 1,
		},
		{
			name: "account read failure abandons the batch",
			setup: func(t *testing.T, f *solanaBatchFixture) {
				f.conn.GetOwnedAccountsErr = errors.New("rpc down")
			},
			wantPending: 1,
		},
		{
			name: "blockhash failure abandons the batch",
			setup: func(t *testing.T, f *solanaBatchFixture) {
				f.conn.LatestBlockhashErr = errors.New("no blockhash")
			},
			wantPending: 1,
		},
		{
			name: "transport error on send",
			setup: func(t *testing.T, f *solanaBatchFixture) {
				f.conn.SendTransactionErr = errors.New("connection reset")
			},
			wantSent:    1,
			wantPending: 1,
		},
		{
			name: "status poll failure abandons the batch",
			setup: func(t *testing.T, f *solanaBatchFixture) {
				f.conn.SignatureStatusesErr = errors.New("status rpc down")
			},
			wantSent:    1,
			wantPending: 1,
		},
		{
			name: "confirm payer mismatch retries once",
			setup: func(t *testing.T, f *solanaBatchFixture) {
				f.conn.DefaultSignatureStatus = &solacctconn.SignatureStatus{Err: customTxError(solanaErrPayerMismatch)}
			},
			wantSent:    maxSolanaSubmitRounds,
			wantPending: 1,
		},
		{
			name: "confirm program failure",
			setup: func(t *testing.T, f *solanaBatchFixture) {
				f.conn.DefaultSignatureStatus = &solacctconn.SignatureStatus{Err: customTxError(solanaErrInvalidSignature)}
			},
			wantSent:    1,
			wantPending: 1,
		},
		{
			name:        "entry deleted before the batch runs",
			deleteEntry: true,
		},
		{
			name:        "guardian not in the set",
			noGuardian:  true,
			wantPending: 1,
		},
	}

	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			ctx, cancel := context.WithTimeout(context.Background(), 30*time.Second)
			defer cancel()

			f := newSolanaBatchFixture(t, ctx)
			if tt.noGuardian {
				f.acct.gst.Set(&common.GuardianSet{Index: 0, Keys: []ethCommon.Address{ethCommon.HexToAddress("0x0000000000000000000000000000000000000001")}})
			}
			if tt.setup != nil {
				tt.setup(t, f)
			}

			f.queue(t)
			if tt.deleteEntry {
				f.acct.deletePendingTransfer(f.msg.MessageIDString())
			}

			require.NoError(t, f.acct.handleSolanaBatch(ctx, f.acct.solana))

			assert.Len(t, f.conn.SentTransactions, tt.wantSent)
			assert.Len(t, f.acct.pendingTransfers, tt.wantPending)
			if pe, exists := f.acct.pendingTransfers[f.msg.MessageIDString()]; exists {
				assert.False(t, pe.submitPending(backendSolana), "submitPending must be clear after the batch")
			}
		})
	}
}

// TestHandleSolanaBatchPayerMismatchRetrySucceeds sends once with the fee payer, then
// again after the pending PDA appears with another payer recorded. The PDA is visible only
// at confirmed commitment, the same as when the retry follows a preflight rejection.
func TestHandleSolanaBatchPayerMismatchRetrySucceeds(t *testing.T) {
	ctx, cancel := context.WithTimeout(context.Background(), 30*time.Second)
	defer cancel()

	f := newSolanaBatchFixture(t, ctx)
	recordedPayer := solana.PublicKey{0xCD}

	sends := 0
	f.conn.SetSendTransactionHook(func(tx *solana.Transaction) error {
		sends++
		if sends == 1 {
			f.conn.SetConfirmedAccount(f.sub.pendingPDA, &solacctconn.OwnedAccount{
				State: solacctconn.AccountInitialised,
				Data:  solanaPendingAccountData(t, f.sub.wttFields(t).Chain, 0, f.sub.wttFields(t).contentDigest, recordedPayer, []uint8{1}),
			})
			return customTxError(solanaErrPayerMismatch)
		}
		return nil
	})
	f.conn.DefaultSignatureStatus = &solacctconn.SignatureStatus{Confirmed: true}

	f.queue(t)
	require.NoError(t, f.acct.handleSolanaBatch(ctx, f.acct.solana))

	require.Len(t, f.conn.SentTransactions, 2)
	second := f.conn.SentTransactions[1]
	accounts, err := second.Message.Instructions[0].ResolveInstructionAccounts(&second.Message)
	require.NoError(t, err)
	require.Len(t, accounts, submitObservationsAccountCount)
	assert.Equal(t, recordedPayer, accounts[9].PublicKey)
}

// TestHandleSolanaBatchPayerMismatchUsesLoggedPayer takes the recorded payer from the
// ACCPAYR entry of the preflight logs. The mock reports the pending PDA as absent, so a
// round-two read would pick the fee payer instead.
func TestHandleSolanaBatchPayerMismatchUsesLoggedPayer(t *testing.T) {
	ctx, cancel := context.WithTimeout(context.Background(), 30*time.Second)
	defer cancel()

	f := newSolanaBatchFixture(t, ctx)
	program := f.acct.solana.program
	recordedPayer := solana.PublicKey{0xCD}

	sends := 0
	f.conn.SetSendTransactionHook(func(tx *solana.Transaction) error {
		sends++
		if sends == 1 {
			txErr := customTxError(solanaErrPayerMismatch)
			txErr.Logs = []string{
				invokeLine(program, 1),
				programDataLine(encodeAccountantPayerLog(f.sub.pendingPDA, recordedPayer)),
				failedLine(program),
			}
			return txErr
		}
		return nil
	})

	f.queue(t)
	require.NoError(t, f.acct.handleSolanaBatch(ctx, f.acct.solana))

	require.Len(t, f.conn.SentTransactions, 2)
	second := f.conn.SentTransactions[1]
	accounts, err := second.Message.Instructions[0].ResolveInstructionAccounts(&second.Message)
	require.NoError(t, err)
	require.Len(t, accounts, submitObservationsAccountCount)
	assert.Equal(t, recordedPayer, accounts[9].PublicKey)
	assert.Len(t, f.conn.GetOwnedAccountsCalls, 1, "only round one reads the pending PDA")
}

// TestConfirmSolanaSubmissions covers the block height expiry and context cancellation.
func TestConfirmSolanaSubmissions(t *testing.T) {
	tests := []struct {
		name string
		// setup runs after the fixture build and before the confirmation.
		setup         func(f *solanaBatchFixture)
		cancel        bool
		wantSubmitted int
		wantDropped   int
	}{
		{
			// Lands between the height read that passes expiry and the next status read.
			name: "lands just before expiry",
			setup: func(f *solanaBatchFixture) {
				f.conn.DefaultSignatureStatus = nil
				f.conn.BlockHeight = 2000
				f.conn.SetBlockHeightHook(func() {
					f.conn.DefaultSignatureStatus = &solacctconn.SignatureStatus{Confirmed: true}
				})
			},
			wantSubmitted: 1,
		},
		{
			name: "processed past expiry confirms",
			setup: func(f *solanaBatchFixture) {
				f.conn.DefaultSignatureStatus = &solacctconn.SignatureStatus{Confirmed: false}
				f.conn.BlockHeight = 2000
				heightReads := 0
				f.conn.SetBlockHeightHook(func() {
					heightReads++
					if heightReads == 2 {
						f.conn.DefaultSignatureStatus = &solacctconn.SignatureStatus{Confirmed: true}
					}
				})
			},
			wantSubmitted: 1,
		},
		{
			name: "unseen past expiry is dropped",
			setup: func(f *solanaBatchFixture) {
				f.conn.DefaultSignatureStatus = nil
				f.conn.BlockHeight = 2000
			},
			wantDropped: 1,
		},
		{
			name:   "context cancelled",
			cancel: true,
		},
	}

	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			ctx, cancel := context.WithTimeout(context.Background(), 30*time.Second)
			defer cancel()

			f := newSolanaBatchFixture(t, ctx)
			core, logs := observer.New(zap.InfoLevel)
			f.acct.logger = zap.New(core)
			if tt.setup != nil {
				tt.setup(f)
			}
			if tt.cancel {
				cancel()
			}

			f.sub.txSignature = solana.Signature{1}
			f.sub.lastValidBlockHeight = 1000
			retry := f.acct.confirmSolanaSubmissions(ctx, f.acct.solana, []*solanaSubmission{f.sub})

			assert.Empty(t, retry)
			assert.Equal(t, tt.wantSubmitted, logs.FilterMessage("submitted an observation to the solana accountant").Len())
			assert.Equal(t, tt.wantDropped, logs.FilterMessage("a solana observation was dropped, the audit will retry").Len())
			if tt.cancel {
				assert.Empty(t, f.conn.GetSignatureStatusesCalls)
			}
		})
	}
}

// wttFields is the WTT record of s.
func (s *solanaSubmission) wttFields(t *testing.T) *solanaObservationFields {
	t.Helper()
	fields, ok := s.record.(*solanaObservationFields)
	require.True(t, ok, "record is %T", s.record)
	return fields
}
