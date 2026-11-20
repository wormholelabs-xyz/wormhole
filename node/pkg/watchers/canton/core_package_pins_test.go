package canton

import (
	"fmt"
	"os"
	"path/filepath"
	"regexp"
	"strings"
	"testing"

	"github.com/certusone/wormhole/node/pkg/cantonclient"
	"github.com/stretchr/testify/require"
)

var (
	nttLibCorePackageIDPattern = regexp.MustCompile(`(?m)^readonly WORMHOLE_CORE_PACKAGE_ID="([0-9a-f]{64})"$`)
	nttLibCoreVersionPattern   = regexp.MustCompile(`(?m)^readonly WORMHOLE_CORE_VERSION="([0-9]+\.[0-9]+\.[0-9]+)"$`)
)

// parseDamlYAMLPackageVersion returns the top-level `version` of a daml.yaml.
func parseDamlYAMLPackageVersion(damlYAML string) (string, error) {
	var versions []string
	for _, line := range strings.Split(damlYAML, "\n") {
		value, found := strings.CutPrefix(line, "version:")
		if found {
			versions = append(versions, strings.TrimSpace(value))
		}
	}
	if len(versions) != 1 {
		return "", fmt.Errorf("daml.yaml has %d version lines, expected exactly 1", len(versions))
	}
	return versions[0], nil
}

// onlyRegexpSubmatch returns the first group of the one match of pattern in text.
func onlyRegexpSubmatch(pattern *regexp.Regexp, text, description string) (string, error) {
	matches := pattern.FindAllStringSubmatch(text, -1)
	if len(matches) != 1 {
		return "", fmt.Errorf("%s: %d matches, expected exactly 1", description, len(matches))
	}
	return matches[0][1], nil
}

// checkCorePackagePinsMatchBuiltCore enforces that the core DAR built from core/daml.yaml is the one
// approved_core_packages.json approves for that version and the one canton/localnet/ntt_lib.sh pins.
func checkCorePackagePinsMatchBuiltCore(builtCorePackageIDHex, coreDamlYAML string, approvedCorePackagesJSON []byte, nttLib string) error {
	builtCorePackageID, err := cantonclient.ParsePackageID(builtCorePackageIDHex)
	if err != nil {
		return fmt.Errorf("built core package id: %w", err)
	}
	coreVersionText, err := parseDamlYAMLPackageVersion(coreDamlYAML)
	if err != nil {
		return fmt.Errorf("core/daml.yaml: %w", err)
	}
	coreVersion, err := cantonclient.ParseCorePackageVersion(coreVersionText)
	if err != nil {
		return fmt.Errorf("core/daml.yaml: %w", err)
	}
	approved, err := cantonclient.ParseApprovedCorePackages(approvedCorePackagesJSON)
	if err != nil {
		return fmt.Errorf("approved_core_packages.json: %w", err)
	}
	if err := approved.CheckCorePackageApproved(builtCorePackageID, coreVersion); err != nil {
		return fmt.Errorf("approved_core_packages.json does not approve the built core %s %s: %w", coreVersionText, builtCorePackageIDHex, err)
	}
	nttLibCorePackageIDHex, err := onlyRegexpSubmatch(nttLibCorePackageIDPattern, nttLib, "ntt_lib.sh WORMHOLE_CORE_PACKAGE_ID")
	if err != nil {
		return err
	}
	if nttLibCorePackageIDHex != builtCorePackageIDHex {
		return fmt.Errorf("ntt_lib.sh WORMHOLE_CORE_PACKAGE_ID %s, built core %s", nttLibCorePackageIDHex, builtCorePackageIDHex)
	}
	nttLibCoreVersion, err := onlyRegexpSubmatch(nttLibCoreVersionPattern, nttLib, "ntt_lib.sh WORMHOLE_CORE_VERSION")
	if err != nil {
		return err
	}
	if nttLibCoreVersion != coreVersionText {
		return fmt.Errorf("ntt_lib.sh WORMHOLE_CORE_VERSION %s, core/daml.yaml version %s", nttLibCoreVersion, coreVersionText)
	}
	return nil
}

func TestCheckCorePackagePinsMatchBuiltCore(t *testing.T) {
	const builtID = "a562ac78ce2617dda9dc1ccd8c7500b4e523ff7ae1d216e28f8836b1fb264034"
	const staleID = "bba94798a73c0fafadd836ec6754cab75205cb09556d13263813151bd748006a"
	coreDamlYAML := "sdk-version: 3.7.3\nname: wormhole-core\nversion: 0.9.0\n"
	approvedJSON := func(packageIDHex, version string) []byte {
		return []byte(`[{"packageId":"` + packageIDHex + `","version":"` + version + `"}]`)
	}
	nttLib := func(packageIDHex, version string) string {
		return "readonly WORMHOLE_CORE_VERSION=\"" + version + "\"\nreadonly WORMHOLE_CORE_PACKAGE_ID=\"" + packageIDHex + "\"\n"
	}
	cases := []struct {
		name         string
		builtID      string
		coreDamlYAML string
		approvedJSON []byte
		nttLib       string
		wantErr      string
	}{
		{name: "all pins match", builtID: builtID, coreDamlYAML: coreDamlYAML, approvedJSON: approvedJSON(builtID, "0.9.0"), nttLib: nttLib(builtID, "0.9.0")},
		{name: "stale approved id", builtID: builtID, coreDamlYAML: coreDamlYAML, approvedJSON: approvedJSON(staleID, "0.9.0"), nttLib: nttLib(builtID, "0.9.0"), wantErr: "does not approve the built core"},
		{name: "approved id for another version", builtID: builtID, coreDamlYAML: coreDamlYAML, approvedJSON: approvedJSON(builtID, "0.9.1"), nttLib: nttLib(builtID, "0.9.0"), wantErr: "does not approve the built core"},
		{name: "stale ntt_lib id", builtID: builtID, coreDamlYAML: coreDamlYAML, approvedJSON: approvedJSON(builtID, "0.9.0"), nttLib: nttLib(staleID, "0.9.0"), wantErr: "WORMHOLE_CORE_PACKAGE_ID " + staleID},
		{name: "stale ntt_lib version", builtID: builtID, coreDamlYAML: coreDamlYAML, approvedJSON: approvedJSON(builtID, "0.9.0"), nttLib: nttLib(builtID, "0.8.3"), wantErr: "WORMHOLE_CORE_VERSION 0.8.3"},
		{name: "ntt_lib without core id", builtID: builtID, coreDamlYAML: coreDamlYAML, approvedJSON: approvedJSON(builtID, "0.9.0"), nttLib: "readonly WORMHOLE_CORE_VERSION=\"0.9.0\"\n", wantErr: "0 matches"},
		{name: "ntt_lib with two core ids", builtID: builtID, coreDamlYAML: coreDamlYAML, approvedJSON: approvedJSON(builtID, "0.9.0"), nttLib: nttLib(builtID, "0.9.0") + nttLib(builtID, "0.9.0"), wantErr: "2 matches"},
		{name: "core/daml.yaml without version", builtID: builtID, coreDamlYAML: "name: wormhole-core\n", approvedJSON: approvedJSON(builtID, "0.9.0"), nttLib: nttLib(builtID, "0.9.0"), wantErr: "0 version lines"},
		{name: "malformed built id", builtID: "pkg", coreDamlYAML: coreDamlYAML, approvedJSON: approvedJSON(builtID, "0.9.0"), nttLib: nttLib(builtID, "0.9.0"), wantErr: "built core package id"},
	}
	for _, tc := range cases {
		t.Run(tc.name, func(t *testing.T) {
			err := checkCorePackagePinsMatchBuiltCore(tc.builtID, tc.coreDamlYAML, tc.approvedJSON, tc.nttLib)
			if tc.wantErr != "" {
				require.ErrorContains(t, err, tc.wantErr)
				return
			}
			require.NoError(t, err)
		})
	}
}

// checkRepositoryCorePackagePins reads the pins of the repository and checks them against builtCorePackageIDHex.
func checkRepositoryCorePackagePins(t *testing.T, cantonDir, builtCorePackageIDHex string) error {
	t.Helper()
	coreDamlYAML, err := os.ReadFile(filepath.Join(cantonDir, "core", "daml.yaml"))
	require.NoError(t, err)
	approvedJSON, err := os.ReadFile(filepath.Join(cantonDir, "..", "node", "pkg", "cantonclient", "approved_core_packages.json"))
	require.NoError(t, err)
	nttLib, err := os.ReadFile(filepath.Join(cantonDir, "localnet", "ntt_lib.sh"))
	require.NoError(t, err)
	return checkCorePackagePinsMatchBuiltCore(builtCorePackageIDHex, string(coreDamlYAML), approvedJSON, string(nttLib))
}
