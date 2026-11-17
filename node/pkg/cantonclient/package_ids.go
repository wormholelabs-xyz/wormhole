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
// PublishMessage exercises the watcher emits: a primary id plus, during an
// upgrade window, at most one next id. The zero value accepts nothing.
//
// SECURITY: every accepted id must be a guardian-approved, vetted core version.
// Messages executed by any accepted id are signed under that version's checks.
type AcceptedPackageIDs struct {
	primary           PackageID
	next              PackageID
	matchesAnyPackage bool
}

// ParseAcceptedPackageIDs requires a primary id. nextHex is empty outside an
// upgrade window; otherwise it is a second id that differs from the primary.
func ParseAcceptedPackageIDs(primaryHex string, nextHex string) (AcceptedPackageIDs, error) {
	primary, err := ParsePackageID(primaryHex)
	if err != nil {
		return AcceptedPackageIDs{}, fmt.Errorf("primary core package id: %w", err)
	}
	accepted := AcceptedPackageIDs{primary: primary}
	if nextHex == "" {
		return accepted, nil
	}
	next, err := ParsePackageID(nextHex)
	if err != nil {
		return AcceptedPackageIDs{}, fmt.Errorf("next core package id: %w", err)
	}
	if next == primary {
		return AcceptedPackageIDs{}, fmt.Errorf("next core package id must differ from the primary core package id %s", primary)
	}
	accepted.next = next
	return accepted, nil
}

// AcceptAnyPackageID matches every package that declares the watched template.
// Unsafe dev mode only: a look-alike package can forge messages under it.
func AcceptAnyPackageID() AcceptedPackageIDs {
	return AcceptedPackageIDs{matchesAnyPackage: true}
}

// Accepts reports whether executingPackageIDHex is in the set.
func (accepted AcceptedPackageIDs) Accepts(executingPackageIDHex string) bool {
	if accepted.matchesAnyPackage {
		return true
	}
	if executingPackageIDHex == "" {
		return false
	}
	if !accepted.primary.isZero() && executingPackageIDHex == accepted.primary.hexID {
		return true
	}
	return !accepted.next.isZero() && executingPackageIDHex == accepted.next.hexID
}

func (accepted AcceptedPackageIDs) MatchesAnyPackage() bool { return accepted.matchesAnyPackage }

// PrimaryPackageIDHex is the primary id, or empty for the dev-mode wildcard.
func (accepted AcceptedPackageIDs) PrimaryPackageIDHex() string { return accepted.primary.hexID }

// NextPackageIDHex is the upgrade-window id, or empty outside a window.
func (accepted AcceptedPackageIDs) NextPackageIDHex() string { return accepted.next.hexID }

// String is "*" for the wildcard, "primary" or "primary,next" otherwise.
func (accepted AcceptedPackageIDs) String() string {
	if accepted.matchesAnyPackage {
		return "*"
	}
	if accepted.next.isZero() {
		return accepted.primary.hexID
	}
	return accepted.primary.hexID + "," + accepted.next.hexID
}
