// This file is the ACS (active-contract-set) read path used by the
// emitter-approval crank: it lists active EmitterRequest contracts via
// StateService.GetActiveContracts. It depends only on the already-committed,
// CI-verified State-service stubs under ./proto/gen (no write-path protos), so
// it builds by default — no build tag. See the package README and
// canton/.claude/tasks/canton-emitter-approval-crank.md §4.
package cantonclient

import (
	"context"
	"fmt"
	"io"

	"go.uber.org/zap"

	apiv2 "github.com/certusone/wormhole/node/pkg/cantonclient/proto/gen/com/daml/ledger/api/v2"
)

// The template the crank's work queue is drawn from. PackageID is intentionally
// left empty in the filter so it matches any package version (Daml package
// upgrades change the package id while preserving module/entity) — the same
// package-id-agnostic convention the watcher uses (see watcher.go).
const (
	emitterRequestModule = "Wormhole.Core.State"
	emitterRequestEntity = "EmitterRequest"
	emitterEntity        = "Emitter"
)

// EmitterInfo is a decoded active Emitter contract: enough of its key components
// to assert exactly-once approval end-to-end. Not part of the CantonClient
// interface — it is a concrete read helper (ActiveEmitters) used by the crank
// integration test; production readers use the watcher's update stream.
type EmitterInfo struct {
	ContractID string
	Operator   string
	Owner      string
	EmitterID  uint64
}

// ActiveEmitters lists the active Emitter contracts visible to the operator
// party. Same StateService.GetActiveContracts snapshot as ActiveEmitterRequests,
// matching the Emitter template and decoding its key components (operator, owner,
// emitterId). See ActiveEmitterRequests for the mechanics.
func (c *grpcClient) ActiveEmitters(ctx context.Context, operator string) ([]EmitterInfo, error) {
	tmpl := TemplateID{ModuleName: emitterRequestModule, EntityName: emitterEntity}
	var out []EmitterInfo
	err := c.scanActiveContracts(ctx, operator, tmpl, func(ce *apiv2.CreatedEvent) error {
		info, derr := decodeEmitter(ce)
		if derr != nil {
			return derr
		}
		out = append(out, info)
		return nil
	})
	if err != nil {
		return nil, err
	}
	return out, nil
}

// ActiveEmitterRequests lists the active EmitterRequest contracts visible to the
// operator party. It snapshots the ACS at the current ledger end
// (GetActiveContracts requires an active_at_offset no greater than ledger end),
// filters to the operator, and client-side matches the EmitterRequest template.
// The ACS is the crank's work queue: a pending request is, by definition, an
// active contract, so no local state is needed.
func (c *grpcClient) ActiveEmitterRequests(ctx context.Context, operator string) ([]PendingEmitterRequest, error) {
	tmpl := TemplateID{ModuleName: emitterRequestModule, EntityName: emitterRequestEntity}
	var out []PendingEmitterRequest
	err := c.scanActiveContracts(ctx, operator, tmpl, func(ce *apiv2.CreatedEvent) error {
		req, derr := decodeEmitterRequest(ce)
		if derr != nil {
			// Skip an undecodable request rather than stalling the whole queue.
			if c.logger != nil {
				c.logger.Error("canton: failed to decode EmitterRequest",
					zap.String("cid", ce.GetContractId()), zap.Error(derr))
			}
			return nil
		}
		out = append(out, req)
		return nil
	})
	if err != nil {
		return nil, err
	}
	return out, nil
}

// scanActiveContracts snapshots the ACS at the current ledger end
// (GetActiveContracts requires an active_at_offset no greater than ledger end),
// scoped to the operator party, and invokes visit for each active contract whose
// CreatedEvent matches tmpl (package-id-agnostic). A visit error aborts the scan.
func (c *grpcClient) scanActiveContracts(ctx context.Context, operator string, tmpl TemplateID, visit func(*apiv2.CreatedEvent) error) error {
	end, err := c.GetLedgerEnd(ctx)
	if err != nil {
		return err
	}
	// Bind the stream to a cancellable child context so an early return (a visit
	// error, or the caller aborting) cancels the RPC and releases its resources
	// instead of leaking the stream. Mirrors SubscribeUpdates in cantongrpc.go.
	ctx, cancel := context.WithCancel(ctx)
	defer cancel()
	stream, err := c.state.GetActiveContracts(ctx, &apiv2.GetActiveContractsRequest{
		ActiveAtOffset: end,
		EventFormat:    acsEventFormat(operator),
	})
	if err != nil {
		return fmt.Errorf("GetActiveContracts: %w", err)
	}
	for {
		resp, err := stream.Recv()
		if err == io.EOF {
			return nil
		}
		if err != nil {
			return fmt.Errorf("GetActiveContracts stream: %w", err)
		}
		// A contract_entry is one of active_contract / incomplete_(un)assigned;
		// only active contracts carry a live CreatedEvent.
		ac := resp.GetActiveContract()
		if ac == nil {
			continue
		}
		ce := ac.GetCreatedEvent()
		if ce == nil || !templateMatches(ce.GetTemplateId(), tmpl) {
			continue
		}
		if err := visit(ce); err != nil {
			return err
		}
	}
}

// acsEventFormat builds the GetActiveContracts EventFormat: verbose (so record
// fields carry labels for partyField) and scoped to a single party via
// filters_by_party with a wildcard template filter (client-side template match
// follows). A server-side TemplateFilter is a valid alternative; wildcard +
// client match keeps this consistent with the update-stream path in cantongrpc.go.
func acsEventFormat(party string) *apiv2.EventFormat {
	wildcard := &apiv2.Filters{
		Cumulative: []*apiv2.CumulativeFilter{{
			IdentifierFilter: &apiv2.CumulativeFilter_WildcardFilter{
				WildcardFilter: &apiv2.WildcardFilter{},
			},
		}},
	}
	return &apiv2.EventFormat{
		Verbose:        true,
		FiltersByParty: map[string]*apiv2.Filters{party: wildcard},
	}
}

// decodeEmitterRequest maps an EmitterRequest CreatedEvent to a
// PendingEmitterRequest. EmitterRequest (see Wormhole.Core.State) has two Party
// fields, requester and operator; the template id is echoed verbatim so the
// approve exercise names the exact package the ledger reported.
func decodeEmitterRequest(ce *apiv2.CreatedEvent) (PendingEmitterRequest, error) {
	rec := ce.GetCreateArguments()
	if rec == nil {
		return PendingEmitterRequest{}, fmt.Errorf("EmitterRequest %s has no create arguments", ce.GetContractId())
	}
	fields := map[string]*apiv2.Value{}
	for _, f := range rec.GetFields() {
		fields[f.GetLabel()] = f.GetValue()
	}
	requester, err := partyField(fields, "requester")
	if err != nil {
		return PendingEmitterRequest{}, err
	}
	operator, err := partyField(fields, "operator")
	if err != nil {
		return PendingEmitterRequest{}, err
	}
	return PendingEmitterRequest{
		ContractID: ce.GetContractId(),
		TemplateID: templateIDFromProto(ce.GetTemplateId()),
		Requester:  requester,
		Operator:   operator,
	}, nil
}

// decodeEmitter maps an Emitter CreatedEvent to an EmitterInfo. Emitter (see
// Wormhole.Core.State) has Party fields operator and owner and an Int emitterId
// among others; only the key-identifying subset is decoded here.
func decodeEmitter(ce *apiv2.CreatedEvent) (EmitterInfo, error) {
	rec := ce.GetCreateArguments()
	if rec == nil {
		return EmitterInfo{}, fmt.Errorf("Emitter %s has no create arguments", ce.GetContractId())
	}
	fields := map[string]*apiv2.Value{}
	for _, f := range rec.GetFields() {
		fields[f.GetLabel()] = f.GetValue()
	}
	operator, err := partyField(fields, "operator")
	if err != nil {
		return EmitterInfo{}, err
	}
	owner, err := partyField(fields, "owner")
	if err != nil {
		return EmitterInfo{}, err
	}
	emitterID, err := intField(fields, "emitterId")
	if err != nil {
		return EmitterInfo{}, err
	}
	if emitterID < 0 {
		return EmitterInfo{}, fmt.Errorf("Emitter %s has negative emitterId %d", ce.GetContractId(), emitterID)
	}
	return EmitterInfo{
		ContractID: ce.GetContractId(),
		Operator:   operator,
		Owner:      owner,
		EmitterID:  uint64(emitterID),
	}, nil
}

// templateIDFromProto lifts a Ledger API Identifier into the domain TemplateID.
func templateIDFromProto(id *apiv2.Identifier) TemplateID {
	if id == nil {
		return TemplateID{}
	}
	return TemplateID{
		PackageID:  id.GetPackageId(),
		ModuleName: id.GetModuleName(),
		EntityName: id.GetEntityName(),
	}
}
