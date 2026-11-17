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

// packageMetadataRecheckTicks is the number of 5 s ledger-end ticks between metadata rechecks (60 s).
const packageMetadataRecheckTicks = 12

var errPackageMetadataMismatched = errors.New("canton: vetted package metadata does not match the expected wormhole-core name and version")

var packageMetadataVerifiedGauge = promauto.NewGaugeVec(
	prometheus.GaugeOpts{
		Name: "wormhole_canton_package_metadata_verified",
		Help: "1 when the participant's vetting metadata for the pinned core package id of this role shows wormhole-core at the expected version; 0 while not yet vetted",
	}, []string{"role"})

// packageMetadataVerification records which pinned roles passed the metadata check.
type packageMetadataVerification struct {
	mu            sync.Mutex
	verifiedRoles map[cantonclient.AcceptedPackageRole]bool
}

func (verification *packageMetadataVerification) setVerified(role cantonclient.AcceptedPackageRole, verified bool) {
	verification.mu.Lock()
	defer verification.mu.Unlock()
	if verification.verifiedRoles == nil {
		verification.verifiedRoles = make(map[cantonclient.AcceptedPackageRole]bool, 2)
	}
	verification.verifiedRoles[role] = verified
}

func (verification *packageMetadataVerification) isVerified(role cantonclient.AcceptedPackageRole) bool {
	verification.mu.Lock()
	defer verification.mu.Unlock()
	return verification.verifiedRoles[role]
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
			e.packageMetadata.setVerified(pinned.Role, true)
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
		switch verdict {
		case cantonclient.PackageMetadataVerdictVerified:
			e.packageMetadata.setVerified(pinned.Role, true)
			packageMetadataVerifiedGauge.WithLabelValues(pinned.Role.String()).Set(1)
		case cantonclient.PackageMetadataVerdictNotYetVetted:
			e.packageMetadata.setVerified(pinned.Role, false)
			packageMetadataVerifiedGauge.WithLabelValues(pinned.Role.String()).Set(0)
			logger.Warn("canton: pinned core package id has no vetting metadata on this participant yet",
				zap.Stringer("role", pinned.Role),
				zap.Stringer("packageId", pinned.PackageID),
				zap.Stringer("expectedVersion", pinned.ExpectedVersion))
		case cantonclient.PackageMetadataVerdictMismatched:
			e.packageMetadata.setVerified(pinned.Role, false)
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

// checkPublishPackageVerified admits a publish whose executing role passed
// the metadata check, rechecking once if it has not. A publish that
// guardianGovernance sees ran under a package vetted here, so its metadata
// must exist on this participant.
func (e *Watcher) checkPublishPackageVerified(ctx context.Context, logger *zap.Logger, client cantonclient.CantonClient, role cantonclient.AcceptedPackageRole) error {
	if e.acceptedPackageIDs.MatchesAnyPackage() || e.packageMetadata.isVerified(role) {
		return nil
	}
	if err := e.refreshPackageMetadata(ctx, logger, client); err != nil {
		return err
	}
	if !e.packageMetadata.isVerified(role) {
		return fmt.Errorf("canton: the %s core package id has no matching vetting metadata on this participant", role)
	}
	return nil
}

// rewindResumeOffsetBefore makes the next Run resubscribe at or before participantOffset.
func (e *Watcher) rewindResumeOffsetBefore(participantOffset int64) {
	if participantOffset <= 0 {
		panic("canton: participant offset of a streamed event must be positive")
	}
	if last := e.lastOffset.Load(); last == 0 || last >= participantOffset {
		e.lastOffset.Store(participantOffset - 1)
	}
}
