package cantonclient

import (
	"fmt"
	"time"
)

// PackageIDHexLen is the length of a Daml package id: a lowercase hex SHA-256.
const PackageIDHexLen = 64

// PackageID is a Daml package id in its exact on-ledger form.
type PackageID struct {
	hexID string
}

// ParsePackageID accepts exactly PackageIDHexLen lowercase hex characters.
func ParsePackageID(packageIDHex string) (PackageID, error) {
	if len(packageIDHex) != PackageIDHexLen {
		return PackageID{}, fmt.Errorf("package id %q must be %d lowercase hex characters, got %d characters", packageIDHex, PackageIDHexLen, len(packageIDHex))
	}
	for _, character := range []byte(packageIDHex) {
		isLowercaseHex := (character >= '0' && character <= '9') || (character >= 'a' && character <= 'f')
		if !isLowercaseHex {
			return PackageID{}, fmt.Errorf("package id %q must be %d lowercase hex characters, found %q", packageIDHex, PackageIDHexLen, character)
		}
	}
	return PackageID{hexID: packageIDHex}, nil
}

func (id PackageID) String() string { return id.hexID }

func (id PackageID) isZero() bool { return id.hexID == "" }

// MaxRetiringPackageWindow bounds a retiring id's deadline minus the guardian's startup wall clock.
const MaxRetiringPackageWindow = 7 * 24 * time.Hour

// The window is at least one hour and at most 30 days (typed-constant underflow fails to compile).
const (
	_ = uint64(MaxRetiringPackageWindow - time.Hour)
	_ = uint64(30*24*time.Hour - MaxRetiringPackageWindow)
)

// RetiringPackageDeadlineLayout is RFC 3339 in UTC with whole seconds.
const RetiringPackageDeadlineLayout = "2006-01-02T15:04:05Z"

// RetiringPackageID is a core package id the watcher accepts only for
// transactions with a record time strictly before acceptedBeforeRecordTime.
type RetiringPackageID struct {
	packageID                PackageID
	expectedVersion          CorePackageVersion
	acceptedBeforeRecordTime time.Time
}

func (retiring RetiringPackageID) PackageID() PackageID { return retiring.packageID }

func (retiring RetiringPackageID) ExpectedVersion() CorePackageVersion {
	return retiring.expectedVersion
}

// AcceptedBeforeRecordTime is the exclusive UTC deadline.
func (retiring RetiringPackageID) AcceptedBeforeRecordTime() time.Time {
	return retiring.acceptedBeforeRecordTime
}

// CurrentPackageConfig is the operator input for the current id. Version is
// empty only in unsafe dev mode.
type CurrentPackageConfig struct {
	PackageIDHex string
	Version      string
}

// RetiringPackageConfig is the operator input for the retiring id. Every field
// is empty outside an upgrade window. Version is empty only in unsafe dev mode.
type RetiringPackageConfig struct {
	PackageIDHex     string
	AcceptedUntilUTC string
	Version          string
}

// PinnedCorePackage is one pinned id with the version its vetting metadata must show.
type PinnedCorePackage struct {
	Role            AcceptedPackageRole
	PackageID       PackageID
	ExpectedVersion CorePackageVersion
}

// AcceptedPackageIDs is the set of wormhole-core package ids whose
// PublishMessage exercises the watcher emits. The zero value accepts nothing.
//
// SECURITY: every accepted id must be a guardian-approved, vetted core version.
// Messages executed by any accepted id are signed under that version's checks.
type AcceptedPackageIDs struct {
	set acceptedPackageSet
}

// acceptedPackageSet is anyPackageSet or pinnedPackageSet; nil accepts nothing.
type acceptedPackageSet interface {
	verdictForPublish(executingPackageIDHex string, recordTime time.Time) PublishPackageVerdict
}

// anyPackageSet matches every package (unsafe dev mode only).
type anyPackageSet struct{}

func (anyPackageSet) verdictForPublish(string, time.Time) PublishPackageVerdict {
	return PublishPackageVerdictAcceptedCurrent
}

// pinnedPackageSet is a current id plus, during an upgrade window, one retiring id.
type pinnedPackageSet struct {
	current                PackageID
	currentExpectedVersion CorePackageVersion
	retiring               *RetiringPackageID
}

func (pinned pinnedPackageSet) verdictForPublish(executingPackageIDHex string, recordTime time.Time) PublishPackageVerdict {
	if executingPackageIDHex == "" {
		return PublishPackageVerdictRejectedUnpinned
	}
	if executingPackageIDHex == pinned.current.hexID {
		return PublishPackageVerdictAcceptedCurrent
	}
	if pinned.retiring == nil || executingPackageIDHex != pinned.retiring.packageID.hexID {
		return PublishPackageVerdictRejectedUnpinned
	}
	// SECURITY: record time is sequencer-assigned; effective time is submitter-chosen.
	if recordTime.IsZero() {
		return PublishPackageVerdictRejectedMissingRecordTime
	}
	if !recordTime.Before(pinned.retiring.acceptedBeforeRecordTime) {
		return PublishPackageVerdictRejectedRetiringAfterDeadline
	}
	return PublishPackageVerdictAcceptedRetiring
}

// PublishPackageVerdict is the watcher's decision for one PublishMessage exercise.
type PublishPackageVerdict int

const (
	PublishPackageVerdictAcceptedCurrent PublishPackageVerdict = iota + 1
	PublishPackageVerdictAcceptedRetiring
	PublishPackageVerdictRejectedUnpinned
	PublishPackageVerdictRejectedRetiringAfterDeadline
	PublishPackageVerdictRejectedMissingRecordTime
)

// AcceptedPackageRole is the role of the package id that executed an accepted publish.
type AcceptedPackageRole int

const (
	AcceptedPackageRoleCurrent AcceptedPackageRole = iota + 1
	AcceptedPackageRoleRetiring
)

// String is the metric label: "current", "retiring", or "unknown".
func (role AcceptedPackageRole) String() string {
	switch role {
	case AcceptedPackageRoleCurrent:
		return "current"
	case AcceptedPackageRoleRetiring:
		return "retiring"
	default:
		return "unknown"
	}
}

func (role AcceptedPackageRole) IsKnown() bool {
	return role == AcceptedPackageRoleCurrent || role == AcceptedPackageRoleRetiring
}

// AcceptedRole is the role of an accepted verdict; false for a rejection.
func (verdict PublishPackageVerdict) AcceptedRole() (AcceptedPackageRole, bool) {
	switch verdict {
	case PublishPackageVerdictAcceptedCurrent:
		return AcceptedPackageRoleCurrent, true
	case PublishPackageVerdictAcceptedRetiring:
		return AcceptedPackageRoleRetiring, true
	default:
		return 0, false
	}
}

// ParseAcceptedPackageIDs requires a current id. retiring is empty outside an
// upgrade window; otherwise it is a second id with an accepted-until deadline
// no more than MaxRetiringPackageWindow after startupWallClock. The bound
// applies to each startup, so a later startup can set a later deadline. A
// deadline in the past is valid: it keeps pre-deadline messages recoverable.
func ParseAcceptedPackageIDs(currentConfig CurrentPackageConfig, retiring RetiringPackageConfig, startupWallClock time.Time) (AcceptedPackageIDs, error) {
	current, err := ParsePackageID(currentConfig.PackageIDHex)
	if err != nil {
		return AcceptedPackageIDs{}, fmt.Errorf("current core package id: %w", err)
	}
	currentExpectedVersion, err := parseOptionalCorePackageVersion(currentConfig.Version)
	if err != nil {
		return AcceptedPackageIDs{}, fmt.Errorf("current core package version: %w", err)
	}
	pinned := pinnedPackageSet{current: current, currentExpectedVersion: currentExpectedVersion}
	if retiring == (RetiringPackageConfig{}) {
		return AcceptedPackageIDs{set: pinned}, nil
	}
	if retiring.PackageIDHex == "" {
		return AcceptedPackageIDs{}, fmt.Errorf("an accepted-until deadline or a version requires a retiring core package id")
	}
	if retiring.AcceptedUntilUTC == "" {
		return AcceptedPackageIDs{}, fmt.Errorf("a retiring core package id requires an accepted-until deadline")
	}
	retiringPackageID, err := ParsePackageID(retiring.PackageIDHex)
	if err != nil {
		return AcceptedPackageIDs{}, fmt.Errorf("retiring core package id: %w", err)
	}
	if retiringPackageID == current {
		return AcceptedPackageIDs{}, fmt.Errorf("retiring core package id must differ from the current core package id %s", current)
	}
	acceptedBeforeRecordTime, err := parseRetiringPackageDeadline(retiring.AcceptedUntilUTC)
	if err != nil {
		return AcceptedPackageIDs{}, err
	}
	retiringExpectedVersion, err := parseOptionalCorePackageVersion(retiring.Version)
	if err != nil {
		return AcceptedPackageIDs{}, fmt.Errorf("retiring core package version: %w", err)
	}
	if acceptedBeforeRecordTime.Sub(startupWallClock) > MaxRetiringPackageWindow {
		return AcceptedPackageIDs{}, fmt.Errorf("accepted-until deadline %s is more than %s after startup %s",
			retiring.AcceptedUntilUTC, MaxRetiringPackageWindow, startupWallClock.UTC().Format(RetiringPackageDeadlineLayout))
	}
	pinned.retiring = &RetiringPackageID{packageID: retiringPackageID, expectedVersion: retiringExpectedVersion, acceptedBeforeRecordTime: acceptedBeforeRecordTime}
	return AcceptedPackageIDs{set: pinned}, nil
}

// parseOptionalCorePackageVersion maps "" to the zero version.
func parseOptionalCorePackageVersion(versionText string) (CorePackageVersion, error) {
	if versionText == "" {
		return CorePackageVersion{}, nil
	}
	return ParseCorePackageVersion(versionText)
}

// parseRetiringPackageDeadline accepts exactly RetiringPackageDeadlineLayout.
func parseRetiringPackageDeadline(deadlineText string) (time.Time, error) {
	deadline, err := time.Parse(RetiringPackageDeadlineLayout, deadlineText)
	if err != nil || deadline.Format(RetiringPackageDeadlineLayout) != deadlineText {
		return time.Time{}, fmt.Errorf("accepted-until deadline %q must be RFC 3339 UTC with whole seconds (%s)", deadlineText, RetiringPackageDeadlineLayout)
	}
	if deadline.Location() != time.UTC {
		return time.Time{}, fmt.Errorf("accepted-until deadline %q must be UTC", deadlineText)
	}
	return deadline, nil
}

// AcceptAnyPackageID matches every package that declares the watched template.
// Unsafe dev mode only: a look-alike package can forge messages under it.
func AcceptAnyPackageID() AcceptedPackageIDs {
	return AcceptedPackageIDs{set: anyPackageSet{}}
}

// VerdictForPublish decides one exercise from ledger data only:
// executingPackageIDHex is the event's raw package id, recordTime its transaction's record time.
func (accepted AcceptedPackageIDs) VerdictForPublish(executingPackageIDHex string, recordTime time.Time) PublishPackageVerdict {
	if accepted.set == nil {
		return PublishPackageVerdictRejectedUnpinned
	}
	return accepted.set.verdictForPublish(executingPackageIDHex, recordTime)
}

func (accepted AcceptedPackageIDs) MatchesAnyPackage() bool {
	_, matchesAny := accepted.set.(anyPackageSet)
	return matchesAny
}

// IsEmpty reports the zero value, which accepts nothing.
func (accepted AcceptedPackageIDs) IsEmpty() bool { return accepted.set == nil }

// CurrentPackageID is false for the wildcard and the zero value.
func (accepted AcceptedPackageIDs) CurrentPackageID() (PackageID, bool) {
	pinned, isPinned := accepted.set.(pinnedPackageSet)
	if !isPinned {
		return PackageID{}, false
	}
	return pinned.current, true
}

// PinnedCorePackages lists the current id, then the retiring id; empty for the wildcard and the zero value.
func (accepted AcceptedPackageIDs) PinnedCorePackages() []PinnedCorePackage {
	pinned, isPinned := accepted.set.(pinnedPackageSet)
	if !isPinned {
		return nil
	}
	pinnedPackages := []PinnedCorePackage{{Role: AcceptedPackageRoleCurrent, PackageID: pinned.current, ExpectedVersion: pinned.currentExpectedVersion}}
	if pinned.retiring != nil {
		pinnedPackages = append(pinnedPackages, PinnedCorePackage{Role: AcceptedPackageRoleRetiring, PackageID: pinned.retiring.packageID, ExpectedVersion: pinned.retiring.expectedVersion})
	}
	return pinnedPackages
}

// RetiringPackageID is false outside an upgrade window.
func (accepted AcceptedPackageIDs) RetiringPackageID() (RetiringPackageID, bool) {
	pinned, isPinned := accepted.set.(pinnedPackageSet)
	if !isPinned || pinned.retiring == nil {
		return RetiringPackageID{}, false
	}
	return *pinned.retiring, true
}

// String is "*" for the wildcard, "current" or "current,retiring" otherwise.
func (accepted AcceptedPackageIDs) String() string {
	switch set := accepted.set.(type) {
	case anyPackageSet:
		return "*"
	case pinnedPackageSet:
		if set.retiring == nil {
			return set.current.hexID
		}
		return set.current.hexID + "," + set.retiring.packageID.hexID
	default:
		return ""
	}
}
