// This file is the Ledger API v2 WRITE path for the emitter-approval crank:
// SubmitApproveEmitter exercises the consuming ApproveEmitter choice on an
// EmitterRequest via CommandService.SubmitAndWaitForTransaction, and classifies
// the submission outcome into the crank's benign/transient taxonomy.
//
// It depends on the generated stubs (apiv2.Commands, apiv2.CommandServiceClient,
// apiv2.ExerciseCommand, ...) produced from the CommandService protos —
// command_service.proto, commands.proto, and reassignment_commands.proto — which
// are vendored under ./proto (extracted from the canton-open-source 3.5.1 jar,
// stubs committed under ./proto/gen; regenerate with `make generate-canton-proto`).
// The field set below was reconciled against those regenerated 3.5.1 stubs.
package cantonclient

import (
	"context"
	"strings"
	"time"

	"go.uber.org/zap"
	"google.golang.org/grpc/status"
	"google.golang.org/protobuf/types/known/durationpb"

	apiv2 "github.com/certusone/wormhole/node/pkg/cantonclient/proto/gen/com/daml/ledger/api/v2"
)

// crankUserID is the fixed Ledger API user id every crank submission runs under.
// It is one component of the deduplication change id (act_as, user_id,
// command_id). Command deduplication and the pinned user_id/act_as are
// defense-in-depth and an efficiency layer — they collapse redundant
// submissions before interpretation. They are NOT the exactly-once correctness
// foundation: that is the consuming ApproveEmitter choice plus Canton's conflict
// detection (two transactions cannot both consume the same EmitterRequest). Even
// so, keep this byte-identical across restarts and across redundant crank
// instances so dedup keys together as intended; never derive it per-instance or
// leave it defaulted. See the command-deduplication deep-dive and the task plan
// §3/§9.3.
//
// NOTE (auth): the dev sandbox is unauthenticated and accepts an arbitrary
// constant user id. A real participant requires a provisioned user with
// actAs(operator) rights; provision it (UserManagementService) or fall back to
// the participant default — but then the default must be identical across all
// crank instances (task plan §9.3, §10).
const crankUserID = "wormhole-crank"

// approveEmitterChoice is the consuming choice the crank exercises. Because it
// consumes the EmitterRequest, any duplicate exercise that gets past dedup
// targets an archived contract and fails at interpretation — the unconditional
// exactly-once backstop independent of command deduplication.
const approveEmitterChoice = "ApproveEmitter"

// defaultDeduplicationDuration is the default explicit dedup period for
// approvals (task plan §3). It must not exceed the participant's configured max
// deduplication duration, else the submission fails FAILED_PRECONDITION /
// INVALID_DEDUPLICATION_PERIOD. It seeds grpcClient.dedupDuration in
// NewCantonGrpcClient; an operator tunes the field below the participant max
// rather than editing this const (task plan §10).
const defaultDeduplicationDuration = time.Hour

// dedupTokenInvalidPeriod is the Canton error-id token for a deduplication period
// that exceeds the participant's configured maximum. It is classified opaquely
// (transient/retried) — but SubmitApproveEmitter logs it distinctly so an
// operator sees a misconfiguration rather than generic transient noise.
const dedupTokenInvalidPeriod = "INVALID_DEDUPLICATION_PERIOD"

// SubmitApproveEmitter exercises ApproveEmitter on req via
// CommandService.SubmitAndWaitForTransaction, acting as the operator. commandID
// is used verbatim as the Ledger API command_id so deduplication keys on it;
// together with the fixed crankUserID and act_as=[operator] it forms the change
// id. Errors are classified into the crank's taxonomy (ErrDuplicateCommand /
// ErrContractInactive / ErrSubmissionInFlight / transient) by classifySubmitError.
func (c *grpcClient) SubmitApproveEmitter(ctx context.Context, operator string, req PendingEmitterRequest, commandID string) error {
	cmd := apiv2.NewCommandServiceClient(c.conn)
	_, err := cmd.SubmitAndWaitForTransaction(ctx, &apiv2.SubmitAndWaitForTransactionRequest{
		Commands: buildApproveCommands(operator, req, commandID, c.dedupDuration),
		// TransactionFormat selects the returned transaction's shape/visibility.
		// ACS_DELTA scoped to the operator is sufficient — the crank does not read
		// the returned transaction, it only needs the submit to succeed. Verify at
		// gate time whether the 3.5.x request makes this field required; if so this
		// value satisfies it, if optional it is harmless.
		TransactionFormat: &apiv2.TransactionFormat{
			TransactionShape: apiv2.TransactionShape_TRANSACTION_SHAPE_ACS_DELTA,
			EventFormat:      acsEventFormat(operator),
		},
	})
	if err != nil {
		classified := classifySubmitError(err)
		// INVALID_DEDUPLICATION_PERIOD stays opaque/transient (the crank retries),
		// but it never self-heals: the participant max is fixed, so every retry
		// re-fails until dedupDuration is lowered. Surface it distinctly so an
		// operator recognizes a misconfiguration, not generic transient noise.
		if c.logger != nil && isDedupPeriodError(classified) {
			c.logger.Warn("canton: deduplication period rejected by participant; lower cantonclient dedupDuration below the participant's configured max deduplication duration",
				zap.Duration("dedupDuration", c.dedupDuration), zap.Error(classified))
		}
		return classified
	}
	return nil
}

// buildApproveCommands constructs the Commands message for one ApproveEmitter
// exercise. All three change-id components are pinned here: act_as=[operator],
// user_id=crankUserID, command_id=commandID (verbatim). The choice takes no
// parameters, so the argument is an empty record. The template id is echoed from
// the ACS CreatedEvent (req.TemplateID) — package-id-agnostic and upgrade-safe.
func buildApproveCommands(operator string, req PendingEmitterRequest, commandID string, dedupDuration time.Duration) *apiv2.Commands {
	return &apiv2.Commands{
		UserId:    crankUserID,
		CommandId: commandID,
		ActAs:     []string{operator},
		DeduplicationPeriod: &apiv2.Commands_DeduplicationDuration{
			DeduplicationDuration: durationpb.New(dedupDuration),
		},
		Commands: []*apiv2.Command{{
			Command: &apiv2.Command_Exercise{
				Exercise: &apiv2.ExerciseCommand{
					TemplateId: &apiv2.Identifier{
						PackageId:  req.TemplateID.PackageID,
						ModuleName: req.TemplateID.ModuleName,
						EntityName: req.TemplateID.EntityName,
					},
					ContractId: req.ContractID,
					Choice:     approveEmitterChoice,
					// Empty-record argument: ApproveEmitter has no parameters.
					ChoiceArgument: &apiv2.Value{Sum: &apiv2.Value_Record{Record: &apiv2.Record{}}},
				},
			},
		}},
	}
}

// classifySubmitError maps a CommandService submission error into the crank's
// outcome taxonomy (task plan §3). Classification is driven PRIMARILY by the
// Canton error-id TOKEN embedded in the status message — NOT by the bare gRPC
// status code. A single gRPC code (e.g. NotFound / AlreadyExists) is reused
// across many unrelated Canton error ids (USER_NOT_FOUND, TEMPLATE_NOT_FOUND,
// PACKAGE_NOT_FOUND, ...); keying on the bare code would misclassify those real
// participant faults as benign "already approved" and silently stall the queue.
//
//	DUPLICATE_COMMAND                            -> ErrDuplicateCommand (dedup hit; terminal-benign)
//	CONTRACT_NOT_FOUND / CONTRACT_NOT_ACTIVE     -> ErrContractInactive (consumed; terminal-benign)
//	SUBMISSION_ALREADY_IN_FLIGHT                 -> ErrSubmissionInFlight (benign skip; retry next tick)
//	everything else, INCLUDING a bare NotFound /
//	  AlreadyExists with no recognized token     -> the raw error (transient/opaque; retried)
//
// CONTRACT_NOT_ACTIVE can also surface transiently from registry contention
// (a concurrent transaction touched the EmitterRegistry key); that is
// benign-but-self-heals — the EmitterRequest stays in the ACS, so the next tick
// retries with the same command id.
//
// A bare NotFound/AlreadyExists with no recognized token is deliberately left
// opaque (transient/retried), not treated as benign — see the misclassification
// hazard above. FAILED_PRECONDITION / INVALID_DEDUPLICATION_PERIOD is likewise
// surfaced opaquely so the crank logs and retries; the operator must then lower
// grpcClient.dedupDuration below the participant max (SubmitApproveEmitter logs
// that case distinctly via isDedupPeriodError).
//
// TOKEN LOCATION (confirmed against Canton 3.5.1 via the integration test): the
// error-id token is present in the gRPC status *message*, e.g.
// "DUPLICATE_COMMAND(10,...): Command submission already exists." and
// "CONTRACT_NOT_FOUND(11,...): Contract could not be found with id ...". Canton
// additionally repeats the id in the status *details* (google.rpc.ErrorInfo),
// but matching on the message is sufficient and is what this function does.
func classifySubmitError(err error) error {
	if err == nil {
		return nil
	}
	st, ok := status.FromError(err)
	if !ok {
		return err // not a gRPC status — treat as transient/opaque
	}
	msg := st.Message()
	switch {
	case containsErrorID(msg, "DUPLICATE_COMMAND"):
		return ErrDuplicateCommand
	case containsErrorID(msg, "CONTRACT_NOT_FOUND"),
		containsErrorID(msg, "CONTRACT_NOT_ACTIVE"):
		return ErrContractInactive
	case containsErrorID(msg, "SUBMISSION_ALREADY_IN_FLIGHT"):
		return ErrSubmissionInFlight
	default:
		// Anything else — including a bare NotFound/AlreadyExists carrying no
		// recognized Canton error-id token — is transient/opaque and retried.
		return err
	}
}

// containsErrorID reports whether the status message carries a Canton
// self-service error id token. Canton renders every error id as
// "TOKEN(category,correlationId): message" (e.g.
// "DUPLICATE_COMMAND(10,abcd): ..."), so we require the token to be immediately
// followed by "(". Matching the bare token would let a hypothetical future id
// such as CONTRACT_NOT_FOUND_X over-match on the CONTRACT_NOT_FOUND prefix;
// anchoring on the "(" delimiter rules that out while staying robust to whatever
// context text surrounds the token.
func containsErrorID(msg, id string) bool {
	return strings.Contains(msg, id+"(")
}

// isDedupPeriodError reports whether err is the INVALID_DEDUPLICATION_PERIOD
// fault (dedup period exceeds the participant max). Used only to log that case
// distinctly; classification treats it as opaque/transient.
func isDedupPeriodError(err error) bool {
	st, ok := status.FromError(err)
	if !ok {
		return false
	}
	return containsErrorID(st.Message(), dedupTokenInvalidPeriod)
}
