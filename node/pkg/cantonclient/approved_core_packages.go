package cantonclient

import (
	"bytes"
	_ "embed"
	"encoding/json"
	"fmt"
)

// approvedCorePackagesJSON lists every wormhole-core package id a guardian may
// pin outside unsafe dev mode. Change it only through a reviewed guardiand release.
//
//go:embed approved_core_packages.json
var approvedCorePackagesJSON []byte

// maxApprovedCorePackages bounds the approved list.
const maxApprovedCorePackages = 64

// ApprovedCorePackages maps each approved package id to its exact version.
type ApprovedCorePackages struct {
	entries map[PackageID]CorePackageVersion
}

type approvedCorePackageEntry struct {
	PackageID string `json:"packageId"`
	Version   string `json:"version"`
}

// DefaultApprovedCorePackages is the list compiled into guardiand. It panics
// on a malformed file; TestDefaultApprovedCorePackagesFile covers it.
func DefaultApprovedCorePackages() ApprovedCorePackages {
	approved, err := ParseApprovedCorePackages(approvedCorePackagesJSON)
	if err != nil {
		panic(fmt.Sprintf("cantonclient: approved_core_packages.json: %v", err))
	}
	return approved
}

// ParseApprovedCorePackages requires a non-empty JSON list with unique ids and unique versions.
func ParseApprovedCorePackages(fileJSON []byte) (ApprovedCorePackages, error) {
	decoder := json.NewDecoder(bytes.NewReader(fileJSON))
	decoder.DisallowUnknownFields()
	var fileEntries []approvedCorePackageEntry
	if err := decoder.Decode(&fileEntries); err != nil {
		return ApprovedCorePackages{}, fmt.Errorf("approved core packages: %w", err)
	}
	if decoder.More() {
		return ApprovedCorePackages{}, fmt.Errorf("approved core packages: trailing data after the list")
	}
	if len(fileEntries) == 0 {
		return ApprovedCorePackages{}, fmt.Errorf("approved core packages: no approved package")
	}
	if len(fileEntries) > maxApprovedCorePackages {
		return ApprovedCorePackages{}, fmt.Errorf("approved core packages: more than %d entries", maxApprovedCorePackages)
	}
	entries := make(map[PackageID]CorePackageVersion, len(fileEntries))
	versionsSeen := make(map[CorePackageVersion]bool, len(fileEntries))
	for _, fileEntry := range fileEntries {
		packageID, err := ParsePackageID(fileEntry.PackageID)
		if err != nil {
			return ApprovedCorePackages{}, fmt.Errorf("approved core packages: %w", err)
		}
		version, err := ParseCorePackageVersion(fileEntry.Version)
		if err != nil {
			return ApprovedCorePackages{}, fmt.Errorf("approved core packages: %w", err)
		}
		if _, duplicate := entries[packageID]; duplicate {
			return ApprovedCorePackages{}, fmt.Errorf("approved core packages: duplicate package id %s", packageID)
		}
		if versionsSeen[version] {
			return ApprovedCorePackages{}, fmt.Errorf("approved core packages: duplicate version %s", version)
		}
		entries[packageID] = version
		versionsSeen[version] = true
	}
	return ApprovedCorePackages{entries: entries}, nil
}

// CheckCorePackageIDApproved requires packageID in the list at any version.
func (approved ApprovedCorePackages) CheckCorePackageIDApproved(packageID PackageID) error {
	if _, found := approved.entries[packageID]; !found {
		return fmt.Errorf("core package id %s is not in the approved list compiled into guardiand", packageID)
	}
	return nil
}

// CheckCorePackageApproved requires packageID in the list at exactly version.
func (approved ApprovedCorePackages) CheckCorePackageApproved(packageID PackageID, version CorePackageVersion) error {
	approvedVersion, found := approved.entries[packageID]
	if !found {
		return fmt.Errorf("core package id %s is not in the approved list compiled into guardiand", packageID)
	}
	if approvedVersion != version {
		return fmt.Errorf("core package id %s is approved as %s, not %s", packageID, approvedVersion, version)
	}
	return nil
}

// CheckAcceptedPackageIDsApproved requires a pinned set whose every id has an expected version in the list.
func (approved ApprovedCorePackages) CheckAcceptedPackageIDsApproved(accepted AcceptedPackageIDs) error {
	if accepted.IsEmpty() {
		return fmt.Errorf("no accepted core package ids")
	}
	if accepted.MatchesAnyPackage() {
		return fmt.Errorf("the any-package wildcard has no approved package id")
	}
	for _, pinned := range accepted.PinnedCorePackages() {
		if pinned.ExpectedVersion.IsZero() {
			return fmt.Errorf("the %s core package id needs an expected version", pinned.Role)
		}
		if err := approved.CheckCorePackageApproved(pinned.PackageID, pinned.ExpectedVersion); err != nil {
			return fmt.Errorf("%s core package: %w", pinned.Role, err)
		}
	}
	return nil
}
