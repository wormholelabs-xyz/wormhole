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

// Field positions in "Program <id> <verb> [<depth>]". The verb line is the shortest one
// the parser reads. An invoke line carries the depth.
const (
	programLineKeywordField = 0
	programLineIDField      = 1
	programLineVerbField    = 2
	programLineDepthField   = 3
	programLineMinFields    = programLineVerbField + 1
	programInvokeLineFields = programLineDepthField + 1
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

// requestSolanaAudit asks the audit goroutine for one Solana audit. A request already queued
// covers this one.
func (acct *Accountant) requestSolanaAudit() {
	select {
	case acct.solanaAuditRequests <- struct{}{}:
	default:
	}
}

// solanaWatcher subscribes to the accountant program's logs, requests one audit, and drains
// the logs. It returns on context cancellation, on a subscribe failure, and when the
// subscription closes, so the supervisor restarts it.
func (acct *Accountant) solanaWatcher(ctx context.Context, b *solanaBackend) error {
	acct.logger.Info("acctwatch: creating solana watcher", zap.String("backend", b.tag), zap.Stringer("program", b.program))

	events, err := b.conn.SubscribeLogs(ctx, b.program)
	if err != nil {
		solanaConnectionErrors.Inc()
		return fmt.Errorf("failed to subscribe to %s logs: %w", b.tag, err)
	}
	// SECURITY: the live path misses commits that finalized while no subscription was open.
	// The audit recovers them.
	acct.requestSolanaAudit()

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
// SECURITY: a `failed:` line in any frame rejects the transaction with nil commits. The
// runtime fails the whole transaction when a frame fails, so a successful transaction with a
// failed frame is malformed.
//
// The parser joins a malformed commit under the frame of program into err. It still returns
// the well-formed commits.
func parseSolanaCommitLogs(logs []string, program solana.PublicKey) ([]solanaCommitEvent, error) {
	commits := make([]solanaCommitEvent, 0, 1)
	failed := false
	rejectFailed := func(idx int, frame solana.PublicKey) error {
		failed = true
		return fmt.Errorf("log line %d: frame of %s failed", idx, frame)
	}
	err := walkSolanaProgramData(logs, program, rejectFailed, func(idx int, rest string) (bool, error) {
		evt, err := parseSolanaProgramDataCommit(rest)
		if err != nil {
			return false, fmt.Errorf("log line %d: %w", idx, err)
		}
		if evt == nil {
			return false, nil
		}
		if len(commits) == maxSolanaCommitEventsPerTx {
			return true, fmt.Errorf("log line %d: more than %d commits in one transaction", idx, maxSolanaCommitEventsPerTx)
		}
		commits = append(commits, *evt)
		return false, nil
	})
	if failed {
		return nil, err
	}
	return commits, err
}

// parseSolanaPayerLog returns the recorded payer of pendingPDA from the logs of a
// submit_observations that failed with PayerMismatch.
//
// SECURITY: the logs also hold the ACCDGST line of the rolled-back commit. Do not pass them
// to parseSolanaCommitLogs.
// SECURITY: postcondition: exactly one ACCPAYR entry from the frame of program, for
// pendingPDA, with a non-zero payer. Any other shape is an error.
func parseSolanaPayerLog(logs []string, program, pendingPDA solana.PublicKey) (solana.PublicKey, error) {
	var found *solanaPayerLog
	// The accountant frame of a PayerMismatch ends in `failed:`.
	err := walkSolanaProgramData(logs, program, nil, func(idx int, rest string) (bool, error) {
		entry, err := parseSolanaProgramDataPayer(rest)
		if err != nil {
			return true, fmt.Errorf("log line %d: %w", idx, err)
		}
		if entry == nil {
			return false, nil
		}
		if found != nil {
			return true, fmt.Errorf("log line %d: more than one payer log in one transaction", idx)
		}
		found = entry
		return false, nil
	})
	if err != nil {
		return solana.PublicKey{}, err
	}
	if found == nil {
		return solana.PublicKey{}, errors.New("payer log: no entry")
	}
	if found.PendingPDA != pendingPDA {
		return solana.PublicKey{}, fmt.Errorf("payer log: pending PDA %s, want %s", found.PendingPDA, pendingPDA)
	}
	if found.RecordedPayer.IsZero() {
		return solana.PublicKey{}, errors.New("payer log: zero recorded payer")
	}
	return found.RecordedPayer, nil
}

// walkSolanaProgramData calls visit with the payload of each `Program data:` line that
// program emitted, in log order, until visit returns stop. It joins the errors of visit.
// A non-nil onFailed runs on each `failed:` line before the frame pops. An error from it ends
// the walk.
//
// SECURITY: a logsSubscribe mentions filter returns every transaction that references the
// program. Thus a foreign program in the same transaction can emit a byte-perfect data line.
// A `Program data:` line counts only while program is the innermost frame. The walk skips
// lines with program-controlled text before it reads the frame keywords.
//
// A broken invoke stack ends the walk, because frames after that cannot be attributed.
func walkSolanaProgramData(logs []string, program solana.PublicKey, onFailed func(idx int, frame solana.PublicKey) error, visit func(idx int, rest string) (stop bool, err error)) error {
	if len(logs) > solacctconn.MaxLogLinesPerTx {
		return fmt.Errorf("transaction logs: %d lines is past the %d line limit", len(logs), solacctconn.MaxLogLinesPerTx)
	}

	var stack solanaInvokeStack
	var errs []error

	for idx, line := range logs {
		if rest, found := strings.CutPrefix(line, solanaProgramDataPrefix); found {
			if !stack.innermostIs(program) {
				continue
			}
			stop, err := visit(idx, rest)
			if err != nil {
				errs = append(errs, err)
			}
			if stop {
				return errors.Join(errs...)
			}
			continue
		}

		if isSolanaProgramOutputLine(line) {
			continue
		}

		fields := strings.Fields(line)
		if len(fields) < programLineMinFields || fields[programLineKeywordField] != "Program" {
			continue
		}
		verb := fields[programLineVerbField]
		if verb != "invoke" && verb != "success" && verb != "failed:" {
			continue
		}

		frame, err := solana.PublicKeyFromBase58(fields[programLineIDField])
		if err != nil {
			errs = append(errs, fmt.Errorf("log line %d: %q program id %q: %w", idx, verb, fields[programLineIDField], err))
			return errors.Join(errs...)
		}
		if verb == "invoke" {
			if len(fields) != programInvokeLineFields {
				err = fmt.Errorf("invoke line has %d fields, want %d", len(fields), programInvokeLineFields)
			} else {
				err = stack.push(frame, fields[programLineDepthField])
			}
		} else {
			if verb == "failed:" && onFailed != nil {
				if err := onFailed(idx, frame); err != nil {
					errs = append(errs, err)
					return errors.Join(errs...)
				}
			}
			err = stack.pop(frame)
		}
		if err != nil {
			errs = append(errs, fmt.Errorf("log line %d: %w", idx, err))
			return errors.Join(errs...)
		}
	}

	return errors.Join(errs...)
}

// decodeSolanaProgramData decodes the base64 payload of a `Program data:` line.
func decodeSolanaProgramData(rest string) ([]byte, error) {
	fields := strings.Fields(rest)
	if len(fields) != 1 {
		return nil, fmt.Errorf("program data: want 1 field, got %d", len(fields))
	}
	payload, err := base64.StdEncoding.DecodeString(fields[0])
	if err != nil {
		return nil, fmt.Errorf("program data: %w", err)
	}
	return payload, nil
}

// hasSolanaLogTag reports whether payload starts with tag.
func hasSolanaLogTag(payload []byte, tag [8]byte) bool {
	return len(payload) >= len(tag) && [8]byte(payload[:len(tag)]) == tag
}

// parseSolanaProgramDataCommit decodes the payload of a `Program data:` line from the frame
// of the accountant program. A payload without the ACCDGST tag gives a nil event.
func parseSolanaProgramDataCommit(rest string) (*solanaCommitEvent, error) {
	payload, err := decodeSolanaProgramData(rest)
	if err != nil {
		return nil, err
	}
	if !hasSolanaLogTag(payload, accountantDigestLogTag) {
		return nil, nil
	}
	return parseAccountantDigestLog(payload)
}

// parseSolanaProgramDataPayer decodes the payload of a `Program data:` line from the frame
// of the accountant program. A payload without the ACCPAYR tag gives a nil entry.
func parseSolanaProgramDataPayer(rest string) (*solanaPayerLog, error) {
	payload, err := decodeSolanaProgramData(rest)
	if err != nil {
		return nil, err
	}
	if !hasSolanaLogTag(payload, accountantPayerLogTag) {
		return nil, nil
	}
	return parseAccountantPayerLog(payload)
}
