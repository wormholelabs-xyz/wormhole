// Package cantonclienttest builds Ledger API core contract events and decodes
// them with the production cantonclient decoder. Import it from tests only.
package cantonclienttest

import (
	"testing"

	"github.com/stretchr/testify/require"

	"github.com/certusone/wormhole/node/pkg/cantonclient"
	apiv2 "github.com/certusone/wormhole/node/pkg/cantonclient/proto/gen/com/daml/ledger/api/v2"
)

// Daml identity of Wormhole.Core.Bytes.DigestNibblePrefix.
const (
	digestNibblePrefixModuleName  = "Wormhole.Core.Bytes"
	digestNibblePrefixEntityName  = "DigestNibblePrefix"
	digestNibblePrefixConstructor = "DigestNibblePrefix"
	replayNodeSplitThreshold      = 128
)

// CoreContractLedger is the read party and the approved package of the events that a test decodes.
type CoreContractLedger struct {
	GuardianGovernanceText string
	PackageIDHex           string
	SynchronizerIDText     string
}

// CoreContractEvent describes one created or archived core contract event.
// ConsumerText, NamespaceText and PrefixNibbles apply to ReplayNode only.
type CoreContractEvent struct {
	Template               cantonclient.CoreContractTemplate
	ContractIDHex          string
	GuardianGovernanceText string
	ConsumerText           string
	NamespaceText          string
	PrefixNibbles          string
	CreatedEventBlob       []byte
}

func (ledger CoreContractLedger) readScope(t *testing.T) cantonclient.ReadScope {
	t.Helper()
	readScope, err := cantonclient.ParseReadScope(ledger.GuardianGovernanceText, false)
	require.NoError(t, err)
	return readScope
}

func (ledger CoreContractLedger) approvedCorePackages(t *testing.T) cantonclient.ApprovedCorePackages {
	t.Helper()
	approved, err := cantonclient.ParseApprovedCorePackages([]byte(`[{"packageId":"` + ledger.PackageIDHex + `","version":"0.12.3"}]`))
	require.NoError(t, err)
	return approved
}

func (ledger CoreContractLedger) templateIdentifier(template cantonclient.CoreContractTemplate) *apiv2.Identifier {
	return &apiv2.Identifier{PackageId: ledger.PackageIDHex, ModuleName: template.ModuleName(), EntityName: template.EntityName()}
}

func partyValue(partyText string) *apiv2.Value {
	return &apiv2.Value{Sum: &apiv2.Value_Party{Party: partyText}}
}

func textValue(text string) *apiv2.Value {
	return &apiv2.Value{Sum: &apiv2.Value_Text{Text: text}}
}

func int64Value(number int64) *apiv2.Value {
	return &apiv2.Value{Sum: &apiv2.Value_Int64{Int64: number}}
}

func (ledger CoreContractLedger) createArguments(event CoreContractEvent) *apiv2.Record {
	fields := []*apiv2.RecordField{{Label: "guardianGovernance", Value: partyValue(event.GuardianGovernanceText)}}
	if event.Template == cantonclient.CoreContractTemplateReplayNode {
		prefixVariant := &apiv2.Value{Sum: &apiv2.Value_Variant{Variant: &apiv2.Variant{
			VariantId:   &apiv2.Identifier{PackageId: ledger.PackageIDHex, ModuleName: digestNibblePrefixModuleName, EntityName: digestNibblePrefixEntityName},
			Constructor: digestNibblePrefixConstructor,
			Value:       textValue(event.PrefixNibbles),
		}}}
		fields = append(fields,
			&apiv2.RecordField{Label: "consumer", Value: partyValue(event.ConsumerText)},
			&apiv2.RecordField{Label: "namespace", Value: textValue(event.NamespaceText)},
			&apiv2.RecordField{Label: "prefix", Value: prefixVariant},
			&apiv2.RecordField{Label: "splitThreshold", Value: int64Value(replayNodeSplitThreshold)},
		)
	}
	return &apiv2.Record{Fields: fields}
}

// CreatedEvent is the verbose created event of event at offset.
func (ledger CoreContractLedger) CreatedEvent(offset int64, event CoreContractEvent) *apiv2.CreatedEvent {
	return &apiv2.CreatedEvent{
		Offset:           offset,
		ContractId:       event.ContractIDHex,
		TemplateId:       ledger.templateIdentifier(event.Template),
		PackageName:      cantonclient.CorePackageName,
		CreateArguments:  ledger.createArguments(event),
		CreatedEventBlob: event.CreatedEventBlob,
	}
}

// ArchivedEvent is the archived event of event at offset.
func (ledger CoreContractLedger) ArchivedEvent(offset int64, event CoreContractEvent) *apiv2.ArchivedEvent {
	return &apiv2.ArchivedEvent{
		Offset:      offset,
		ContractId:  event.ContractIDHex,
		TemplateId:  ledger.templateIdentifier(event.Template),
		PackageName: cantonclient.CorePackageName,
	}
}

// DecodeCreated decodes the created event of event as an active contract.
func (ledger CoreContractLedger) DecodeCreated(t *testing.T, event CoreContractEvent) cantonclient.CreatedCoreContract {
	t.Helper()
	contract, err := cantonclient.DecodeCreatedCoreContractForTest(ledger.readScope(t), ledger.approvedCorePackages(t), ledger.CreatedEvent(1, event), ledger.SynchronizerIDText)
	require.NoError(t, err)
	require.Equal(t, event.ContractIDHex, contract.ContractID().String())
	return contract
}

// CoreContractChange is one event of a transaction: a create, or an archive when IsArchive is true.
type CoreContractChange struct {
	IsArchive bool
	Event     CoreContractEvent
}

// Created is a create of event.
func Created(event CoreContractEvent) CoreContractChange {
	return CoreContractChange{Event: event}
}

// Archived is an archive of event.
func Archived(event CoreContractEvent) CoreContractChange {
	return CoreContractChange{IsArchive: true, Event: event}
}

// DecodeTransaction decodes a transaction at offset with changes, which must follow previousOffset.
func (ledger CoreContractLedger) DecodeTransaction(t *testing.T, previousOffset int64, offset int64, changes ...CoreContractChange) cantonclient.CoreContractLedgerAdvance {
	t.Helper()
	events := make([]*apiv2.Event, 0, len(changes))
	for _, change := range changes {
		if change.IsArchive {
			events = append(events, &apiv2.Event{Event: &apiv2.Event_Archived{Archived: ledger.ArchivedEvent(offset, change.Event)}})
			continue
		}
		events = append(events, &apiv2.Event{Event: &apiv2.Event_Created{Created: ledger.CreatedEvent(offset, change.Event)}})
	}
	response := &apiv2.GetUpdatesResponse{Update: &apiv2.GetUpdatesResponse_Transaction{Transaction: &apiv2.Transaction{
		Offset:         offset,
		SynchronizerId: ledger.SynchronizerIDText,
		Events:         events,
	}}}
	advance, advances, err := cantonclient.DecodeCoreContractUpdatesResponseForTest(ledger.readScope(t), ledger.approvedCorePackages(t), response, previousOffset)
	require.NoError(t, err)
	require.True(t, advances)
	require.Len(t, advance.Updates(), len(changes))
	return advance
}

// DecodeOffsetCheckpoint decodes an offset checkpoint at offset, which must exceed previousOffset.
func (ledger CoreContractLedger) DecodeOffsetCheckpoint(t *testing.T, previousOffset int64, offset int64) cantonclient.CoreContractLedgerAdvance {
	t.Helper()
	response := &apiv2.GetUpdatesResponse{Update: &apiv2.GetUpdatesResponse_OffsetCheckpoint{OffsetCheckpoint: &apiv2.OffsetCheckpoint{Offset: offset}}}
	advance, advances, err := cantonclient.DecodeCoreContractUpdatesResponseForTest(ledger.readScope(t), ledger.approvedCorePackages(t), response, previousOffset)
	require.NoError(t, err)
	require.True(t, advances)
	require.True(t, advance.IsOffsetCheckpoint())
	return advance
}
