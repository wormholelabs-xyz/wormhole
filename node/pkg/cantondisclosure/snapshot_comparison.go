package cantondisclosure

import (
	"fmt"
	"slices"
	"strings"

	"github.com/certusone/wormhole/node/pkg/cantonclient"
)

// maxReportedSnapshotDifferences bounds SnapshotComparison.ReportedDifferences. The counts include every difference.
const maxReportedSnapshotDifferences = 1024

// SnapshotDifferenceKind is how a snapshot contract and the live index disagree.
type SnapshotDifferenceKind int

const (
	// SnapshotDifferenceMissingFromIndex: the snapshot has the contract, the index does not.
	SnapshotDifferenceMissingFromIndex SnapshotDifferenceKind = iota + 1
	// SnapshotDifferenceStaleInIndex: the index has the contract, the snapshot does not. A lost archive causes this.
	SnapshotDifferenceStaleInIndex
	// SnapshotDifferenceContentMismatch: both have the contract id with different template, synchronizer, location or blob.
	SnapshotDifferenceContentMismatch
)

func (kind SnapshotDifferenceKind) String() string {
	switch kind {
	case SnapshotDifferenceMissingFromIndex:
		return "missing_from_index"
	case SnapshotDifferenceStaleInIndex:
		return "stale_in_index"
	case SnapshotDifferenceContentMismatch:
		return "content_mismatch"
	default:
		return fmt.Sprintf("unknown_snapshot_difference_%d", int(kind))
	}
}

// SnapshotDifference is one contract on which a snapshot and the index disagree.
type SnapshotDifference struct {
	Kind       SnapshotDifferenceKind
	ContractID cantonclient.ContractID
	Template   cantonclient.CoreContractTemplate
}

// SnapshotComparison is the result of a comparison at one offset.
type SnapshotComparison struct {
	offset                int64
	reportedDifferences   []SnapshotDifference
	missingFromIndexCount int
	staleInIndexCount     int
	contentMismatchCount  int
}

func (comparison SnapshotComparison) Offset() int64 { return comparison.offset }

// ReportedDifferences is a copy of the first maxReportedSnapshotDifferences differences:
// snapshot order for missing and mismatched contracts, then stale contracts by contract id.
func (comparison SnapshotComparison) ReportedDifferences() []SnapshotDifference {
	return slices.Clone(comparison.reportedDifferences)
}

func (comparison SnapshotComparison) MissingFromIndexCount() int {
	return comparison.missingFromIndexCount
}

func (comparison SnapshotComparison) StaleInIndexCount() int { return comparison.staleInIndexCount }

func (comparison SnapshotComparison) ContentMismatchCount() int {
	return comparison.contentMismatchCount
}

func (comparison SnapshotComparison) MatchesIndex() bool {
	return comparison.missingFromIndexCount == 0 && comparison.staleInIndexCount == 0 && comparison.contentMismatchCount == 0
}

func (comparison *SnapshotComparison) recordDifference(difference SnapshotDifference) {
	switch difference.Kind {
	case SnapshotDifferenceMissingFromIndex:
		comparison.missingFromIndexCount++
	case SnapshotDifferenceStaleInIndex:
		comparison.staleInIndexCount++
	case SnapshotDifferenceContentMismatch:
		comparison.contentMismatchCount++
	default:
		panic(fmt.Sprintf("cantondisclosure: unknown snapshot difference kind %d", int(difference.Kind)))
	}
	if len(comparison.reportedDifferences) < maxReportedSnapshotDifferences {
		comparison.reportedDifferences = append(comparison.reportedDifferences, difference)
	}
}

// compareSnapshotWithLiveContracts compares a snapshot with the live contracts of an index at the same offset.
func compareSnapshotWithLiveContracts(offset int64, snapshot []IndexedCoreContract, liveContractsByID map[cantonclient.ContractID]IndexedCoreContract) (SnapshotComparison, error) {
	if len(snapshot) > cantonclient.MaxActiveCoreContractsPerSnapshot {
		return SnapshotComparison{}, fmt.Errorf("cantondisclosure: snapshot of %d contracts exceeds %d", len(snapshot), cantonclient.MaxActiveCoreContractsPerSnapshot)
	}
	comparison := SnapshotComparison{offset: offset}
	snapshotContractIDs := make(map[cantonclient.ContractID]struct{}, len(snapshot))
	for _, snapshotContract := range snapshot {
		if _, isRepeated := snapshotContractIDs[snapshotContract.contractID]; isRepeated {
			return SnapshotComparison{}, fmt.Errorf("%w: snapshot repeats %s", ErrDuplicateCreate, snapshotContract.contractID)
		}
		snapshotContractIDs[snapshotContract.contractID] = struct{}{}
		liveContract, isLive := liveContractsByID[snapshotContract.contractID]
		if !isLive {
			comparison.recordDifference(SnapshotDifference{Kind: SnapshotDifferenceMissingFromIndex, ContractID: snapshotContract.contractID, Template: snapshotContract.template})
			continue
		}
		if !liveContract.hasEqualContent(snapshotContract) {
			comparison.recordDifference(SnapshotDifference{Kind: SnapshotDifferenceContentMismatch, ContractID: snapshotContract.contractID, Template: liveContract.template})
		}
	}
	staleContractIDs := []cantonclient.ContractID{}
	for contractID := range liveContractsByID {
		if _, inSnapshot := snapshotContractIDs[contractID]; !inSnapshot {
			staleContractIDs = append(staleContractIDs, contractID)
		}
	}
	slices.SortFunc(staleContractIDs, func(left cantonclient.ContractID, right cantonclient.ContractID) int {
		return strings.Compare(left.String(), right.String())
	})
	for _, contractID := range staleContractIDs {
		comparison.recordDifference(SnapshotDifference{Kind: SnapshotDifferenceStaleInIndex, ContractID: contractID, Template: liveContractsByID[contractID].template})
	}
	if len(snapshotContractIDs)-comparison.missingFromIndexCount != len(liveContractsByID)-comparison.staleInIndexCount {
		panic("cantondisclosure: snapshot comparison counts disagree on the shared contracts")
	}
	return comparison, nil
}
