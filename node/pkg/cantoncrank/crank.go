// Package cantoncrank implements the off-ledger "crank" that fulfils the
// EmitterRequest -> Emitter approval flow. The operator is the sole controller
// of EmitterRequest.ApproveEmitter, so nothing on-ledger advances a pending
// request; this service watches for pending requests and exercises the choice.
//
// Design follows the current Canton 3.5.x guidance (docs.canton.network,
// appdev/modules/m4-backend-dev): a backend ledger client that reads active
// contracts and submits commands over the Ledger API v2.
//
// The exactly-once CORRECTNESS foundation is the consuming ApproveEmitter choice
// plus Canton's conflict detection: ApproveEmitter archives the EmitterRequest,
// and two transactions can never both consume the same request, so a duplicate
// approval can never mint a second Emitter — regardless of how the crank is
// operated. Ledger API command deduplication (appdev/deep-dives/command-
// deduplication) is a defense-in-depth / efficiency layer on top: the dedup
// change id is (act-as, user id, command id), and we derive a *deterministic*
// command id from the request contract id (commandIDFor) so retries, crashes,
// and a redundant second crank instance collapse onto one submission before
// interpretation. Dedup saves wasted work; it is not the sole guarantee.
//
// This is a PoC sketch: the read/submit transport is expressed against the
// Reader/Submitter interfaces below so the crank loop and its unit tests are
// real today; the concrete gRPC Submitter (cantonclient.SubmitApproveEmitter)
// lands once the CommandService protos are vendored (see the task plan §7).
package cantoncrank

import (
	"context"
	"crypto/sha256"
	"encoding/hex"
	"errors"
	"time"

	"go.uber.org/zap"

	"github.com/certusone/wormhole/node/pkg/cantonclient"
)

// PendingRequest is one active EmitterRequest as seen in the ACS. It is the
// cantonclient domain type, aliased here for brevity; the type lives in
// cantonclient so the dependency direction stays one-way (cantoncrank ->
// cantonclient). It carries the template id echoed from the ACS CreatedEvent,
// which the approve exercise needs.
type PendingRequest = cantonclient.PendingEmitterRequest

// Reader lists active EmitterRequest contracts for the operator party. Backed by
// StateService.GetActiveContracts (cantonclient.grpcClient satisfies it).
type Reader interface {
	ActiveEmitterRequests(ctx context.Context, operator string) ([]PendingRequest, error)
}

// Submitter exercises ApproveEmitter on a single EmitterRequest. commandID MUST
// be used verbatim as the Ledger API command_id so deduplication keys on it; the
// request carries the template id the exercise names. Implementations set
// act_as=operator, a fixed user id, and a deduplication_duration. Backed by
// CommandService.SubmitAndWaitForTransaction.
type Submitter interface {
	SubmitApproveEmitter(ctx context.Context, operator string, req PendingRequest, commandID string) error
}

// ErrDuplicateCommand reports a terminal, benign outcome from the dedup layer:
// command deduplication rejected a resubmission of the same change id within the
// deduplication period (DUPLICATE_COMMAND). The approval already succeeded, so
// the crank treats it as success, not a retry. It is the same sentinel value the
// submitter returns (defined in cantonclient), so errors.Is matches across the
// package boundary.
var ErrDuplicateCommand = cantonclient.ErrDuplicateCommand

// ErrContractInactive reports a terminal, benign outcome from the consuming
// choice (the correctness backstop): the request contract is no longer active
// (CONTRACT_NOT_FOUND / CONTRACT_NOT_ACTIVE) because ApproveEmitter already
// archived it. A second approve can never mint a duplicate Emitter. Shared with
// cantonclient so errors.Is matches.
var ErrContractInactive = cantonclient.ErrContractInactive

// ErrSubmissionInFlight reports that a redundant submission of the same change id
// is in flight right now (SUBMISSION_ALREADY_IN_FLIGHT). Benign: the crank skips
// this tick and reconciles on the next one. Shared with cantonclient so
// errors.Is matches.
var ErrSubmissionInFlight = cantonclient.ErrSubmissionInFlight

// Policy decides whether a pending request should be approved. Production plugs
// in the real allowlist / governance gate here; compose the helpers below with
// All to pin an allowlisted package id and authorize requesters.
type Policy func(PendingRequest) bool

// ApproveAll approves every well-formed request.
//
// SANDBOX/PoC ONLY. This is a confused-deputy hazard in production: any party can
// create an EmitterRequest naming our operator, and ApproveAll would auto-approve
// it into a VAA-publishing Emitter. Production MUST pin an allowlisted package id
// (RequirePackageID) and authorize requesters (AllowRequesters), combined with
// All — and ultimately gate on the real approval authority (see the plan §10;
// the authority source — off-ledger ops config, on-ledger precondition, or
// governance VAA — remains an open decision, not invented here).
func ApproveAll(PendingRequest) bool { return true }

// DenyAll approves nothing. It is the fail-safe default when no policy is
// supplied to New: a misconfigured crank must not auto-approve emitters.
func DenyAll(PendingRequest) bool { return false }

// RequirePackageID returns a Policy that approves only requests whose template
// package id matches pkgID. Pinning the package id defends against a rogue party
// that publishes a look-alike EmitterRequest from a different Daml package.
func RequirePackageID(pkgID string) Policy {
	return func(r PendingRequest) bool { return r.TemplateID.PackageID == pkgID }
}

// AllowRequesters returns a Policy that approves only requests whose requester is
// in the allowlist. This authorizes WHO may be granted a VAA-publishing emitter.
func AllowRequesters(parties ...string) Policy {
	allowed := make(map[string]struct{}, len(parties))
	for _, p := range parties {
		allowed[p] = struct{}{}
	}
	return func(r PendingRequest) bool {
		_, ok := allowed[r.Requester]
		return ok
	}
}

// All composes policies with logical AND: the result approves a request only if
// every policy approves it. With no policies it approves everything (identity),
// so always combine it with at least RequirePackageID + AllowRequesters in
// production.
func All(ps ...Policy) Policy {
	return func(r PendingRequest) bool {
		for _, p := range ps {
			if !p(r) {
				return false
			}
		}
		return true
	}
}

// defaultSubmitTimeout bounds a single SubmitApproveEmitter call so one wedged
// submission cannot stall the tick indefinitely. A zero submitTimeout in New
// defaults to this.
const defaultSubmitTimeout = 30 * time.Second

// Crank polls the ledger and approves pending EmitterRequests.
type Crank struct {
	reader        Reader
	submit        Submitter
	operator      string
	policy        Policy
	interval      time.Duration
	submitTimeout time.Duration
	logger        *zap.Logger
}

// New builds a Crank. A zero interval defaults to 5s; a zero submitTimeout
// defaults to 30s. A nil logger becomes a no-op logger. A nil policy is
// fail-safe: it becomes DenyAll (never ApproveAll) with a warning, so a
// misconfigured crank approves nothing rather than every request.
func New(reader Reader, submit Submitter, operator string, policy Policy, interval, submitTimeout time.Duration, logger *zap.Logger) *Crank {
	if logger == nil {
		logger = zap.NewNop()
	}
	if policy == nil {
		logger.Warn("cantoncrank: no policy supplied; defaulting to DenyAll (approves nothing)")
		policy = DenyAll
	}
	if interval <= 0 {
		interval = 5 * time.Second
	}
	if submitTimeout <= 0 {
		submitTimeout = defaultSubmitTimeout
	}
	return &Crank{
		reader:        reader,
		submit:        submit,
		operator:      operator,
		policy:        policy,
		interval:      interval,
		submitTimeout: submitTimeout,
		logger:        logger,
	}
}

// commandIDFor derives the deduplication command id for a request. It is a pure
// function of the contract id, so every (re)submission for the same request —
// across ticks, restarts, or parallel crank instances — carries the same change
// id and the ledger collapses them to a single approval.
func commandIDFor(contractID string) string {
	sum := sha256.Sum256([]byte("emitter-approve:" + contractID))
	return "approve-" + hex.EncodeToString(sum[:16])
}

// Run polls until ctx is cancelled. Errors from a single tick are logged and the
// loop continues (transient failures retry on the next tick, backed by dedup).
// Shutdown is not a failure: a cancelled in-flight gRPC call surfaces as a status
// with code Canceled (which does NOT satisfy errors.Is(err, context.Canceled)),
// so the tick-failed warning is gated on the parent ctx state, not the error.
func (c *Crank) Run(ctx context.Context) error {
	ticker := time.NewTicker(c.interval)
	defer ticker.Stop()
	for {
		if err := c.tick(ctx); err != nil && ctx.Err() == nil {
			c.logger.Warn("crank tick failed", zap.Error(err))
		}
		select {
		case <-ctx.Done():
			return ctx.Err()
		case <-ticker.C:
		}
	}
}

// tick lists pending requests and approves each one that passes policy. Exported
// indirectly via Run; separated so unit tests can drive a single iteration.
func (c *Crank) tick(ctx context.Context) error {
	reqs, err := c.reader.ActiveEmitterRequests(ctx, c.operator)
	if err != nil {
		return err
	}
	for _, r := range reqs {
		// Defensive: ActiveEmitterRequests is scoped to the operator, but never
		// approve a request whose operator is not us.
		if r.Operator != c.operator {
			continue
		}
		if !c.policy(r) {
			c.logger.Debug("request rejected by policy", zap.String("cid", r.ContractID))
			continue
		}
		cmdID := commandIDFor(r.ContractID)
		// Bound each submission so one wedged call cannot stall the tick.
		submitCtx, cancel := context.WithTimeout(ctx, c.submitTimeout)
		err := c.submit.SubmitApproveEmitter(submitCtx, c.operator, r, cmdID)
		cancel()
		switch {
		case err == nil:
			c.logger.Info("approved emitter request",
				zap.String("cid", r.ContractID), zap.String("requester", r.Requester), zap.String("commandId", cmdID))
		case errors.Is(err, ErrDuplicateCommand):
			// Benign: dedup rejected a resubmission of the same change id. The
			// approval already succeeded. Nothing to retry.
			c.logger.Debug("request already approved (duplicate command)", zap.String("cid", r.ContractID))
		case errors.Is(err, ErrContractInactive):
			// Benign: the consuming choice already archived the request. Nothing to
			// retry. (Registry contention can also surface this; it self-heals — the
			// request stays in the ACS and the next tick retries.)
			c.logger.Debug("request already approved (contract inactive)", zap.String("cid", r.ContractID))
		case errors.Is(err, ErrSubmissionInFlight):
			// Benign: another instance is submitting this change id right now. The
			// request is still in the ACS, so the next tick reconciles it with the
			// same command id. Not a failure — do not surface.
			c.logger.Debug("approval already in flight; will reconcile next tick", zap.String("cid", r.ContractID))
		case errors.Is(err, context.Canceled) || ctx.Err() != nil:
			// Shutdown (parent ctx cancelled), not a failure. An in-flight submit
			// cancelled by shutdown surfaces as a gRPC status with code Canceled,
			// which does NOT satisfy errors.Is(err, context.Canceled); key on the
			// parent ctx state too so shutdown stays quiet. Stop this tick; remaining
			// requests are picked up when the crank next runs.
			return nil
		default:
			// Transient — log and let the next tick retry with the same command id.
			c.logger.Warn("approve submission failed; will retry",
				zap.String("cid", r.ContractID), zap.Error(err))
		}
	}
	return nil
}
