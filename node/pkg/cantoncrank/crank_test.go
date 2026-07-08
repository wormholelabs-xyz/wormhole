package cantoncrank

import (
	"context"
	"errors"
	"testing"

	"github.com/stretchr/testify/assert"
	"github.com/stretchr/testify/require"
	"go.uber.org/zap"

	"github.com/certusone/wormhole/node/pkg/cantonclient"
)

// fakeClient implements Reader + Submitter with scripted behaviour.
type fakeClient struct {
	acs     []PendingRequest
	submits []submitCall           // recorded, in order
	respond func(cid string) error // per-contract submit result
	readErr func() error           // optional per-call ActiveEmitterRequests error
}

type submitCall struct{ operator, contractID, commandID string }

func (f *fakeClient) ActiveEmitterRequests(_ context.Context, operator string) ([]PendingRequest, error) {
	if f.readErr != nil {
		if err := f.readErr(); err != nil {
			return nil, err
		}
	}
	var out []PendingRequest
	for _, r := range f.acs {
		if r.Operator == operator {
			out = append(out, r)
		}
	}
	return out, nil
}

func (f *fakeClient) SubmitApproveEmitter(_ context.Context, operator string, req PendingRequest, commandID string) error {
	f.submits = append(f.submits, submitCall{operator, req.ContractID, commandID})
	if f.respond != nil {
		return f.respond(req.ContractID)
	}
	return nil
}

const opParty = "Operator::1220abcd"

func newCrank(f *fakeClient, policy Policy) *Crank {
	return New(f, f, opParty, policy, 0, 0, zap.NewNop())
}

func TestCommandIdIsDeterministic(t *testing.T) {
	a1 := commandIDFor("00aabb#0")
	a2 := commandIDFor("00aabb#0")
	b := commandIDFor("00ccdd#1")
	assert.Equal(t, a1, a2, "same contract id must yield the same command id (dedup key)")
	assert.NotEqual(t, a1, b, "different contract ids must yield different command ids")
	assert.NotEmpty(t, a1)
}

func TestApprovesPendingRequest(t *testing.T) {
	f := &fakeClient{acs: []PendingRequest{{ContractID: "req#0", Requester: "Alice::12", Operator: opParty}}}
	require.NoError(t, newCrank(f, ApproveAll).tick(context.Background()))
	require.Len(t, f.submits, 1)
	assert.Equal(t, opParty, f.submits[0].operator)
	assert.Equal(t, "req#0", f.submits[0].contractID)
	assert.Equal(t, commandIDFor("req#0"), f.submits[0].commandID)
}

func TestIdempotentAcrossTicks(t *testing.T) {
	// Request still present on tick two (approval not yet observed): the crank
	// must re-submit with the SAME command id so the ledger dedups it.
	f := &fakeClient{acs: []PendingRequest{{ContractID: "req#0", Operator: opParty}}}
	c := newCrank(f, ApproveAll)
	require.NoError(t, c.tick(context.Background()))
	require.NoError(t, c.tick(context.Background()))
	require.Len(t, f.submits, 2)
	assert.Equal(t, f.submits[0].commandID, f.submits[1].commandID, "command id must be stable across ticks")
}

func TestSkipsForeignOperator(t *testing.T) {
	f := &fakeClient{acs: []PendingRequest{{ContractID: "req#0", Operator: "SomeoneElse::99"}}}
	require.NoError(t, newCrank(f, ApproveAll).tick(context.Background()))
	assert.Empty(t, f.submits, "must not approve requests whose operator is not us")
}

func TestAlreadyApprovedIsNotRetried(t *testing.T) {
	// Both terminal-benign sentinels must be swallowed: the tick returns nil (no
	// surfaced error to retry on) whether dedup fired (ErrDuplicateCommand) or the
	// request contract was already consumed (ErrContractInactive).
	for _, benign := range []error{ErrDuplicateCommand, ErrContractInactive} {
		f := &fakeClient{
			acs:     []PendingRequest{{ContractID: "req#0", Operator: opParty}},
			respond: func(string) error { return benign },
		}
		c := newCrank(f, ApproveAll)
		require.NoError(t, c.tick(context.Background()))
		assert.Len(t, f.submits, 1)
	}
}

func TestTransientErrorRetriesNextTick(t *testing.T) {
	calls := 0
	f := &fakeClient{
		acs: []PendingRequest{{ContractID: "req#0", Operator: opParty}},
		respond: func(string) error {
			calls++
			if calls == 1 {
				return errors.New("UNAVAILABLE: transient")
			}
			return nil
		},
	}
	c := newCrank(f, ApproveAll)
	require.NoError(t, c.tick(context.Background())) // fails transiently
	require.NoError(t, c.tick(context.Background())) // succeeds
	require.Len(t, f.submits, 2)
	assert.Equal(t, f.submits[0].commandID, f.submits[1].commandID)
}

func TestValidationPolicyGate(t *testing.T) {
	f := &fakeClient{acs: []PendingRequest{
		{ContractID: "allow#0", Requester: "Good::1", Operator: opParty},
		{ContractID: "deny#0", Requester: "Bad::1", Operator: opParty},
	}}
	denyBad := func(r PendingRequest) bool { return r.Requester != "Bad::1" }
	require.NoError(t, newCrank(f, denyBad).tick(context.Background()))
	require.Len(t, f.submits, 1)
	assert.Equal(t, "allow#0", f.submits[0].contractID)
}

func TestInFlightIsSkippedNotFatal(t *testing.T) {
	// A redundant instance is submitting this change id right now. The submit
	// reports in-flight; the crank skips it benignly (no surfaced error) and, on
	// the next tick with the request still in the ACS, resubmits with the SAME
	// command id.
	f := &fakeClient{
		acs:     []PendingRequest{{ContractID: "req#0", Operator: opParty}},
		respond: func(string) error { return ErrSubmissionInFlight },
	}
	c := newCrank(f, ApproveAll)
	require.NoError(t, c.tick(context.Background()), "in-flight must not surface as a tick error")
	require.NoError(t, c.tick(context.Background()))
	require.Len(t, f.submits, 2)
	assert.Equal(t, f.submits[0].commandID, f.submits[1].commandID, "command id must be stable across in-flight retries")
}

func TestPolicyHelpers(t *testing.T) {
	const pkg = "cafebabe"
	mk := func(pkgID, requester string) PendingRequest {
		return PendingRequest{
			TemplateID: cantonclient.TemplateID{PackageID: pkgID},
			Requester:  requester,
			Operator:   opParty,
		}
	}

	assert.True(t, RequirePackageID(pkg)(mk(pkg, "Alice::1")))
	assert.False(t, RequirePackageID(pkg)(mk("deadbeef", "Alice::1")), "wrong package id must be rejected")

	allow := AllowRequesters("Alice::1", "Bob::2")
	assert.True(t, allow(mk(pkg, "Alice::1")))
	assert.False(t, allow(mk(pkg, "Mallory::9")), "unlisted requester must be rejected")

	// All ANDs the constituents: both package id and requester must match.
	prod := All(RequirePackageID(pkg), AllowRequesters("Alice::1"))
	assert.True(t, prod(mk(pkg, "Alice::1")))
	assert.False(t, prod(mk("deadbeef", "Alice::1")), "wrong package id fails the AND")
	assert.False(t, prod(mk(pkg, "Mallory::9")), "unlisted requester fails the AND")

	// DenyAll rejects everything; empty All is identity (approves).
	assert.False(t, DenyAll(mk(pkg, "Alice::1")))
	assert.True(t, All()(mk(pkg, "Alice::1")), "empty All is identity")
}

// TestNilPolicyIsDenyAll verifies the fail-safe default: New with a nil policy
// must approve nothing (DenyAll), never fall back to ApproveAll.
func TestNilPolicyIsDenyAll(t *testing.T) {
	f := &fakeClient{acs: []PendingRequest{{ContractID: "req#0", Operator: opParty}}}
	c := New(f, f, opParty, nil, 0, 0, zap.NewNop())
	require.NoError(t, c.tick(context.Background()))
	assert.Empty(t, f.submits, "nil policy must default to DenyAll, not ApproveAll")
}

func TestReaderErrorSurfacesAndLoopSurvives(t *testing.T) {
	// The ACS read fails on the first tick: tick surfaces the error (Run logs it
	// and keeps looping). On the next tick the read succeeds and the request is
	// approved — proving one failed read does not wedge the crank.
	calls := 0
	f := &fakeClient{
		acs: []PendingRequest{{ContractID: "req#0", Operator: opParty}},
		readErr: func() error {
			calls++
			if calls == 1 {
				return errors.New("UNAVAILABLE: ledger end unreachable")
			}
			return nil
		},
	}
	c := newCrank(f, ApproveAll)
	require.Error(t, c.tick(context.Background()), "reader error must surface from tick")
	assert.Empty(t, f.submits, "no submit when the ACS read failed")
	require.NoError(t, c.tick(context.Background()))
	require.Len(t, f.submits, 1, "loop survives the earlier read error and approves on the next tick")
}
