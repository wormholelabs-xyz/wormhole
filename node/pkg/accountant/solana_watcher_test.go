package accountant

import (
	"context"
	"encoding/base64"
	"encoding/binary"
	"errors"
	"fmt"
	"testing"
	"time"

	"github.com/certusone/wormhole/node/pkg/common"
	"github.com/certusone/wormhole/node/pkg/solacctconn"
	"github.com/gagliardetto/solana-go"
	"github.com/stretchr/testify/assert"
	"github.com/stretchr/testify/require"
	"github.com/wormhole-foundation/wormhole/sdk/vaa"
)

// encodeAccountantDigestLog is the inverse of parseAccountantDigestLog.
func encodeAccountantDigestLog(evt solanaCommitEvent) []byte {
	out := make([]byte, accountantDigestLogLen)
	copy(out[:8], accountantDigestLogTag[:])
	binary.BigEndian.PutUint16(out[8:10], uint16(evt.Chain))
	copy(out[10:42], evt.Emitter[:])
	binary.BigEndian.PutUint64(out[42:50], evt.Sequence)
	copy(out[50:82], evt.Digest[:])
	binary.LittleEndian.PutUint32(out[82:86], evt.GuardianSetIndex)
	return out
}

func programDataLine(payload []byte) string {
	return solanaProgramDataPrefix + base64.StdEncoding.EncodeToString(payload)
}

func invokeLine(program solana.PublicKey, depth int) string {
	return fmt.Sprintf("Program %s invoke [%d]", program, depth)
}

func successLine(program solana.PublicKey) string {
	return fmt.Sprintf("Program %s success", program)
}

func failedLine(program solana.PublicKey) string {
	return fmt.Sprintf("Program %s failed: custom program error: 0x7", program)
}

func foreignProgram() solana.PublicKey {
	var pk [32]byte
	for i := range pk {
		pk[i] = 0x33
	}
	return pk
}

func newSolanaCommitEvent(chain vaa.ChainID, emitter vaa.Address, sequence uint64, digest [32]byte, gsIndex uint32) solanaCommitEvent {
	return solanaCommitEvent{Chain: chain, Emitter: emitter, Sequence: sequence, Digest: digest, GuardianSetIndex: gsIndex}
}

// commitLogs wraps commits in an invocation of program.
func commitLogs(program solana.PublicKey, commits ...solanaCommitEvent) []string {
	logs := []string{invokeLine(program, 1)}
	for _, commit := range commits {
		logs = append(logs, programDataLine(encodeAccountantDigestLog(commit)))
	}
	return append(logs, successLine(program))
}

func TestTransferKeyMatchesMessageID(t *testing.T) {
	key := TransferKey{EmitterChain: uint16(vaa.ChainIDEthereum), EmitterAddress: fixtureEmitter(), Sequence: 99}
	msg := common.MessagePublication{EmitterChain: vaa.ChainIDEthereum, EmitterAddress: fixtureEmitter(), Sequence: 99}
	assert.Equal(t, msg.MessageIDString(), key.String())
}

func TestParseSolanaCommitLogs(t *testing.T) {
	program := solanaTestProgram()
	foreign := foreignProgram()
	commit := newSolanaCommitEvent(vaa.ChainIDEthereum, fixtureEmitter(), 7, fixtureDigest(), 4)
	other := newSolanaCommitEvent(vaa.ChainIDSolana, fixtureEmitter(), 8, fixtureDigest(), 0)
	payload := encodeAccountantDigestLog(commit)

	overLineBound := make([]string, solacctconn.MaxLogLinesPerTx+1)
	for i := range overLineBound {
		overLineBound[i] = "Program log: noise"
	}

	overCommitBound := []string{invokeLine(program, 1)}
	for i := 0; i <= maxSolanaCommitEventsPerTx; i++ {
		overCommitBound = append(overCommitBound, programDataLine(payload))
	}
	overCommitBound = append(overCommitBound, successLine(program))

	overDepthBound := []string{}
	for i := 0; i <= maxSolanaInvokeDepth; i++ {
		overDepthBound = append(overDepthBound, invokeLine(foreign, i+1))
	}

	otherTag := make([]byte, accountantDigestLogLen)
	copy(otherTag[:8], []byte("OTHERTAG"))

	tests := []struct {
		name        string
		logs        []string
		wantCommits []solanaCommitEvent
		wantErr     bool
	}{
		{
			name:        "one commit",
			logs:        commitLogs(program, commit),
			wantCommits: []solanaCommitEvent{commit},
		},
		{
			name:        "two commits",
			logs:        commitLogs(program, commit, other),
			wantCommits: []solanaCommitEvent{commit, other},
		},
		{
			name: "only program log lines",
			logs: []string{invokeLine(program, 1), "Program log: Instruction: SubmitObservations", successLine(program)},
		},
		{name: "empty logs", logs: []string{}},
		{
			name: "foreign frame",
			logs: commitLogs(foreign, commit),
		},
		{
			name: "data at depth zero",
			logs: []string{programDataLine(payload)},
		},
		{
			name: "nested cpi, data in our frame",
			logs: []string{
				invokeLine(program, 1),
				invokeLine(foreign, 2),
				successLine(foreign),
				programDataLine(payload),
				successLine(program),
			},
			wantCommits: []solanaCommitEvent{commit},
		},
		{
			name: "nested cpi, data in child frame",
			logs: []string{
				invokeLine(program, 1),
				invokeLine(foreign, 2),
				programDataLine(payload),
				successLine(foreign),
				successLine(program),
			},
		},
		{
			name: "failed child pops the frame",
			logs: []string{
				invokeLine(program, 1),
				invokeLine(foreign, 2),
				failedLine(foreign),
				programDataLine(payload),
				successLine(program),
			},
			wantCommits: []solanaCommitEvent{commit},
		},
		{
			name: "compute unit and return lines ignored",
			logs: []string{
				invokeLine(program, 1),
				"Program log: Instruction: SubmitObservations",
				programDataLine(payload),
				"Program return: " + program.String() + " AQID",
				"Program consumption: 123456 units remaining",
				fmt.Sprintf("Program %s consumed 41234 of 200000 compute units", program),
				"Log truncated",
				successLine(program),
			},
			wantCommits: []solanaCommitEvent{commit},
		},
		{
			name: "other tag skipped",
			logs: []string{invokeLine(program, 1), programDataLine(otherTag), successLine(program)},
		},
		{
			name: "short payload skipped",
			logs: []string{invokeLine(program, 1), programDataLine([]byte{1, 2, 3}), successLine(program)},
		},
		{
			name:    "accdgst one byte short",
			logs:    []string{invokeLine(program, 1), programDataLine(payload[:accountantDigestLogLen-1]), successLine(program)},
			wantErr: true,
		},
		{
			name:    "base64 garbage",
			logs:    []string{invokeLine(program, 1), solanaProgramDataPrefix + "!!!!", successLine(program)},
			wantErr: true,
		},
		{
			name:    "two fields on the data line",
			logs:    []string{invokeLine(program, 1), programDataLine(payload) + " extra", successLine(program)},
			wantErr: true,
		},
		{
			name: "malformed then valid",
			logs: []string{
				invokeLine(program, 1),
				programDataLine(payload[:accountantDigestLogLen-1]),
				programDataLine(payload),
				successLine(program),
			},
			wantCommits: []solanaCommitEvent{commit},
			wantErr:     true,
		},
		{
			name:        "over the commit bound",
			logs:        overCommitBound,
			wantCommits: repeatCommit(commit, maxSolanaCommitEventsPerTx),
			wantErr:     true,
		},
		{
			name:    "over the line bound",
			logs:    overLineBound,
			wantErr: true,
		},
		{
			name:    "over the depth bound",
			logs:    overDepthBound,
			wantErr: true,
		},
		{
			name:    "success with an empty stack",
			logs:    []string{successLine(program)},
			wantErr: true,
		},
		{
			name:    "unparsable program id",
			logs:    []string{"Program notvalid0OIl invoke [1]"},
			wantErr: true,
		},
		{
			name: "program log success in a child frame cannot forge a commit",
			logs: []string{
				invokeLine(program, 1),
				invokeLine(foreign, 2),
				"Program log: success",
				programDataLine(payload),
				successLine(foreign),
				successLine(program),
			},
		},
		{
			name: "program log success under our frame keeps the commit",
			logs: []string{
				invokeLine(program, 1),
				"Program log: success",
				programDataLine(payload),
				successLine(program),
			},
			wantCommits: []solanaCommitEvent{commit},
		},
		{
			name:    "success id differs from the innermost frame",
			logs:    []string{invokeLine(program, 1), successLine(foreign), programDataLine(payload), successLine(program)},
			wantErr: true,
		},
		{
			name:    "invoke height skips a level",
			logs:    []string{invokeLine(program, 2), programDataLine(payload), successLine(program)},
			wantErr: true,
		},
		{
			name:        "commit at the SIMD-0268 invoke height of 9",
			logs:        nestedCommitLogs(program, foreign, 9, payload),
			wantCommits: []solanaCommitEvent{commit},
		},
		{
			name:    "success with an unparsable id",
			logs:    []string{invokeLine(program, 1), "Program notvalid0OIl success"},
			wantErr: true,
		},
	}

	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			commits, err := parseSolanaCommitLogs(tt.logs, program)
			if tt.wantErr {
				require.Error(t, err)
			} else {
				require.NoError(t, err)
			}
			assert.Equal(t, len(tt.wantCommits), len(commits))
			for i := range tt.wantCommits {
				assert.Equal(t, tt.wantCommits[i], commits[i])
			}
		})
	}
}

// nestedCommitLogs emits payload from program at invoke height depth, under depth-1 foreign frames.
func nestedCommitLogs(program, foreign solana.PublicKey, depth int, payload []byte) []string {
	logs := make([]string, 0, 2*depth+1)
	for height := 1; height < depth; height++ {
		logs = append(logs, invokeLine(foreign, height))
	}
	logs = append(logs, invokeLine(program, depth), programDataLine(payload), successLine(program))
	for height := 1; height < depth; height++ {
		logs = append(logs, successLine(foreign))
	}
	return logs
}

func repeatCommit(commit solanaCommitEvent, n int) []solanaCommitEvent {
	out := make([]solanaCommitEvent, n)
	for i := range out {
		out[i] = commit
	}
	return out
}

func TestProcessCommittedDigest(t *testing.T) {
	ctx := context.Background()
	otherDigest := [32]byte{0xde, 0xad}

	tests := []struct {
		name            string
		enforce         bool
		acceptContent   bool
		unknownMsgId    bool
		emptyMsgId      bool
		useContent      bool
		useOtherDigest  bool
		wantApproved    bool
		wantPublished   int
		wantStillPendng bool
	}{
		{name: "vaa digest while enforcing", enforce: true, wantApproved: true, wantPublished: 1},
		{name: "vaa digest in log only mode", wantApproved: true},
		{name: "content digest accepted", enforce: true, acceptContent: true, useContent: true, wantApproved: true, wantPublished: 1},
		{name: "content digest not accepted", enforce: true, useContent: true},
		{name: "mismatch", enforce: true, acceptContent: true, useOtherDigest: true},
		{name: "unknown message id", enforce: true, unknownMsgId: true, wantStillPendng: true},
		{name: "empty message id", enforce: true, emptyMsgId: true, wantStillPendng: true},
	}

	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			acct, _, msgChan := newSolanaTestAccountant(t, ctx, solanaTestOpts{wormchainContract: "0xdeadbeef", enforce: tt.enforce})

			msg := solanaTestTransfer(t, 5)
			_, err := acct.SubmitObservation(msg)
			require.NoError(t, err)
			msgId := msg.MessageIDString()

			pe := acct.pendingTransfers[msgId]
			require.NotNil(t, pe)
			require.NotNil(t, pe.solanaFields)

			got := pe.vaaDigest
			if tt.useContent {
				got = pe.solanaFields.contentDigest
			}
			if tt.useOtherDigest {
				got = otherDigest
			}

			lookupId := msgId
			if tt.unknownMsgId {
				lookupId = "2/0000000000000000000000000290fb167208af455bb137780163b7b7a9a10c16/999"
			}
			if tt.emptyMsgId {
				lookupId = ""
			}

			acct.pendingTransfersLock.Lock()
			approved := acct.processCommittedDigest(lookupId, got, tt.acceptContent, "test")
			acct.pendingTransfersLock.Unlock()

			assert.Equal(t, tt.wantApproved, approved)
			assert.Equal(t, tt.wantPublished, len(msgChan))
			if tt.wantStillPendng {
				assert.Len(t, acct.pendingTransfers, 1)
			} else {
				assert.Empty(t, acct.pendingTransfers)
			}
		})
	}
}

func TestHandleSolanaLogEvent(t *testing.T) {
	ctx := context.Background()
	program := solanaTestProgram()
	foreign := foreignProgram()

	tests := []struct {
		name          string
		build         func(pe *pendingEntry) []string
		failed        bool
		wantPublished int
		wantPending   int
	}{
		{
			name: "content digest approves",
			build: func(pe *pendingEntry) []string {
				return commitLogs(program, newSolanaCommitEvent(pe.msg.EmitterChain, pe.msg.EmitterAddress, pe.msg.Sequence, pe.solanaFields.contentDigest, 0))
			},
			wantPublished: 1,
		},
		{
			name: "vaa digest approves",
			build: func(pe *pendingEntry) []string {
				vaaDigest, err := digestBytes(pe.digest)
				require.NoError(t, err)
				return commitLogs(program, newSolanaCommitEvent(pe.msg.EmitterChain, pe.msg.EmitterAddress, pe.msg.Sequence, vaaDigest, 6))
			},
			wantPublished: 1,
		},
		{
			name: "failed transaction",
			build: func(pe *pendingEntry) []string {
				return commitLogs(program, newSolanaCommitEvent(pe.msg.EmitterChain, pe.msg.EmitterAddress, pe.msg.Sequence, pe.solanaFields.contentDigest, 0))
			},
			failed:      true,
			wantPending: 1,
		},
		{
			name: "forged commit from a foreign frame",
			build: func(pe *pendingEntry) []string {
				return commitLogs(foreign, newSolanaCommitEvent(pe.msg.EmitterChain, pe.msg.EmitterAddress, pe.msg.Sequence, pe.solanaFields.contentDigest, 0))
			},
			wantPending: 1,
		},
		{
			name: "malformed line beside a valid commit",
			build: func(pe *pendingEntry) []string {
				commit := newSolanaCommitEvent(pe.msg.EmitterChain, pe.msg.EmitterAddress, pe.msg.Sequence, pe.solanaFields.contentDigest, 0)
				return []string{
					invokeLine(program, 1),
					solanaProgramDataPrefix + "!!!!",
					programDataLine(encodeAccountantDigestLog(commit)),
					successLine(program),
				}
			},
			wantPublished: 1,
		},
	}

	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			acct, _, msgChan := newSolanaTestAccountant(t, ctx, solanaTestOpts{enforce: true})

			msg := solanaTestTransfer(t, 11)
			_, err := acct.SubmitObservation(msg)
			require.NoError(t, err)
			pe := acct.pendingTransfers[msg.MessageIDString()]
			require.NotNil(t, pe)

			acct.handleSolanaLogEvent(solacctconn.LogEvent{
				Signature: solana.Signature{1},
				Logs:      tt.build(pe),
				Failed:    tt.failed,
			}, acct.solana)

			assert.Equal(t, tt.wantPublished, len(msgChan))
			assert.Equal(t, tt.wantPending, len(acct.pendingTransfers))
		})
	}
}

func TestSolanaWatcherLoop(t *testing.T) {
	t.Run("publishes then returns on cancel", func(t *testing.T) {
		ctx, cancel := context.WithCancel(context.Background())
		defer cancel()

		acct, conn, msgChan := newSolanaTestAccountant(t, ctx, solanaTestOpts{enforce: true})
		msg := solanaTestTransfer(t, 21)
		_, err := acct.SubmitObservation(msg)
		require.NoError(t, err)
		pe := acct.pendingTransfers[msg.MessageIDString()]
		require.NotNil(t, pe)

		errC := make(chan error, 1)
		go func() { errC <- acct.solanaWatcher(ctx, acct.solana) }()

		conn.PushLogEvent(solacctconn.LogEvent{
			Signature: solana.Signature{2},
			Logs:      commitLogs(solanaTestProgram(), newSolanaCommitEvent(msg.EmitterChain, msg.EmitterAddress, msg.Sequence, pe.solanaFields.contentDigest, 0)),
		})

		select {
		case published := <-msgChan:
			assert.Equal(t, msg.MessageIDString(), published.MessageIDString())
		case <-time.After(5 * time.Second):
			t.Fatal("timed out waiting for the watcher to publish")
		}

		cancel()
		select {
		case err := <-errC:
			assert.ErrorIs(t, err, context.Canceled)
		case <-time.After(5 * time.Second):
			t.Fatal("timed out waiting for the watcher to return")
		}
	})

	t.Run("subscription close returns an error", func(t *testing.T) {
		ctx, cancel := context.WithCancel(context.Background())
		defer cancel()

		acct, conn, _ := newSolanaTestAccountant(t, ctx, solanaTestOpts{enforce: true})
		errC := make(chan error, 1)
		go func() { errC <- acct.solanaWatcher(ctx, acct.solana) }()

		conn.CloseLogEvents()
		select {
		case err := <-errC:
			require.Error(t, err)
			assert.NotErrorIs(t, err, context.Canceled)
			assert.Contains(t, err.Error(), "subscription closed")
		case <-time.After(5 * time.Second):
			t.Fatal("timed out waiting for the watcher to return")
		}
	})

	t.Run("cancel wins over a closed subscription", func(t *testing.T) {
		ctx, cancel := context.WithCancel(context.Background())
		acct, conn, _ := newSolanaTestAccountant(t, ctx, solanaTestOpts{enforce: true})
		cancel()
		conn.CloseLogEvents()
		// select picks randomly among ready cases.
		for range 32 {
			require.ErrorIs(t, acct.solanaWatcher(ctx, acct.solana), context.Canceled)
		}
	})

	t.Run("subscribe failure is wrapped", func(t *testing.T) {
		ctx := context.Background()
		acct, conn, _ := newSolanaTestAccountant(t, ctx, solanaTestOpts{enforce: true})
		want := errors.New("dial failed")
		conn.SetSubscribeLogsErr(want)

		err := acct.solanaWatcher(ctx, acct.solana)
		require.ErrorIs(t, err, want)
	})
}

func TestParseSolanaCommitLogsRustFixture(t *testing.T) {
	line := programDataLine(mustHexDecode(t, fixtureACCDGSTLogHex))
	commits, err := parseSolanaCommitLogs([]string{invokeLine(solanaTestProgram(), 1), line, successLine(solanaTestProgram())}, solanaTestProgram())
	require.NoError(t, err)
	require.Len(t, commits, 1)
	assert.Equal(t, fixtureDigest(), commits[0].Digest)
}
