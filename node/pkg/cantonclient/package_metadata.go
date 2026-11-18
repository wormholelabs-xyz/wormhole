package cantonclient

import (
	"fmt"
	"regexp"
)

// CorePackageName is the Daml package name of the core bridge (canton/core/daml.yaml `name`).
const CorePackageName = "wormhole-core"

// corePackageVersionPattern is MAJOR.MINOR.PATCH, each part at most 9 digits without leading zeros.
var corePackageVersionPattern = regexp.MustCompile(`^(0|[1-9][0-9]{0,8})\.(0|[1-9][0-9]{0,8})\.(0|[1-9][0-9]{0,8})$`)

// CorePackageVersion is a wormhole-core package version. The zero value means
// "not configured", which only unsafe dev mode admits.
type CorePackageVersion struct {
	versionText string
}

func ParseCorePackageVersion(versionText string) (CorePackageVersion, error) {
	if !corePackageVersionPattern.MatchString(versionText) {
		return CorePackageVersion{}, fmt.Errorf("core package version %q must be MAJOR.MINOR.PATCH", versionText)
	}
	return CorePackageVersion{versionText: versionText}, nil
}

func (version CorePackageVersion) String() string { return version.versionText }

func (version CorePackageVersion) IsZero() bool { return version.versionText == "" }

// VettedPackageMetadata is one VettedPackage entry of ListVettedPackages.
// PackageName and PackageVersion are empty when the package was not uploaded
// to the queried participant.
type VettedPackageMetadata struct {
	PackageID      string
	PackageName    string
	PackageVersion string
}

// PackageMetadataVerdict is the result of comparing vetting metadata with the expected name and version.
type PackageMetadataVerdict int

const (
	PackageMetadataVerdictVerified PackageMetadataVerdict = iota + 1
	PackageMetadataVerdictMismatched
	PackageMetadataVerdictNotYetVetted
)

// CheckVettedPackageMatchesExpected is Verified iff at least one entry for
// packageID carries metadata and every such entry names CorePackageName at
// exactly expectedVersion. Any other metadata for packageID is Mismatched.
//
// SECURITY: this catches operator copy errors. The author of a look-alike
// package chooses its name and version, so it is not a security boundary.
func CheckVettedPackageMatchesExpected(entries []VettedPackageMetadata, packageID PackageID, expectedVersion CorePackageVersion) PackageMetadataVerdict {
	if packageID.isZero() || expectedVersion.IsZero() {
		panic("cantonclient: metadata check needs a package id and an expected version")
	}
	entriesWithMetadata := 0
	for _, entry := range entries {
		if entry.PackageID != packageID.hexID {
			continue
		}
		if entry.PackageName == "" && entry.PackageVersion == "" {
			continue
		}
		if entry.PackageName != CorePackageName || entry.PackageVersion != expectedVersion.versionText {
			return PackageMetadataVerdictMismatched
		}
		entriesWithMetadata++
	}
	if entriesWithMetadata == 0 {
		return PackageMetadataVerdictNotYetVetted
	}
	return PackageMetadataVerdictVerified
}
