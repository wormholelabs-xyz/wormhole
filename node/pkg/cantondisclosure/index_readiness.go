package cantondisclosure

import (
	"errors"
	"fmt"

	"github.com/certusone/wormhole/node/pkg/cantonclient"
)

// IndexReadiness is the serving state of a CoreContractIndex.
type IndexReadiness int

const (
	// IndexReadinessBootstrapping: the index waits for its bootstrap snapshot.
	IndexReadinessBootstrapping IndexReadiness = iota + 1
	IndexReadinessReady
	// IndexReadinessReadyNearByteCap: ready, with used bytes at byteCapWarningPercent of the cap or more.
	IndexReadinessReadyNearByteCap
	// IndexReadinessSingletonMissing: a singleton template has zero live instances.
	IndexReadinessSingletonMissing
	// IndexReadinessSingletonConflict: a singleton template has two or more live instances.
	IndexReadinessSingletonConflict
	// IndexReadinessByteCapReached: a create reached the byte cap, and the index dropped every ReplayNode.
	// Singleton lookups serve; ReplayNode lookups fail until a rebuild.
	IndexReadinessByteCapReached
	// IndexReadinessInconsistent: an update contradicts the index. Terminal: rebuild the index.
	IndexReadinessInconsistent
)

func (readiness IndexReadiness) String() string {
	switch readiness {
	case IndexReadinessBootstrapping:
		return "bootstrapping"
	case IndexReadinessReady:
		return "ready"
	case IndexReadinessReadyNearByteCap:
		return "ready_near_byte_cap"
	case IndexReadinessSingletonMissing:
		return "singleton_missing"
	case IndexReadinessSingletonConflict:
		return "singleton_conflict"
	case IndexReadinessByteCapReached:
		return "byte_cap_reached"
	case IndexReadinessInconsistent:
		return "inconsistent"
	default:
		return fmt.Sprintf("unknown_index_readiness_%d", int(readiness))
	}
}

// ServesSingletonLookups is true for the ready states and IndexReadinessByteCapReached.
func (readiness IndexReadiness) ServesSingletonLookups() bool {
	return readiness.ServesReplayNodeLookups() || readiness == IndexReadinessByteCapReached
}

// ServesReplayNodeLookups is true only for the ready states.
func (readiness IndexReadiness) ServesReplayNodeLookups() bool {
	return readiness == IndexReadinessReady || readiness == IndexReadinessReadyNearByteCap
}

var (
	ErrIndexNotReady               = errors.New("cantondisclosure: core contract index is not ready")
	ErrIndexNotBootstrapped        = errors.New("cantondisclosure: core contract index has no bootstrap snapshot")
	ErrIndexAlreadyBootstrapped    = errors.New("cantondisclosure: core contract index already has a bootstrap snapshot")
	ErrIndexInconsistent           = errors.New("cantondisclosure: update contradicts the core contract index")
	ErrOffsetNotIncreasing         = errors.New("cantondisclosure: transaction offset does not increase")
	ErrTransactionUpdateCount      = errors.New("cantondisclosure: transaction update count is out of range")
	ErrDuplicateCreate             = errors.New("cantondisclosure: contract id is already live")
	ErrByteCapExceeded             = errors.New("cantondisclosure: core contract index byte cap exceeded")
	ErrNoCoveringReplayNode        = errors.New("cantondisclosure: no live replay node covers the digest")
	ErrMultipleCoveringReplayNodes = errors.New("cantondisclosure: more than one live replay node covers the digest")
	ErrReservedReplayNamespace     = errors.New("cantondisclosure: replay namespace is reserved for the claimed-roots tree")
	ErrSnapshotOffsetMismatch      = errors.New("cantondisclosure: snapshot offset is not the index offset")
	ErrSnapshotRepeatsContractID   = errors.New("cantondisclosure: snapshot repeats a contract id")
	ErrReplayNodesDroppedAtByteCap = errors.New("cantondisclosure: index dropped its replay nodes at the byte cap")
)

// IndexNotReadyError is returned by every lookup while the readiness does not serve lookups.
type IndexNotReadyError struct {
	Readiness IndexReadiness
}

func (notReady IndexNotReadyError) Error() string {
	return fmt.Sprintf("%s: %s", ErrIndexNotReady, notReady.Readiness)
}

func (notReady IndexNotReadyError) Is(target error) bool { return target == ErrIndexNotReady }

// MultipleCoveringReplayNodesError: two or more live nodes of one (consumer, namespace) trie cover the digest.
type MultipleCoveringReplayNodesError struct {
	CoveringNodeCount int
}

func (multiple MultipleCoveringReplayNodesError) Error() string {
	return fmt.Sprintf("%s: %d covering nodes", ErrMultipleCoveringReplayNodes, multiple.CoveringNodeCount)
}

func (multiple MultipleCoveringReplayNodesError) Is(target error) bool {
	return target == ErrMultipleCoveringReplayNodes
}

// ByteCapExceededError: UsedBytes plus EntryBytes exceeds CapBytes.
type ByteCapExceededError struct {
	UsedBytes  int64
	EntryBytes int64
	CapBytes   int64
}

func (exceeded ByteCapExceededError) Error() string {
	return fmt.Sprintf("%s: %d used bytes plus %d entry bytes exceeds %d", ErrByteCapExceeded, exceeded.UsedBytes, exceeded.EntryBytes, exceeded.CapBytes)
}

func (exceeded ByteCapExceededError) Is(target error) bool { return target == ErrByteCapExceeded }

// singletonTemplates are the core templates with exactly one live instance per guardianGovernance.
var singletonTemplates = [...]cantonclient.CoreContractTemplate{
	cantonclient.CoreContractTemplateCoreState,
	cantonclient.CoreContractTemplateEmitterRegistry,
	cantonclient.CoreContractTemplateReplayRootRegistry,
}

func isSingletonTemplate(template cantonclient.CoreContractTemplate) bool {
	for _, singletonTemplate := range singletonTemplates {
		if template == singletonTemplate {
			return true
		}
	}
	return false
}
