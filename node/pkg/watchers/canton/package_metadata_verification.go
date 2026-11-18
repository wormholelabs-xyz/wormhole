package canton

import (
	"context"
	"errors"
	"fmt"
	"sync"

	"github.com/certusone/wormhole/node/pkg/cantonclient"
	"github.com/prometheus/client_golang/prometheus"
	"github.com/prometheus/client_golang/prometheus/promauto"
	"go.uber.org/zap"
)

var (
	errPackageMetadataMismatched = errors.New("canton: vetted package metadata does not match the expected wormhole-core name and version")
	// errPackageMetadataNeverVerifiedInDevMode refuses one publish; the watcher keeps running.
	errPackageMetadataNeverVerifiedInDevMode = errors.New("canton: unsafe dev mode emits a core package role only after one vetting metadata match")
)

var packageMetadataVerifiedGauge = promauto.NewGaugeVec(
	prometheus.GaugeOpts{
		Name: "wormhole_canton_package_metadata_verified",
		Help: "1 when the participant's vetting metadata for the pinned core package id of this role shows wormhole-core at the expected version; 0 while the participant does not vet the id (not yet, or after the unvet)",
	}, []string{"role"})

// packageMetadataVerification records the latest metadata verdict of each
// pinned role, and the roles that had a Verified verdict at least once.
type packageMetadataVerification struct {
	mu                sync.Mutex
	latestVerdicts    map[cantonclient.AcceptedPackageRole]cantonclient.PackageMetadataVerdict
	verifiedOnceRoles map[cantonclient.AcceptedPackageRole]bool
}

func (verification *packageMetadataVerification) recordVerdict(role cantonclient.AcceptedPackageRole, verdict cantonclient.PackageMetadataVerdict) {
	verification.mu.Lock()
	defer verification.mu.Unlock()
	if verification.latestVerdicts == nil {
		verification.latestVerdicts = make(map[cantonclient.AcceptedPackageRole]cantonclient.PackageMetadataVerdict, 2)
		verification.verifiedOnceRoles = make(map[cantonclient.AcceptedPackageRole]bool, 2)
	}
	verification.latestVerdicts[role] = verdict
	if verdict == cantonclient.PackageMetadataVerdictVerified {
		verification.verifiedOnceRoles[role] = true
	}
}

// wasVerifiedOnce stays true after an unvet: a package id is the hash of the
// package, so its name and version cannot change.
func (verification *packageMetadataVerification) wasVerifiedOnce(role cantonclient.AcceptedPackageRole) bool {
	verification.mu.Lock()
	defer verification.mu.Unlock()
	return verification.verifiedOnceRoles[role]
}

func (verification *packageMetadataVerification) isMismatched(role cantonclient.AcceptedPackageRole) bool {
	verification.mu.Lock()
	defer verification.mu.Unlock()
	return verification.latestVerdicts[role] == cantonclient.PackageMetadataVerdictMismatched
}

// refreshPackageMetadata checks every pinned id against the participant's
// vetting metadata. It returns errPackageMetadataMismatched (wrapped) on a
// mismatch, and the RPC error if the participant cannot answer. A pinned id
// without an expected version (unsafe dev mode only) counts as verified.
func (e *Watcher) refreshPackageMetadata(ctx context.Context, logger *zap.Logger, client cantonclient.CantonClient) error {
	pinnedPackages := e.acceptedPackageIDs.PinnedCorePackages()
	packageIDsToCheck := make([]cantonclient.PackageID, 0, len(pinnedPackages))
	for _, pinned := range pinnedPackages {
		if pinned.ExpectedVersion.IsZero() {
			if !e.unsafeDevMode {
				panic("canton: pinned core package id without an expected version outside unsafe dev mode")
			}
			e.packageMetadata.recordVerdict(pinned.Role, cantonclient.PackageMetadataVerdictVerified)
			continue
		}
		packageIDsToCheck = append(packageIDsToCheck, pinned.PackageID)
	}
	if len(packageIDsToCheck) == 0 {
		return nil
	}
	entries, err := client.ListVettedPackageMetadata(ctx, packageIDsToCheck)
	if err != nil {
		return fmt.Errorf("canton: failed to list vetted package metadata: %w", err)
	}
	for _, pinned := range pinnedPackages {
		if pinned.ExpectedVersion.IsZero() {
			continue
		}
		verdict := cantonclient.CheckVettedPackageMatchesExpected(entries, pinned.PackageID, pinned.ExpectedVersion)
		e.packageMetadata.recordVerdict(pinned.Role, verdict)
		switch verdict {
		case cantonclient.PackageMetadataVerdictVerified:
			packageMetadataVerifiedGauge.WithLabelValues(pinned.Role.String()).Set(1)
		case cantonclient.PackageMetadataVerdictNotYetVetted:
			packageMetadataVerifiedGauge.WithLabelValues(pinned.Role.String()).Set(0)
			logger.Warn("canton: the participant has no vetting metadata for a pinned core package id (not vetted yet, or unvetted)",
				zap.Stringer("role", pinned.Role),
				zap.Stringer("packageId", pinned.PackageID),
				zap.Stringer("expectedVersion", pinned.ExpectedVersion),
				zap.Bool("verifiedOnce", e.packageMetadata.wasVerifiedOnce(pinned.Role)),
				zap.Bool("unsafeDevMode", e.unsafeDevMode))
		case cantonclient.PackageMetadataVerdictMismatched:
			packageMetadataVerifiedGauge.WithLabelValues(pinned.Role.String()).Set(0)
			logger.Error("canton: pinned core package id is vetted with a different name or version",
				zap.Stringer("role", pinned.Role),
				zap.Stringer("packageId", pinned.PackageID),
				zap.String("expectedName", cantonclient.CorePackageName),
				zap.Stringer("expectedVersion", pinned.ExpectedVersion))
			return fmt.Errorf("%w: role %s, package id %s", errPackageMetadataMismatched, pinned.Role, pinned.PackageID)
		default:
			panic("canton: unknown package metadata verdict")
		}
	}
	return nil
}

// checkPublishPackageAdmitted refuses a publish of a role whose metadata
// mismatched. Outside unsafe dev mode a role without vetting metadata is
// admitted: the approved list binds the id to its version, and the unvet must
// keep pre-deadline messages recoverable (canton/README.md section 7.5).
//
// SECURITY: unsafe dev mode has no approved list. There a role needs one
// Verified verdict in the life of this Watcher; until then the publish is
// refused with errPackageMetadataNeverVerifiedInDevMode.
func (e *Watcher) checkPublishPackageAdmitted(ctx context.Context, logger *zap.Logger, client cantonclient.CantonClient, role cantonclient.AcceptedPackageRole) error {
	if e.acceptedPackageIDs.MatchesAnyPackage() {
		return nil
	}
	if e.packageMetadata.isMismatched(role) {
		return fmt.Errorf("%w: role %s", errPackageMetadataMismatched, role)
	}
	if e.packageMetadata.wasVerifiedOnce(role) {
		return nil
	}
	if !e.unsafeDevMode {
		return nil
	}
	if err := e.refreshPackageMetadata(ctx, logger, client); err != nil {
		return err
	}
	if !e.packageMetadata.wasVerifiedOnce(role) {
		return fmt.Errorf("%w: role %s", errPackageMetadataNeverVerifiedInDevMode, role)
	}
	return nil
}

// rewindResumeOffsetBefore makes the next Run resubscribe at or before participantOffset.
func (e *Watcher) rewindResumeOffsetBefore(participantOffset int64) {
	if participantOffset <= 0 {
		panic("canton: participant offset of a streamed event must be positive")
	}
	if !e.hasResumeOffset.Load() || e.lastOffset.Load() >= participantOffset {
		e.lastOffset.Store(participantOffset - 1)
		e.hasResumeOffset.Store(true)
	}
}
