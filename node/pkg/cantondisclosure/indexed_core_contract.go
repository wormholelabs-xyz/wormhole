package cantondisclosure

import (
	"bytes"
	"errors"
	"fmt"

	"github.com/certusone/wormhole/node/pkg/cantonclient"
)

// indexEntryOverheadBytes is the fixed byte count charged per entry in
// addition to its blob, for the contract id, key texts, map and struct overhead.
// A typical entry uses about 600 bytes; the 255-byte party and 512-byte namespace maximum uses about 1.3 KiB.
const indexEntryOverheadBytes = 1536

// replayNodeLocation is the trie node that a ReplayNode covers: Daml fields consumer, namespace and prefix.
type replayNodeLocation struct {
	consumer  cantonclient.PartyID
	namespace cantonclient.ReplayNamespace
	prefix    cantonclient.DigestNibblePrefix
}

func (location replayNodeLocation) isZero() bool {
	return location == replayNodeLocation{}
}

// IndexedCoreContract is one live core contract in the index. The blob is immutable.
type IndexedCoreContract struct {
	contractID         cantonclient.ContractID
	templatePackageID  cantonclient.PackageID
	template           cantonclient.CoreContractTemplate
	synchronizerID     cantonclient.SynchronizerID
	replayNodeLocation replayNodeLocation
	createdEventBlob   []byte
}

// newIndexedCoreContract requires a ReplayNode location for ReplayNode and the zero location for every other template.
func newIndexedCoreContract(
	contractID cantonclient.ContractID,
	templatePackageID cantonclient.PackageID,
	template cantonclient.CoreContractTemplate,
	synchronizerID cantonclient.SynchronizerID,
	location replayNodeLocation,
	createdEventBlob []byte,
) (IndexedCoreContract, error) {
	if contractID == (cantonclient.ContractID{}) {
		return IndexedCoreContract{}, errors.New("missing contract id")
	}
	if templatePackageID == (cantonclient.PackageID{}) {
		return IndexedCoreContract{}, fmt.Errorf("contract %s: missing template package id", contractID)
	}
	if synchronizerID == (cantonclient.SynchronizerID{}) {
		return IndexedCoreContract{}, fmt.Errorf("contract %s: missing synchronizer id", contractID)
	}
	if len(createdEventBlob) == 0 {
		return IndexedCoreContract{}, fmt.Errorf("contract %s: empty created event blob", contractID)
	}
	isReplayNode := template == cantonclient.CoreContractTemplateReplayNode
	if !isReplayNode && !isSingletonTemplate(template) {
		return IndexedCoreContract{}, fmt.Errorf("contract %s: unknown core contract template %d", contractID, int(template))
	}
	if isReplayNode && location.isZero() {
		return IndexedCoreContract{}, fmt.Errorf("contract %s: replay node without a location", contractID)
	}
	if isReplayNode && (location.consumer == cantonclient.PartyID{} || location.namespace == cantonclient.ReplayNamespace{}) {
		return IndexedCoreContract{}, fmt.Errorf("contract %s: replay node location without consumer or namespace", contractID)
	}
	if !isReplayNode && !location.isZero() {
		return IndexedCoreContract{}, fmt.Errorf("contract %s: %s with a replay node location", contractID, template)
	}
	return IndexedCoreContract{
		contractID:         contractID,
		templatePackageID:  templatePackageID,
		template:           template,
		synchronizerID:     synchronizerID,
		replayNodeLocation: location,
		createdEventBlob:   bytes.Clone(createdEventBlob),
	}, nil
}

// indexedCoreContractFromCreated requires the guardianGovernance of the index.
func indexedCoreContractFromCreated(created cantonclient.CreatedCoreContract, guardianGovernance cantonclient.PartyID) (IndexedCoreContract, error) {
	if created.GuardianGovernance() != guardianGovernance {
		return IndexedCoreContract{}, fmt.Errorf("contract %s: guardianGovernance %q is not the index guardianGovernance %s", created.ContractID(), created.GuardianGovernance(), guardianGovernance)
	}
	location := replayNodeLocation{}
	if replayNodeKey, isReplayNode := created.ReplayNodeKey(); isReplayNode {
		location = replayNodeLocation{consumer: replayNodeKey.Consumer(), namespace: replayNodeKey.Namespace(), prefix: replayNodeKey.Prefix()}
	}
	contract, err := newIndexedCoreContract(created.ContractID(), created.TemplateID().PackageID(), created.TemplateID().Template(), created.SynchronizerID(), location, created.CreatedEventBlob())
	if err != nil {
		return IndexedCoreContract{}, err
	}
	if contract.indexEntryByteCount() != int64(created.CreatedEventBlobByteCount())+indexEntryOverheadBytes {
		return IndexedCoreContract{}, fmt.Errorf("contract %s: blob byte count changed in conversion", contract.contractID)
	}
	return contract, nil
}

func (contract IndexedCoreContract) ContractID() cantonclient.ContractID { return contract.contractID }

func (contract IndexedCoreContract) TemplatePackageID() cantonclient.PackageID {
	return contract.templatePackageID
}

func (contract IndexedCoreContract) Template() cantonclient.CoreContractTemplate {
	return contract.template
}

// TemplateID is "packageId:Module:Entity".
func (contract IndexedCoreContract) TemplateID() string {
	return contract.templatePackageID.String() + ":" + contract.template.String()
}

func (contract IndexedCoreContract) SynchronizerID() cantonclient.SynchronizerID {
	return contract.synchronizerID
}

// CreatedEventBlob is a copy of the explicit-disclosure payload.
func (contract IndexedCoreContract) CreatedEventBlob() []byte {
	return bytes.Clone(contract.createdEventBlob)
}

func (contract IndexedCoreContract) replayNodeLocationOf() (replayNodeLocation, bool) {
	if contract.template != cantonclient.CoreContractTemplateReplayNode {
		return replayNodeLocation{}, false
	}
	return contract.replayNodeLocation, true
}

// indexEntryByteCount is the byte count that the entry adds to the index.
func (contract IndexedCoreContract) indexEntryByteCount() int64 {
	return int64(len(contract.createdEventBlob)) + indexEntryOverheadBytes
}

func (contract IndexedCoreContract) hasEqualContent(other IndexedCoreContract) bool {
	return contract.contractID == other.contractID &&
		contract.templatePackageID == other.templatePackageID &&
		contract.template == other.template &&
		contract.synchronizerID == other.synchronizerID &&
		contract.replayNodeLocation == other.replayNodeLocation &&
		bytes.Equal(contract.createdEventBlob, other.createdEventBlob)
}

// archivedCoreContract identifies an archive by contract id. The template is checked against the live entry.
type archivedCoreContract struct {
	contractID cantonclient.ContractID
	template   cantonclient.CoreContractTemplate
}

// coreContractIndexUpdate is one create or archive of a transaction.
type coreContractIndexUpdate struct {
	kind     cantonclient.CoreContractUpdateKind
	created  IndexedCoreContract
	archived archivedCoreContract
}

// indexUpdateFromCoreContractUpdate requires the offset of the transaction.
func indexUpdateFromCoreContractUpdate(update cantonclient.CoreContractUpdate, transactionOffset int64, guardianGovernance cantonclient.PartyID) (coreContractIndexUpdate, error) {
	if update.Offset() != transactionOffset {
		return coreContractIndexUpdate{}, fmt.Errorf("update offset %d is not the transaction offset %d", update.Offset(), transactionOffset)
	}
	switch update.Kind() {
	case cantonclient.CoreContractUpdateKindCreated:
		created, isCreated := update.Created()
		if !isCreated {
			return coreContractIndexUpdate{}, errors.New("created update without a created contract")
		}
		contract, err := indexedCoreContractFromCreated(created, guardianGovernance)
		if err != nil {
			return coreContractIndexUpdate{}, err
		}
		return coreContractIndexUpdate{kind: cantonclient.CoreContractUpdateKindCreated, created: contract}, nil
	case cantonclient.CoreContractUpdateKindArchived:
		archived, isArchived := update.Archived()
		if !isArchived {
			return coreContractIndexUpdate{}, errors.New("archived update without an archived contract")
		}
		if archived.ContractID() == (cantonclient.ContractID{}) {
			return coreContractIndexUpdate{}, errors.New("archived update without a contract id")
		}
		return coreContractIndexUpdate{
			kind:     cantonclient.CoreContractUpdateKindArchived,
			archived: archivedCoreContract{contractID: archived.ContractID(), template: archived.TemplateID().Template()},
		}, nil
	default:
		return coreContractIndexUpdate{}, fmt.Errorf("unknown core contract update kind %d", int(update.Kind()))
	}
}
