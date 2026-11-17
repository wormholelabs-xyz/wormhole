package cantonclient

import (
	"fmt"
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
	acceptsPackageID(executingPackageIDHex string) bool
}

// anyPackageSet matches every package (unsafe dev mode only).
type anyPackageSet struct{}

func (anyPackageSet) acceptsPackageID(string) bool { return true }

// pinnedPackageSet is a primary id plus, during an upgrade window, one next id.
type pinnedPackageSet struct {
	primary PackageID
	next    *PackageID
}

func (pinned pinnedPackageSet) acceptsPackageID(executingPackageIDHex string) bool {
	if executingPackageIDHex == "" {
		return false
	}
	if executingPackageIDHex == pinned.primary.hexID {
		return true
	}
	return pinned.next != nil && executingPackageIDHex == pinned.next.hexID
}

// ParseAcceptedPackageIDs requires a primary id. nextHex is empty outside an
// upgrade window; otherwise it is a second id that differs from the primary.
func ParseAcceptedPackageIDs(primaryHex string, nextHex string) (AcceptedPackageIDs, error) {
	primary, err := ParsePackageID(primaryHex)
	if err != nil {
		return AcceptedPackageIDs{}, fmt.Errorf("primary core package id: %w", err)
	}
	pinned := pinnedPackageSet{primary: primary}
	if nextHex == "" {
		return AcceptedPackageIDs{set: pinned}, nil
	}
	next, err := ParsePackageID(nextHex)
	if err != nil {
		return AcceptedPackageIDs{}, fmt.Errorf("next core package id: %w", err)
	}
	if next == primary {
		return AcceptedPackageIDs{}, fmt.Errorf("next core package id must differ from the primary core package id %s", primary)
	}
	pinned.next = &next
	return AcceptedPackageIDs{set: pinned}, nil
}

// AcceptAnyPackageID matches every package that declares the watched template.
// Unsafe dev mode only: a look-alike package can forge messages under it.
func AcceptAnyPackageID() AcceptedPackageIDs {
	return AcceptedPackageIDs{set: anyPackageSet{}}
}

// Accepts reports whether executingPackageIDHex is in the set.
func (accepted AcceptedPackageIDs) Accepts(executingPackageIDHex string) bool {
	if accepted.set == nil {
		return false
	}
	return accepted.set.acceptsPackageID(executingPackageIDHex)
}

func (accepted AcceptedPackageIDs) MatchesAnyPackage() bool {
	_, matchesAny := accepted.set.(anyPackageSet)
	return matchesAny
}

// IsEmpty reports the zero value, which accepts nothing.
func (accepted AcceptedPackageIDs) IsEmpty() bool { return accepted.set == nil }

// PrimaryPackageID is the primary id; false for the wildcard and the zero value.
func (accepted AcceptedPackageIDs) PrimaryPackageID() (PackageID, bool) {
	pinned, isPinned := accepted.set.(pinnedPackageSet)
	if !isPinned {
		return PackageID{}, false
	}
	return pinned.primary, true
}

// NextPackageID is the upgrade-window id; false outside a window.
func (accepted AcceptedPackageIDs) NextPackageID() (PackageID, bool) {
	pinned, isPinned := accepted.set.(pinnedPackageSet)
	if !isPinned || pinned.next == nil {
		return PackageID{}, false
	}
	return *pinned.next, true
}

// String is "*" for the wildcard, "primary" or "primary,next" otherwise.
func (accepted AcceptedPackageIDs) String() string {
	switch set := accepted.set.(type) {
	case anyPackageSet:
		return "*"
	case pinnedPackageSet:
		if set.next == nil {
			return set.primary.hexID
		}
		return set.primary.hexID + "," + set.next.hexID
	default:
		return ""
	}
}
