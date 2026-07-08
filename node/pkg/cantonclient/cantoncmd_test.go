// Unit tests for the write path — see cantoncmd.go. Run with:
//
//	go test ./pkg/cantonclient -run 'TestClassifySubmitError|TestBuildApproveCommands' -v
package cantonclient

import (
	"errors"
	"testing"
	"time"

	"github.com/stretchr/testify/assert"
	"github.com/stretchr/testify/require"
	"google.golang.org/grpc/codes"
	"google.golang.org/grpc/status"

	apiv2 "github.com/certusone/wormhole/node/pkg/cantonclient/proto/gen/com/daml/ledger/api/v2"
)

func TestClassifySubmitError(t *testing.T) {
	cases := []struct {
		name string
		in   error
		want error // sentinel to errors.Is against; nil means "opaque/transient (neither sentinel)"
	}{
		{"nil is nil", nil, nil},
		// A bare ALREADY_EXISTS with no recognized token is opaque/transient, NOT
		// benign — the code alone is reused across unrelated faults.
		{"bare ALREADY_EXISTS code is opaque", status.Error(codes.AlreadyExists, "the command has already been submitted"), nil},
		{"DUPLICATE_COMMAND id", status.Error(codes.AlreadyExists, "DUPLICATE_COMMAND(10,abcd): A command with the given command id has already been successfully processed"), ErrDuplicateCommand},
		// A bare NOT_FOUND with no recognized token is opaque/transient — a real
		// USER_NOT_FOUND / TEMPLATE_NOT_FOUND must not read as "already approved".
		{"bare NOT_FOUND code is opaque", status.Error(codes.NotFound, "contract could not be found"), nil},
		{"USER_NOT_FOUND id is opaque", status.Error(codes.NotFound, "USER_NOT_FOUND(11,abcd): getting user failed for unknown user"), nil},
		{"CONTRACT_NOT_FOUND id", status.Error(codes.NotFound, "CONTRACT_NOT_FOUND(11,abcd): Contract could not be found with id ..."), ErrContractInactive},
		{"CONTRACT_NOT_ACTIVE id", status.Error(codes.FailedPrecondition, "CONTRACT_NOT_ACTIVE(9,abcd): The contract ... has already been archived"), ErrContractInactive},
		{"SUBMISSION_ALREADY_IN_FLIGHT id", status.Error(codes.Aborted, "SUBMISSION_ALREADY_IN_FLIGHT(9,abcd): A command with the given change id is already in flight"), ErrSubmissionInFlight},
		{"UNAVAILABLE is transient", status.Error(codes.Unavailable, "the participant is unavailable"), nil},
		{"DEADLINE_EXCEEDED is transient", status.Error(codes.DeadlineExceeded, "deadline exceeded"), nil},
		{"INVALID_DEDUPLICATION_PERIOD is opaque", status.Error(codes.FailedPrecondition, "INVALID_DEDUPLICATION_PERIOD(9,abcd): The dedup period exceeds the max"), nil},
		// The token must be anchored on the "(" delimiter: a hypothetical future
		// id sharing a recognized token as a prefix must NOT over-match into a
		// benign sentinel.
		{"CONTRACT_NOT_FOUND prefix over-match is opaque", status.Error(codes.NotFound, "CONTRACT_NOT_FOUND_X(11,abcd): a different future error"), nil},
		// A recognized token not rendered in the TOKEN(...) form (no "(") is not
		// treated as a Canton error id.
		{"bare DUPLICATE_COMMAND word without paren is opaque", status.Error(codes.AlreadyExists, "the phrase DUPLICATE_COMMAND appears but not as a token"), nil},
		{"non-status error passes through", errors.New("dial tcp: connection refused"), nil},
	}
	for _, tc := range cases {
		t.Run(tc.name, func(t *testing.T) {
			got := classifySubmitError(tc.in)
			switch tc.want {
			case nil:
				// Must be none of the benign sentinels. nil in stays nil; anything
				// else is surfaced opaquely (== the input) so the crank retries.
				assert.False(t, errors.Is(got, ErrDuplicateCommand), "must not be ErrDuplicateCommand")
				assert.False(t, errors.Is(got, ErrContractInactive), "must not be ErrContractInactive")
				assert.False(t, errors.Is(got, ErrSubmissionInFlight), "must not be ErrSubmissionInFlight")
				if tc.in == nil {
					assert.NoError(t, got)
				} else {
					assert.Equal(t, tc.in, got, "transient/opaque errors pass through unchanged")
				}
			default:
				assert.ErrorIs(t, got, tc.want)
			}
		})
	}
}

func TestBuildApproveCommands(t *testing.T) {
	const operator = "Operator::1220abcd"
	req := PendingEmitterRequest{
		ContractID: "00deadbeef#0",
		TemplateID: TemplateID{PackageID: "cafebabe", ModuleName: "Wormhole.Core.State", EntityName: "EmitterRequest"},
		Requester:  "Alice::1220ef01",
		Operator:   operator,
	}
	cmdID := "approve-0011223344556677"
	const dedup = 90 * time.Minute

	commands := buildApproveCommands(operator, req, cmdID, dedup)

	// Change-id components must be pinned exactly.
	assert.Equal(t, cmdID, commands.GetCommandId(), "command id used verbatim (dedup key)")
	assert.Equal(t, crankUserID, commands.GetUserId(), "fixed crank user id (dedup key component)")
	assert.Equal(t, []string{operator}, commands.GetActAs(), "act_as is exactly [operator]")

	// Explicit deduplication duration must be set.
	dur, ok := commands.GetDeduplicationPeriod().(*apiv2.Commands_DeduplicationDuration)
	require.True(t, ok, "deduplication_duration must be set (not offset/unset)")
	assert.Equal(t, dedup, dur.DeduplicationDuration.AsDuration(), "dedup duration threaded through verbatim")

	// Exactly one ExerciseCommand for ApproveEmitter with an empty-record argument
	// and the template id echoed from the request.
	require.Len(t, commands.GetCommands(), 1)
	ex := commands.GetCommands()[0].GetExercise()
	require.NotNil(t, ex, "must be an ExerciseCommand")
	assert.Equal(t, approveEmitterChoice, ex.GetChoice())
	assert.Equal(t, req.ContractID, ex.GetContractId())
	assert.Equal(t, req.TemplateID.PackageID, ex.GetTemplateId().GetPackageId())
	assert.Equal(t, req.TemplateID.ModuleName, ex.GetTemplateId().GetModuleName())
	assert.Equal(t, req.TemplateID.EntityName, ex.GetTemplateId().GetEntityName())
	rec := ex.GetChoiceArgument().GetRecord()
	require.NotNil(t, rec, "choice argument must be a record")
	assert.Empty(t, rec.GetFields(), "ApproveEmitter takes no parameters -> empty record")
}
