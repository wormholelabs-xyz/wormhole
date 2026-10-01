package accountant

import (
	"context"
	"encoding/base64"
	"encoding/binary"
	"errors"
	"fmt"
	"slices"
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

func foreignProgram() solana.PublicKey { return filledKey(0x33) }

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
			name:        "two commits",
			logs:        commitLogs(program, commit, other),
			wantCommits: []solanaCommitEvent{commit, other},
		},
		{
			name: "data at depth zero",
			logs: []string{programDataLine(payload)},
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
			wantCommits: slices.Repeat([]solanaCommitEvent{commit}, maxSolanaCommitEventsPerTx),
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
			name:        "commit at the maximum invoke depth",
			logs:        nestedCommitLogs(program, foreign, maxSolanaInvokeDepth, payload),
			wantCommits: []solanaCommitEvent{commit},
		},
		{
			name:        "rust fixture payload",
			logs:        []string{invokeLine(program, 1), programDataLine(mustHexDecode(t, fixtureACCDGSTLogHex)), successLine(program)},
			wantCommits: []solanaCommitEvent{newSolanaCommitEvent(vaa.ChainIDEthereum, fixtureEmitter(), 100_000, fixtureDigest(), 4)},
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

func TestProcessCommittedDigest(t *testing.T) {
	ctx := context.Background()
	contentDigest := func(pe *pendingEntry) [32]byte { return pe.solanaFields.contentDigest }
	otherDigest := func(*pendingEntry) [32]byte { return [32]byte{0xde, 0xad} }
	const ownMsgId = "own"

	tests := []struct {
		name          string
		msgId         string
		digest        func(pe *pendingEntry) [32]byte
		acceptContent bool
		wantPending   bool
	}{
		{name: "content digest not accepted", msgId: ownMsgId, digest: contentDigest},
		{name: "mismatch", msgId: ownMsgId, digest: otherDigest, acceptContent: true},
		{name: "unknown message id", msgId: "2/0000000000000000000000000290fb167208af455bb137780163b7b7a9a10c16/999", digest: contentDigest, wantPending: true},
		{name: "empty message id", msgId: "", digest: contentDigest, wantPending: true},
	}

	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			acct, _, msgChan := newSolanaTestAccountant(t, ctx, solanaTestOpts{wormchainContract: "0xdeadbeef", enforce: true})

			msg := solanaTestTransfer(t, 5)
			_, err := acct.SubmitObservation(msg)
			require.NoError(t, err)
			pe := acct.pendingTransfers[msg.MessageIDString()]
			require.NotNil(t, pe)
			require.NotNil(t, pe.solanaFields)

			msgId := tt.msgId
			if msgId == ownMsgId {
				msgId = msg.MessageIDString()
			}

			acct.pendingTransfersLock.Lock()
			approved := acct.processCommittedDigest(msgId, tt.digest(pe), tt.acceptContent, "test")
			acct.pendingTransfersLock.Unlock()

			assert.False(t, approved)
			assert.Empty(t, msgChan)
			if tt.wantPending {
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

	tests := []struct {
		name          string
		build         func(pe *pendingEntry) []string
		failed        bool
		wantPublished int
		wantPending   int
	}{
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

// startSolanaWatcher runs the watcher of acct.solana and returns its result channel.
func startSolanaWatcher(t *testing.T, ctx context.Context) (*Accountant, *MockAccountantSolanaConn, chan *common.MessagePublication, chan error) {
	t.Helper()
	acct, conn, msgChan := newSolanaTestAccountant(t, ctx, solanaTestOpts{enforce: true})
	errC := make(chan error, 1)
	go func() { errC <- acct.solanaWatcher(ctx, acct.solana) }()
	return acct, conn, msgChan, errC
}

func waitSolanaWatcher(t *testing.T, errC chan error) error {
	t.Helper()
	select {
	case err := <-errC:
		return err
	case <-time.After(5 * time.Second):
		t.Fatal("timed out waiting for the watcher to return")
		return nil
	}
}

func TestSolanaWatcherLoop(t *testing.T) {
	t.Run("publishes then returns on cancel", func(t *testing.T) {
		ctx, cancel := context.WithCancel(context.Background())
		defer cancel()

		acct, conn, msgChan, errC := startSolanaWatcher(t, ctx)
		msg := solanaTestTransfer(t, 21)
		_, err := acct.SubmitObservation(msg)
		require.NoError(t, err)
		pe := acct.pendingTransfers[msg.MessageIDString()]
		require.NotNil(t, pe)

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
		assert.ErrorIs(t, waitSolanaWatcher(t, errC), context.Canceled)
	})

	t.Run("subscription close returns an error", func(t *testing.T) {
		ctx, cancel := context.WithCancel(context.Background())
		defer cancel()

		_, conn, _, errC := startSolanaWatcher(t, ctx)
		conn.CloseLogEvents()
		err := waitSolanaWatcher(t, errC)
		require.Error(t, err)
		assert.NotErrorIs(t, err, context.Canceled)
		assert.Contains(t, err.Error(), "subscription closed")
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
