// Watcher for the svm/accountant program. It subscribes to the program's transaction logs,
// decodes each ACCDGST commit, and releases the matching pending transfer.

package accountant

import (
	"context"
	"encoding/base64"
	"errors"
	"fmt"
	"strings"

	"github.com/certusone/wormhole/node/pkg/solacctconn"
	"github.com/gagliardetto/solana-go"

	"go.uber.org/zap"
)

const (
	solanaProgramDataPrefix = "Program data: "

	maxSolanaCommitEventsPerTx = 64

	// Agave max invoke stack height is 5, and 9 under SIMD-0268.
	maxSolanaInvokeDepth = 16
)

// Runtime lines whose second field is a keyword, not a program id. `Program log: success`
// must not pop a frame. Thus the parser skips these lines before it reads the frame keywords.
var solanaProgramOutputPrefixes = [...]string{
	"Program log: ",
	"Program return: ",
	"Program consumption: ",
}

func isSolanaProgramOutputLine(line string) bool {
	for _, prefix := range solanaProgramOutputPrefixes {
		if strings.HasPrefix(line, prefix) {
			return true
		}
	}
	return false
}

// solanaBaseWatcher is the entry point for the Token Bridge Solana watcher.
func (acct *Accountant) solanaBaseWatcher(ctx context.Context) error {
	if acct.solana == nil {
		return errors.New("acctwatch: the solana backend is not configured")
	}
	return acct.solanaWatcher(ctx, acct.solana)
}

// solanaWatcher subscribes to the accountant program's logs and drains them. It returns on
// context cancellation, on a subscribe failure, and when the subscription closes, so the
// supervisor restarts it.
func (acct *Accountant) solanaWatcher(ctx context.Context, b *solanaBackend) error {
	acct.logger.Info("acctwatch: creating solana watcher", zap.String("backend", b.tag), zap.Stringer("program", b.program))

	events, err := b.conn.SubscribeLogs(ctx, b.program)
	if err != nil {
		solanaConnectionErrors.Inc()
		return fmt.Errorf("failed to subscribe to %s logs: %w", b.tag, err)
	}

	for {
		select {
		case <-ctx.Done():
			return ctx.Err()
		case evt, ok := <-events:
			if !ok {
				// The subscription also closes on cancel.
				if err := ctx.Err(); err != nil {
					return err
				}
				solanaConnectionErrors.Inc()
				return fmt.Errorf("%s log subscription closed", b.tag)
			}
			acct.handleSolanaLogEvent(evt, b)
		}
	}
}

// handleSolanaLogEvent processes the logs of one transaction.
func (acct *Accountant) handleSolanaLogEvent(evt solacctconn.LogEvent, b *solanaBackend) {
	solanaEventsReceived.Inc()

	if evt.Failed {
		solanaFailedTxSkipped.Inc()
		acct.logger.Debug("acctwatch: skipping failed solana transaction", zap.String("backend", b.tag), zap.Stringer("signature", evt.Signature))
		return
	}

	commits, err := parseSolanaCommitLogs(evt.Logs, b.program)
	if err != nil {
		solanaMalformedLogs.Inc()
		acct.logger.Error("acctwatch: failed to parse solana transaction logs", zap.String("backend", b.tag), zap.Stringer("signature", evt.Signature), zap.Error(err))
	}

	for idx := range commits {
		acct.processSolanaCommitEvent(&commits[idx], evt.Signature, b.tag)
	}
}

// processSolanaCommitEvent applies one ACCDGST commit to the pending transfer map.
//
// SECURITY: precondition: evt comes from parseSolanaCommitLogs over a successful,
// finalized transaction.
func (acct *Accountant) processSolanaCommitEvent(evt *solanaCommitEvent, sig solana.Signature, tag string) {
	msgId := TransferKey{EmitterChain: uint16(evt.Chain), EmitterAddress: evt.Emitter, Sequence: evt.Sequence}.String()

	acct.logger.Debug("acctwatch: solana commit detected",
		zap.String("backend", tag),
		zap.String("msgId", msgId),
		zap.Stringer("signature", sig),
		zap.Uint32("guardianSetIndex", evt.GuardianSetIndex),
	)

	acct.pendingTransfersLock.Lock()
	defer acct.pendingTransfersLock.Unlock()

	// submit_observations commits the content digest. submit_vaas and the backfill commit
	// the VAA digest. Either digest releases the transfer.
	if acct.processCommittedDigest(msgId, evt.Digest, true, tag) {
		solanaTransfersApproved.Inc()
	}
}

// solanaInvokeStack mirrors the runtime instruction stack from the invoke, success and
// failed lines of one transaction.
type solanaInvokeStack struct {
	frames [maxSolanaInvokeDepth]solana.PublicKey
	depth  int
}

func (s *solanaInvokeStack) innermostIs(program solana.PublicKey) bool {
	return s.depth > 0 && s.frames[s.depth-1] == program
}

// push opens the frame of `Program <program> invoke [<height>]`.
//
// SECURITY: height must be exactly one above the current depth.
func (s *solanaInvokeStack) push(program solana.PublicKey, height string) error {
	if s.depth == maxSolanaInvokeDepth {
		return fmt.Errorf("invoke depth is past the limit of %d", maxSolanaInvokeDepth)
	}
	want := fmt.Sprintf("[%d]", s.depth+1)
	if height != want {
		return fmt.Errorf("invoke height %q for %s, want %q", height, program, want)
	}
	s.frames[s.depth] = program
	s.depth++
	return nil
}

// pop closes the frame of a `success` or `failed:` line.
//
// SECURITY: program must be the innermost frame.
func (s *solanaInvokeStack) pop(program solana.PublicKey) error {
	if s.depth == 0 {
		return fmt.Errorf("frame end for %s with an empty invoke stack", program)
	}
	if innermost := s.frames[s.depth-1]; program != innermost {
		return fmt.Errorf("frame end for %s, innermost frame is %s", program, innermost)
	}
	s.depth--
	return nil
}

// parseSolanaCommitLogs returns the ACCDGST commits program emitted, in log order.
//
// SECURITY: a logsSubscribe mentions filter returns every transaction that references the
// program. Thus a foreign program in the same transaction can emit a byte-perfect commit line.
// A `Program data:` line counts only while program is the innermost frame. The parser skips
// lines with program-controlled text before it reads the frame keywords.
//
// The parser joins a malformed commit under the frame of program into err. It still returns
// the well-formed commits. A broken invoke stack ends the scan, because the parser cannot
// attribute frames after that.
func parseSolanaCommitLogs(logs []string, program solana.PublicKey) ([]solanaCommitEvent, error) {
	if len(logs) > solacctconn.MaxLogLinesPerTx {
		return nil, fmt.Errorf("transaction logs: %d lines is past the %d line limit", len(logs), solacctconn.MaxLogLinesPerTx)
	}

	var stack solanaInvokeStack
	commits := make([]solanaCommitEvent, 0, 1)
	var errs []error

	for idx, line := range logs {
		if rest, found := strings.CutPrefix(line, solanaProgramDataPrefix); found {
			if !stack.innermostIs(program) {
				continue
			}
			evt, err := parseSolanaProgramDataCommit(rest)
			if err != nil {
				errs = append(errs, fmt.Errorf("log line %d: %w", idx, err))
				continue
			}
			if evt == nil {
				continue
			}
			if len(commits) == maxSolanaCommitEventsPerTx {
				errs = append(errs, fmt.Errorf("log line %d: more than %d commits in one transaction", idx, maxSolanaCommitEventsPerTx))
				return commits, errors.Join(errs...)
			}
			commits = append(commits, *evt)
			continue
		}

		if isSolanaProgramOutputLine(line) {
			continue
		}

		// "Program <id> <verb> ..." is the shortest program line the parser reads.
		fields := strings.Fields(line)
		if len(fields) < 3 || fields[0] != "Program" {
			continue
		}
		verb := fields[2]
		if verb != "invoke" && verb != "success" && verb != "failed:" {
			continue
		}

		frame, err := solana.PublicKeyFromBase58(fields[1])
		if err != nil {
			errs = append(errs, fmt.Errorf("log line %d: %q program id %q: %w", idx, verb, fields[1], err))
			return commits, errors.Join(errs...)
		}
		if verb == "invoke" {
			if len(fields) != 4 {
				err = fmt.Errorf("invoke line has %d fields, want 4", len(fields))
			} else {
				err = stack.push(frame, fields[3])
			}
		} else {
			err = stack.pop(frame)
		}
		if err != nil {
			errs = append(errs, fmt.Errorf("log line %d: %w", idx, err))
			return commits, errors.Join(errs...)
		}
	}

	return commits, errors.Join(errs...)
}

// parseSolanaProgramDataCommit decodes the payload of a `Program data:` line from the frame
// of the accountant program. A payload without the ACCDGST tag gives a nil event.
func parseSolanaProgramDataCommit(rest string) (*solanaCommitEvent, error) {
	fields := strings.Fields(rest)
	if len(fields) != 1 {
		return nil, fmt.Errorf("program data: want 1 field, got %d", len(fields))
	}

	payload, err := base64.StdEncoding.DecodeString(fields[0])
	if err != nil {
		return nil, fmt.Errorf("program data: %w", err)
	}
	tagLen := len(accountantDigestLogTag)
	if len(payload) < tagLen {
		return nil, nil
	}
	if [8]byte(payload[:tagLen]) != accountantDigestLogTag {
		return nil, nil
	}

	return parseAccountantDigestLog(payload)
}
