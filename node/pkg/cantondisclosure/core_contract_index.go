package cantondisclosure

import (
	"errors"
	"fmt"
	"strings"
	"sync"

	"github.com/certusone/wormhole/node/pkg/cantonclient"
)

// ProductionIndexByteCapBytes is the production byte cap of a CoreContractIndex: 4 GiB.
const ProductionIndexByteCapBytes int64 = 4 << 30

// maxIndexByteCapBytes keeps usedBytes*100 inside int64.
const maxIndexByteCapBytes int64 = 1 << 40

// byteCapWarningPercent is the used-byte percentage of the cap at which readiness is IndexReadinessReadyNearByteCap.
const byteCapWarningPercent = 80

// maxCoreContractUpdatesPerTransaction bounds one ApplyTransaction call.
const maxCoreContractUpdatesPerTransaction = 1 << 16

type indexLifecycle int

const (
	indexLifecycleAwaitingBootstrap indexLifecycle = iota + 1
	indexLifecycleLive
	indexLifecycleInconsistent
)

// CoreContractIndex holds the live disclosed core contracts of one
// guardianGovernance party with their created event blobs.
// One goroutine applies the snapshot and the transactions; any goroutine can look up.
type CoreContractIndex struct {
	guardianGovernance cantonclient.PartyID
	byteCapBytes       int64

	mutex                           sync.RWMutex
	lifecycle                       indexLifecycle
	offset                          int64
	usedBytes                       int64
	contractsByID                   map[cantonclient.ContractID]IndexedCoreContract
	singletonContractIDsByTemplate  map[cantonclient.CoreContractTemplate]map[cantonclient.ContractID]struct{}
	replayNodeContractIDsByLocation map[replayNodeLocation][]cantonclient.ContractID
	liveReplayNodeCount             int
	ignoredUnknownArchiveCount      uint64
	// replayNodesDroppedAtByteCap: a create reached the byte cap. The index holds singletons only from then on.
	replayNodesDroppedAtByteCap  bool
	droppedReplayNodeUpdateCount uint64
	// lookupsSuspended: a resnapshot found a stale or missing contract. The index keeps applying updates.
	lookupsSuspended bool
}

// IndexStatus is a point-in-time copy of the index counters.
type IndexStatus struct {
	Readiness           IndexReadiness
	Offset              int64
	UsedBytes           int64
	ByteCapBytes        int64
	LiveContractCount   int
	LiveReplayNodeCount int
	// IgnoredUnknownArchiveCount counts archives of unknown contract ids.
	IgnoredUnknownArchiveCount  uint64
	ReplayNodesDroppedAtByteCap bool
	// DroppedReplayNodeUpdateCount counts the ReplayNode creates and archives skipped at the byte cap.
	DroppedReplayNodeUpdateCount uint64
}

// CoreContractLookupResult is a live contract and the index offset at which it is live.
type CoreContractLookupResult struct {
	contract          IndexedCoreContract
	liveAtIndexOffset int64
}

func (result CoreContractLookupResult) Contract() IndexedCoreContract { return result.contract }

func (result CoreContractLookupResult) LiveAtIndexOffset() int64 { return result.liveAtIndexOffset }

// NewCoreContractIndex returns an index that waits for its bootstrap snapshot.
// byteCapBytes bounds the blob bytes plus indexEntryOverheadBytes per entry.
func NewCoreContractIndex(guardianGovernance cantonclient.PartyID, byteCapBytes int64) (*CoreContractIndex, error) {
	if guardianGovernance == (cantonclient.PartyID{}) {
		return nil, errors.New("cantondisclosure: missing guardianGovernance party")
	}
	if byteCapBytes <= indexEntryOverheadBytes {
		return nil, fmt.Errorf("cantondisclosure: byte cap %d is not above the entry overhead %d", byteCapBytes, indexEntryOverheadBytes)
	}
	if byteCapBytes > maxIndexByteCapBytes {
		return nil, fmt.Errorf("cantondisclosure: byte cap %d exceeds %d", byteCapBytes, maxIndexByteCapBytes)
	}
	singletonContractIDsByTemplate := make(map[cantonclient.CoreContractTemplate]map[cantonclient.ContractID]struct{}, len(singletonTemplates))
	for _, template := range singletonTemplates {
		singletonContractIDsByTemplate[template] = map[cantonclient.ContractID]struct{}{}
	}
	return &CoreContractIndex{
		guardianGovernance:              guardianGovernance,
		byteCapBytes:                    byteCapBytes,
		lifecycle:                       indexLifecycleAwaitingBootstrap,
		contractsByID:                   map[cantonclient.ContractID]IndexedCoreContract{},
		singletonContractIDsByTemplate:  singletonContractIDsByTemplate,
		replayNodeContractIDsByLocation: map[replayNodeLocation][]cantonclient.ContractID{},
	}, nil
}

// ApplyBootstrapSnapshot applies the active core contracts at offset. Call it once, before ApplyTransaction.
func (index *CoreContractIndex) ApplyBootstrapSnapshot(offset int64, snapshot []cantonclient.CreatedCoreContract) error {
	if len(snapshot) > cantonclient.MaxActiveCoreContractsPerSnapshot {
		return fmt.Errorf("cantondisclosure: bootstrap snapshot of %d contracts exceeds %d", len(snapshot), cantonclient.MaxActiveCoreContractsPerSnapshot)
	}
	contracts, err := index.indexedContractsFromSnapshot(snapshot)
	if err != nil {
		return fmt.Errorf("cantondisclosure: bootstrap snapshot at offset %d: %w", offset, err)
	}
	return index.applyBootstrapSnapshot(offset, contracts, false)
}

// ApplyBootstrapSnapshotWithoutReplayNodes applies the singletons of a snapshot that has more
// ReplayNodes than the byte cap admits. The index starts with its ReplayNodes dropped (IndexReadinessByteCapReached).
func (index *CoreContractIndex) ApplyBootstrapSnapshotWithoutReplayNodes(offset int64, singletons []cantonclient.CreatedCoreContract) error {
	if len(singletons) > cantonclient.MaxActiveCoreContractsPerSnapshot {
		return fmt.Errorf("cantondisclosure: bootstrap snapshot of %d contracts exceeds %d", len(singletons), cantonclient.MaxActiveCoreContractsPerSnapshot)
	}
	contracts, err := index.indexedContractsFromSnapshot(singletons)
	if err != nil {
		return fmt.Errorf("cantondisclosure: bootstrap snapshot at offset %d: %w", offset, err)
	}
	for contractIndex, contract := range contracts {
		if !isSingletonTemplate(contract.template) {
			return fmt.Errorf("cantondisclosure: bootstrap snapshot at offset %d: contract %d: %s in a snapshot without replay nodes", offset, contractIndex, contract.template.EntityName())
		}
	}
	return index.applyBootstrapSnapshot(offset, contracts, true)
}

// ApplyTransaction applies every update of the transaction at offset in one step.
// offset must exceed the index offset, and every update must carry offset.
//
// SECURITY: a duplicate create, an archive whose template differs from the live
// entry, or a singleton above the byte cap makes the index terminal (inconsistent),
// and every lookup then fails. A create at the byte cap drops every ReplayNode;
// singleton lookups continue.
func (index *CoreContractIndex) ApplyTransaction(offset int64, updates []cantonclient.CoreContractUpdate) error {
	if len(updates) < 1 || len(updates) > maxCoreContractUpdatesPerTransaction {
		return fmt.Errorf("%w: %d updates at offset %d, expected [1, %d]", ErrTransactionUpdateCount, len(updates), offset, maxCoreContractUpdatesPerTransaction)
	}
	indexUpdates := make([]coreContractIndexUpdate, 0, len(updates))
	for updateIndex, update := range updates {
		indexUpdate, err := indexUpdateFromCoreContractUpdate(update, offset, index.guardianGovernance)
		if err != nil {
			return fmt.Errorf("cantondisclosure: transaction at offset %d: update %d: %w", offset, updateIndex, err)
		}
		indexUpdates = append(indexUpdates, indexUpdate)
	}
	return index.applyTransactionUpdates(offset, indexUpdates)
}

// AdvanceOffset moves the index offset to offset without an update: an offset
// checkpoint, or a transaction without core contract events.
func (index *CoreContractIndex) AdvanceOffset(offset int64) error {
	index.mutex.Lock()
	defer index.mutex.Unlock()
	if err := index.checkAcceptsTransactionsLocked(); err != nil {
		return err
	}
	if offset <= index.offset {
		return fmt.Errorf("%w: offset %d after index offset %d", ErrOffsetNotIncreasing, offset, index.offset)
	}
	index.offset = offset
	index.checkInvariantsLocked()
	return nil
}

// SuspendLookupsUntilReplaced fails every lookup from now on with IndexReadinessResnapshotDifference.
// The index keeps applying updates until the writer swaps in its replacement.
func (index *CoreContractIndex) SuspendLookupsUntilReplaced() {
	index.mutex.Lock()
	defer index.mutex.Unlock()
	if index.lifecycle == indexLifecycleAwaitingBootstrap {
		panic("cantondisclosure: suspend the lookups of an index without a bootstrap snapshot")
	}
	index.lookupsSuspended = true
	if index.readinessLocked().ServesSingletonLookups() {
		panic("cantondisclosure: suspended index serves lookups")
	}
}

// CompareWithSnapshot compares the live index with a snapshot read at the index offset.
func (index *CoreContractIndex) CompareWithSnapshot(snapshotOffset int64, snapshot []cantonclient.CreatedCoreContract) (SnapshotComparison, error) {
	if len(snapshot) > cantonclient.MaxActiveCoreContractsPerSnapshot {
		return SnapshotComparison{}, fmt.Errorf("cantondisclosure: snapshot of %d contracts exceeds %d", len(snapshot), cantonclient.MaxActiveCoreContractsPerSnapshot)
	}
	contracts, err := index.indexedContractsFromSnapshot(snapshot)
	if err != nil {
		return SnapshotComparison{}, fmt.Errorf("cantondisclosure: snapshot at offset %d: %w", snapshotOffset, err)
	}
	return index.compareWithIndexedSnapshot(snapshotOffset, contracts)
}

func (index *CoreContractIndex) indexedContractsFromSnapshot(snapshot []cantonclient.CreatedCoreContract) ([]IndexedCoreContract, error) {
	contracts := make([]IndexedCoreContract, 0, len(snapshot))
	for contractIndex, created := range snapshot {
		contract, err := indexedCoreContractFromCreated(created, index.guardianGovernance)
		if err != nil {
			return nil, fmt.Errorf("contract %d: %w", contractIndex, err)
		}
		contracts = append(contracts, contract)
	}
	if len(contracts) != len(snapshot) {
		panic("cantondisclosure: snapshot conversion lost contracts")
	}
	return contracts, nil
}

func (index *CoreContractIndex) applyBootstrapSnapshot(offset int64, contracts []IndexedCoreContract, replayNodesDropped bool) error {
	index.mutex.Lock()
	defer index.mutex.Unlock()
	if index.lifecycle != indexLifecycleAwaitingBootstrap {
		return fmt.Errorf("%w (readiness %s)", ErrIndexAlreadyBootstrapped, index.readinessLocked())
	}
	if offset < 0 {
		return fmt.Errorf("cantondisclosure: negative bootstrap offset %d", offset)
	}
	if len(contracts) > cantonclient.MaxActiveCoreContractsPerSnapshot {
		return fmt.Errorf("cantondisclosure: bootstrap snapshot of %d contracts exceeds %d", len(contracts), cantonclient.MaxActiveCoreContractsPerSnapshot)
	}
	if replayNodesDropped && countSingletonContracts(contracts) != len(contracts) {
		panic("cantondisclosure: bootstrap without replay nodes received a replay node")
	}
	index.replayNodesDroppedAtByteCap = replayNodesDropped
	for contractIndex, contract := range contracts {
		if err := index.insertContractLocked(contract); err != nil {
			return fmt.Errorf("cantondisclosure: bootstrap snapshot at offset %d: contract %d: %w", offset, contractIndex, err)
		}
	}
	index.offset = offset
	index.lifecycle = indexLifecycleLive
	expectedLiveContractCount := len(contracts)
	if index.replayNodesDroppedAtByteCap {
		expectedLiveContractCount = countSingletonContracts(contracts)
	}
	if len(index.contractsByID) != expectedLiveContractCount {
		panic("cantondisclosure: bootstrap live contract count differs from the snapshot")
	}
	index.checkInvariantsLocked()
	return nil
}

func countSingletonContracts(contracts []IndexedCoreContract) int {
	singletonCount := 0
	for _, contract := range contracts {
		if isSingletonTemplate(contract.template) {
			singletonCount++
		}
	}
	return singletonCount
}

func (index *CoreContractIndex) applyTransactionUpdates(offset int64, updates []coreContractIndexUpdate) error {
	index.mutex.Lock()
	defer index.mutex.Unlock()
	if err := index.checkAcceptsTransactionsLocked(); err != nil {
		return err
	}
	if offset <= index.offset {
		return fmt.Errorf("%w: offset %d after index offset %d", ErrOffsetNotIncreasing, offset, index.offset)
	}
	if len(updates) < 1 || len(updates) > maxCoreContractUpdatesPerTransaction {
		return fmt.Errorf("%w: %d updates at offset %d, expected [1, %d]", ErrTransactionUpdateCount, len(updates), offset, maxCoreContractUpdatesPerTransaction)
	}
	for updateIndex, update := range updates {
		if update.kind != cantonclient.CoreContractUpdateKindCreated && update.kind != cantonclient.CoreContractUpdateKindArchived {
			return fmt.Errorf("cantondisclosure: transaction at offset %d: update %d has unknown kind %d", offset, updateIndex, int(update.kind))
		}
	}
	for updateIndex, update := range updates {
		var err error
		if update.kind == cantonclient.CoreContractUpdateKindCreated {
			err = index.insertContractLocked(update.created)
		} else {
			err = index.removeArchivedContractLocked(update.archived)
		}
		if err != nil {
			return fmt.Errorf("cantondisclosure: transaction at offset %d: update %d: %w", offset, updateIndex, err)
		}
	}
	index.offset = offset
	index.checkInvariantsLocked()
	return nil
}

func (index *CoreContractIndex) checkAcceptsTransactionsLocked() error {
	switch index.lifecycle {
	case indexLifecycleAwaitingBootstrap:
		return ErrIndexNotBootstrapped
	case indexLifecycleLive:
		return nil
	case indexLifecycleInconsistent:
		return IndexNotReadyError{Readiness: index.readinessLocked()}
	default:
		panic(fmt.Sprintf("cantondisclosure: unknown index lifecycle %d", int(index.lifecycle)))
	}
}

// insertContractLocked refuses a live contract id and makes the index terminal.
// A ReplayNode create at the byte cap drops every ReplayNode. A singleton create
// at the byte cap drops every ReplayNode, and makes the index terminal if it still does not fit.
func (index *CoreContractIndex) insertContractLocked(contract IndexedCoreContract) error {
	if _, isLive := index.contractsByID[contract.contractID]; isLive {
		index.lifecycle = indexLifecycleInconsistent
		return fmt.Errorf("%w: %s %s", ErrDuplicateCreate, contract.template, contract.contractID)
	}
	location, isReplayNode := contract.replayNodeLocationOf()
	if isReplayNode && index.replayNodesDroppedAtByteCap {
		index.droppedReplayNodeUpdateCount++
		return nil
	}
	entryBytes := contract.indexEntryByteCount()
	if entryBytes > index.byteCapBytes-index.usedBytes {
		index.dropReplayNodesLocked()
		if isReplayNode {
			index.droppedReplayNodeUpdateCount++
			return nil
		}
		if entryBytes > index.byteCapBytes-index.usedBytes {
			index.lifecycle = indexLifecycleInconsistent
			return ByteCapExceededError{UsedBytes: index.usedBytes, EntryBytes: entryBytes, CapBytes: index.byteCapBytes}
		}
	}
	index.contractsByID[contract.contractID] = contract
	index.usedBytes += entryBytes
	if isReplayNode {
		index.replayNodeContractIDsByLocation[location] = append(index.replayNodeContractIDsByLocation[location], contract.contractID)
		index.liveReplayNodeCount++
	} else {
		index.singletonContractIDsByTemplate[contract.template][contract.contractID] = struct{}{}
	}
	if index.usedBytes > index.byteCapBytes {
		panic("cantondisclosure: used bytes exceed the byte cap after an insert")
	}
	return nil
}

// dropReplayNodesLocked removes every live ReplayNode and stops ReplayNode tracking until a rebuild.
func (index *CoreContractIndex) dropReplayNodesLocked() {
	droppedContractCount := 0
	for contractID, contract := range index.contractsByID {
		if _, isReplayNode := contract.replayNodeLocationOf(); !isReplayNode {
			continue
		}
		delete(index.contractsByID, contractID)
		index.usedBytes -= contract.indexEntryByteCount()
		droppedContractCount++
	}
	if droppedContractCount != index.liveReplayNodeCount {
		panic(fmt.Sprintf("cantondisclosure: dropped %d replay nodes, but %d were live", droppedContractCount, index.liveReplayNodeCount))
	}
	clear(index.replayNodeContractIDsByLocation)
	index.liveReplayNodeCount = 0
	index.replayNodesDroppedAtByteCap = true
	if index.usedBytes < 0 {
		panic("cantondisclosure: used bytes are negative after dropping the replay nodes")
	}
}

// removeArchivedContractLocked matches by contract id. An unknown contract id is counted and ignored:
// the index serves only the contracts it holds, and the snapshot comparison reports any drift.
func (index *CoreContractIndex) removeArchivedContractLocked(archived archivedCoreContract) error {
	contract, isLive := index.contractsByID[archived.contractID]
	if !isLive {
		if index.replayNodesDroppedAtByteCap && archived.template == cantonclient.CoreContractTemplateReplayNode {
			index.droppedReplayNodeUpdateCount++
			return nil
		}
		index.ignoredUnknownArchiveCount++
		return nil
	}
	if contract.template != archived.template {
		index.lifecycle = indexLifecycleInconsistent
		return fmt.Errorf("%w: archive of %s names template %s, live entry is %s", ErrIndexInconsistent, archived.contractID, archived.template, contract.template)
	}
	delete(index.contractsByID, contract.contractID)
	index.usedBytes -= contract.indexEntryByteCount()
	if location, isReplayNode := contract.replayNodeLocationOf(); isReplayNode {
		index.removeReplayNodeLocationLocked(location, contract.contractID)
		index.liveReplayNodeCount--
	} else {
		delete(index.singletonContractIDsByTemplate[contract.template], contract.contractID)
	}
	if index.usedBytes < 0 {
		panic("cantondisclosure: used bytes are negative after an archive")
	}
	return nil
}

func (index *CoreContractIndex) removeReplayNodeLocationLocked(location replayNodeLocation, contractID cantonclient.ContractID) {
	contractIDs := index.replayNodeContractIDsByLocation[location]
	remainingContractIDs := make([]cantonclient.ContractID, 0, len(contractIDs))
	for _, locationContractID := range contractIDs {
		if locationContractID != contractID {
			remainingContractIDs = append(remainingContractIDs, locationContractID)
		}
	}
	if len(remainingContractIDs) != len(contractIDs)-1 {
		panic(fmt.Sprintf("cantondisclosure: replay node %s is not listed exactly once at its location", contractID))
	}
	if len(remainingContractIDs) == 0 {
		delete(index.replayNodeContractIDsByLocation, location)
		return
	}
	index.replayNodeContractIDsByLocation[location] = remainingContractIDs
}

func (index *CoreContractIndex) checkInvariantsLocked() {
	if index.usedBytes < 0 || index.usedBytes > index.byteCapBytes {
		panic(fmt.Sprintf("cantondisclosure: used bytes %d outside [0, %d]", index.usedBytes, index.byteCapBytes))
	}
	singletonCount := 0
	for _, contractIDs := range index.singletonContractIDsByTemplate {
		singletonCount += len(contractIDs)
	}
	if len(index.contractsByID) != singletonCount+index.liveReplayNodeCount {
		panic(fmt.Sprintf("cantondisclosure: %d live contracts, but %d singletons and %d replay nodes", len(index.contractsByID), singletonCount, index.liveReplayNodeCount))
	}
	if len(index.singletonContractIDsByTemplate) != len(singletonTemplates) {
		panic("cantondisclosure: singleton template set changed")
	}
	if index.replayNodesDroppedAtByteCap && index.liveReplayNodeCount != 0 {
		panic("cantondisclosure: replay nodes are live after the index dropped them")
	}
}

// checkUsedBytesMatchLiveContractsLocked recomputes the used bytes from every live entry.
func (index *CoreContractIndex) checkUsedBytesMatchLiveContractsLocked() {
	recomputedUsedBytes := int64(0)
	for _, contract := range index.contractsByID {
		recomputedUsedBytes += contract.indexEntryByteCount()
	}
	if recomputedUsedBytes != index.usedBytes {
		panic(fmt.Sprintf("cantondisclosure: used bytes %d, but the live entries sum to %d", index.usedBytes, recomputedUsedBytes))
	}
}

func (index *CoreContractIndex) readinessLocked() IndexReadiness {
	switch index.lifecycle {
	case indexLifecycleAwaitingBootstrap:
		return IndexReadinessBootstrapping
	case indexLifecycleInconsistent:
		return IndexReadinessInconsistent
	case indexLifecycleLive:
	default:
		panic(fmt.Sprintf("cantondisclosure: unknown index lifecycle %d", int(index.lifecycle)))
	}
	if index.lookupsSuspended {
		return IndexReadinessResnapshotDifference
	}
	hasSingletonConflict := false
	hasMissingSingleton := false
	for _, template := range singletonTemplates {
		liveCount := len(index.singletonContractIDsByTemplate[template])
		hasSingletonConflict = hasSingletonConflict || liveCount > 1
		hasMissingSingleton = hasMissingSingleton || liveCount == 0
	}
	if hasSingletonConflict {
		return IndexReadinessSingletonConflict
	}
	if hasMissingSingleton {
		return IndexReadinessSingletonMissing
	}
	if index.replayNodesDroppedAtByteCap {
		return IndexReadinessByteCapReached
	}
	if index.usedBytes*100 >= index.byteCapBytes*byteCapWarningPercent {
		return IndexReadinessReadyNearByteCap
	}
	return IndexReadinessReady
}

func (index *CoreContractIndex) checkServesSingletonLookupsLocked() error {
	readiness := index.readinessLocked()
	if !readiness.ServesSingletonLookups() {
		return IndexNotReadyError{Readiness: readiness}
	}
	return nil
}

func (index *CoreContractIndex) checkServesReplayNodeLookupsLocked() error {
	readiness := index.readinessLocked()
	if !readiness.ServesReplayNodeLookups() {
		return IndexNotReadyError{Readiness: readiness}
	}
	return nil
}

// Status is a copy of the index counters.
func (index *CoreContractIndex) Status() IndexStatus {
	index.mutex.RLock()
	defer index.mutex.RUnlock()
	return IndexStatus{
		Readiness:                    index.readinessLocked(),
		Offset:                       index.offset,
		UsedBytes:                    index.usedBytes,
		ByteCapBytes:                 index.byteCapBytes,
		LiveContractCount:            len(index.contractsByID),
		LiveReplayNodeCount:          index.liveReplayNodeCount,
		IgnoredUnknownArchiveCount:   index.ignoredUnknownArchiveCount,
		ReplayNodesDroppedAtByteCap:  index.replayNodesDroppedAtByteCap,
		DroppedReplayNodeUpdateCount: index.droppedReplayNodeUpdateCount,
	}
}

// LookupSingleton returns the one live CoreState, EmitterRegistry or ReplayRootRegistry.
func (index *CoreContractIndex) LookupSingleton(template cantonclient.CoreContractTemplate) (CoreContractLookupResult, error) {
	if !isSingletonTemplate(template) {
		return CoreContractLookupResult{}, fmt.Errorf("cantondisclosure: template %d is not a singleton core template", int(template))
	}
	index.mutex.RLock()
	defer index.mutex.RUnlock()
	if err := index.checkServesSingletonLookupsLocked(); err != nil {
		return CoreContractLookupResult{}, err
	}
	contractIDs := index.singletonContractIDsByTemplate[template]
	if len(contractIDs) != 1 {
		panic(fmt.Sprintf("cantondisclosure: ready index has %d live %s", len(contractIDs), template))
	}
	for contractID := range contractIDs {
		return index.lookupResultLocked(contractID), nil
	}
	panic("unreachable: a set of one contract id yields one contract id")
}

// LookupCoveringReplayNode returns the one live ReplayNode of the (consumer, namespace) trie that covers digest.
func (index *CoreContractIndex) LookupCoveringReplayNode(consumer cantonclient.PartyID, namespace cantonclient.ReplayNamespace, digest ReplayDigest) (CoreContractLookupResult, error) {
	if consumer == (cantonclient.PartyID{}) || namespace == (cantonclient.ReplayNamespace{}) || digest == (ReplayDigest{}) {
		return CoreContractLookupResult{}, errors.New("cantondisclosure: covering replay node lookup needs a consumer, a namespace and a digest")
	}
	index.mutex.RLock()
	defer index.mutex.RUnlock()
	if err := index.checkServesReplayNodeLookupsLocked(); err != nil {
		return CoreContractLookupResult{}, err
	}
	return index.lookupCoveringReplayNodeLocked(consumer, namespace, digest)
}

// LookupClaimedRootsNode returns the node of the claimed-roots tree that ClaimReplayRoot of (consumer, namespace) consumes in:
// consumer guardianGovernance, namespace claimedRootsNamespace, digest ClaimedRootDigest(consumer, namespace).
func (index *CoreContractIndex) LookupClaimedRootsNode(consumer cantonclient.PartyID, namespace cantonclient.ReplayNamespace) (CoreContractLookupResult, error) {
	if consumer == (cantonclient.PartyID{}) || namespace == (cantonclient.ReplayNamespace{}) {
		return CoreContractLookupResult{}, errors.New("cantondisclosure: claimed-roots node lookup needs a consumer and a namespace")
	}
	if namespace.String() == claimedRootsNamespace {
		return CoreContractLookupResult{}, ErrReservedReplayNamespace
	}
	claimedRootsReplayNamespace, err := cantonclient.ParseReplayNamespace(claimedRootsNamespace)
	if err != nil {
		panic(fmt.Sprintf("cantondisclosure: claimed-roots namespace: %v", err))
	}
	pairDigest := ClaimedRootDigest(consumer, namespace)
	index.mutex.RLock()
	defer index.mutex.RUnlock()
	if err := index.checkServesReplayNodeLookupsLocked(); err != nil {
		return CoreContractLookupResult{}, err
	}
	return index.lookupCoveringReplayNodeLocked(index.guardianGovernance, claimedRootsReplayNamespace, pairDigest)
}

// lookupCoveringReplayNodeLocked checks every prefix depth 0 to maxReplayNodePrefixNibbles,
// so that two covering nodes at different depths are an error.
func (index *CoreContractIndex) lookupCoveringReplayNodeLocked(consumer cantonclient.PartyID, namespace cantonclient.ReplayNamespace, digest ReplayDigest) (CoreContractLookupResult, error) {
	coveringNodeCount := 0
	var firstCoveringContractID cantonclient.ContractID
	checkedPrefixCount := 0
	for nibbleCount := 0; nibbleCount < coveringPrefixCandidateCount; nibbleCount++ {
		prefix, err := digest.prefixOfNibbleCount(nibbleCount)
		if err != nil {
			panic(fmt.Sprintf("cantondisclosure: prefix of a parsed digest: %v", err))
		}
		contractIDs := index.replayNodeContractIDsByLocation[replayNodeLocation{consumer: consumer, namespace: namespace, prefix: prefix}]
		if coveringNodeCount == 0 && len(contractIDs) > 0 {
			firstCoveringContractID = contractIDs[0]
		}
		coveringNodeCount += len(contractIDs)
		checkedPrefixCount++
	}
	if checkedPrefixCount != coveringPrefixCandidateCount {
		panic("cantondisclosure: covering lookup skipped a prefix depth")
	}
	switch coveringNodeCount {
	case 0:
		return CoreContractLookupResult{}, ErrNoCoveringReplayNode
	case 1:
	default:
		return CoreContractLookupResult{}, MultipleCoveringReplayNodesError{CoveringNodeCount: coveringNodeCount}
	}
	result := index.lookupResultLocked(firstCoveringContractID)
	location, isReplayNode := result.contract.replayNodeLocationOf()
	if !isReplayNode || location.consumer != consumer || location.namespace != namespace {
		panic("cantondisclosure: covering node is not a replay node of the requested trie")
	}
	if !strings.HasPrefix(digest.String(), location.prefix.String()) {
		panic("cantondisclosure: covering node prefix does not prefix the digest")
	}
	return result, nil
}

func (index *CoreContractIndex) lookupResultLocked(contractID cantonclient.ContractID) CoreContractLookupResult {
	contract, isLive := index.contractsByID[contractID]
	if !isLive {
		panic(fmt.Sprintf("cantondisclosure: lookup key names %s, which is not live", contractID))
	}
	if contract.contractID != contractID {
		panic("cantondisclosure: live entry has another contract id than its key")
	}
	return CoreContractLookupResult{contract: contract, liveAtIndexOffset: index.offset}
}

func (index *CoreContractIndex) compareWithIndexedSnapshot(snapshotOffset int64, snapshot []IndexedCoreContract) (SnapshotComparison, error) {
	index.mutex.RLock()
	defer index.mutex.RUnlock()
	if index.lifecycle == indexLifecycleAwaitingBootstrap {
		return SnapshotComparison{}, ErrIndexNotBootstrapped
	}
	if index.replayNodesDroppedAtByteCap {
		return SnapshotComparison{}, ErrReplayNodesDroppedAtByteCap
	}
	if snapshotOffset != index.offset {
		return SnapshotComparison{}, fmt.Errorf("%w: snapshot offset %d, index offset %d", ErrSnapshotOffsetMismatch, snapshotOffset, index.offset)
	}
	index.checkUsedBytesMatchLiveContractsLocked()
	return compareSnapshotWithLiveContracts(snapshotOffset, snapshot, index.contractsByID)
}
