package canton

import (
	"fmt"
	"os"
	"path/filepath"
	"regexp"
	"strings"
	"testing"

	"github.com/stretchr/testify/require"
)

var damlSDKVersionPattern = regexp.MustCompile(`^[0-9]+\.[0-9]+\.[0-9]+$`)

// damlYAMLPathsUnderCanton lists every Daml package of canton/. Each must pin the same SDK.
var damlYAMLPathsUnderCanton = []string{
	"core/daml.yaml",
	"test/daml.yaml",
	"test-lookalike/daml.yaml",
	"localnet/daml/daml.yaml",
	"localnet/vetting_probe/v1/daml.yaml",
	"localnet/vetting_probe/v2/daml.yaml",
}

// parsePinnedDamlSDKVersion returns the `sdk-version` of a daml.yaml.
func parsePinnedDamlSDKVersion(damlYAML string) (string, error) {
	var versions []string
	for _, line := range strings.Split(damlYAML, "\n") {
		value, found := strings.CutPrefix(line, "sdk-version:")
		if found {
			versions = append(versions, strings.TrimSpace(value))
		}
	}
	if len(versions) != 1 {
		return "", fmt.Errorf("daml.yaml has %d sdk-version lines, expected exactly 1", len(versions))
	}
	if !damlSDKVersionPattern.MatchString(versions[0]) {
		return "", fmt.Errorf("sdk-version %q is not MAJOR.MINOR.PATCH", versions[0])
	}
	return versions[0], nil
}

// parseDockerfileDamlSDKVersion returns the default of `ARG DAML_SDK_VERSION=` in canton/Dockerfile.
func parseDockerfileDamlSDKVersion(dockerfile string) (string, error) {
	var versions []string
	for _, line := range strings.Split(dockerfile, "\n") {
		value, found := strings.CutPrefix(line, "ARG DAML_SDK_VERSION=")
		if found {
			versions = append(versions, strings.TrimSpace(value))
		}
	}
	if len(versions) != 1 {
		return "", fmt.Errorf("Dockerfile has %d ARG DAML_SDK_VERSION= lines, expected exactly 1", len(versions))
	}
	if !damlSDKVersionPattern.MatchString(versions[0]) {
		return "", fmt.Errorf("DAML_SDK_VERSION %q is not MAJOR.MINOR.PATCH", versions[0])
	}
	return versions[0], nil
}

// pinnedDamlSDKVersion is the one SDK version that every daml.yaml under cantonDir pins.
func pinnedDamlSDKVersion(t *testing.T, cantonDir string) string {
	t.Helper()
	require.NotEmpty(t, damlYAMLPathsUnderCanton)
	pinnedVersions := make(map[string]string, len(damlYAMLPathsUnderCanton))
	for _, damlYAMLPath := range damlYAMLPathsUnderCanton {
		damlYAML, err := os.ReadFile(filepath.Join(cantonDir, damlYAMLPath))
		require.NoError(t, err)
		version, err := parsePinnedDamlSDKVersion(string(damlYAML))
		require.NoErrorf(t, err, "canton/%s", damlYAMLPath)
		pinnedVersions[damlYAMLPath] = version
	}
	coreVersion := pinnedVersions[damlYAMLPathsUnderCanton[0]]
	for damlYAMLPath, version := range pinnedVersions {
		require.Equalf(t, coreVersion, version, "canton/%s pins SDK %s, canton/core/daml.yaml pins %s", damlYAMLPath, version, coreVersion)
	}
	return coreVersion
}

func TestEveryCantonDamlYAMLAndTheDockerfilePinOneSDKVersion(t *testing.T) {
	cantonDir, err := filepath.Abs("../../../../canton")
	require.NoError(t, err)
	pinnedVersion := pinnedDamlSDKVersion(t, cantonDir)
	dockerfile, err := os.ReadFile(filepath.Join(cantonDir, "Dockerfile"))
	require.NoError(t, err)
	dockerfileVersion, err := parseDockerfileDamlSDKVersion(string(dockerfile))
	require.NoError(t, err)
	require.Equal(t, pinnedVersion, dockerfileVersion, "canton/Dockerfile DAML_SDK_VERSION differs from the daml.yaml pins")
}

func TestParsePinnedDamlSDKVersion(t *testing.T) {
	cases := []struct {
		name        string
		damlYAML    string
		wantVersion string
		wantErr     string
	}{
		{name: "one pin", damlYAML: "sdk-version: 3.7.3\nname: wormhole-core\n", wantVersion: "3.7.3"},
		{name: "pin with trailing spaces", damlYAML: "sdk-version: 3.7.3  \n", wantVersion: "3.7.3"},
		{name: "no pin", damlYAML: "name: wormhole-core\n", wantErr: "0 sdk-version lines"},
		{name: "two pins", damlYAML: "sdk-version: 3.7.3\nsdk-version: 3.7.4\n", wantErr: "2 sdk-version lines"},
		{name: "commented pin is ignored", damlYAML: "# sdk-version: 3.7.4\n", wantErr: "0 sdk-version lines"},
		{name: "empty pin", damlYAML: "sdk-version:\n", wantErr: "not MAJOR.MINOR.PATCH"},
		{name: "two-part pin", damlYAML: "sdk-version: 3.7\n", wantErr: "not MAJOR.MINOR.PATCH"},
		{name: "snapshot pin", damlYAML: "sdk-version: 3.7.0-snapshot.20261009\n", wantErr: "not MAJOR.MINOR.PATCH"},
	}
	for _, tc := range cases {
		t.Run(tc.name, func(t *testing.T) {
			version, err := parsePinnedDamlSDKVersion(tc.damlYAML)
			if tc.wantErr != "" {
				require.ErrorContains(t, err, tc.wantErr)
				require.Empty(t, version)
				return
			}
			require.NoError(t, err)
			require.Equal(t, tc.wantVersion, version)
		})
	}
}

func TestParseDockerfileDamlSDKVersion(t *testing.T) {
	cases := []struct {
		name        string
		dockerfile  string
		wantVersion string
		wantErr     string
	}{
		{name: "one default", dockerfile: "ARG DAML_SDK_VERSION=3.7.3\nFROM x\nARG DAML_SDK_VERSION\n", wantVersion: "3.7.3"},
		{name: "no default", dockerfile: "FROM x\nARG DAML_SDK_VERSION\n", wantErr: "0 ARG DAML_SDK_VERSION= lines"},
		{name: "two defaults", dockerfile: "ARG DAML_SDK_VERSION=3.7.3\nARG DAML_SDK_VERSION=3.7.4\n", wantErr: "2 ARG DAML_SDK_VERSION= lines"},
		{name: "empty default", dockerfile: "ARG DAML_SDK_VERSION=\n", wantErr: "not MAJOR.MINOR.PATCH"},
		{name: "two-part default", dockerfile: "ARG DAML_SDK_VERSION=3.7\n", wantErr: "not MAJOR.MINOR.PATCH"},
	}
	for _, tc := range cases {
		t.Run(tc.name, func(t *testing.T) {
			version, err := parseDockerfileDamlSDKVersion(tc.dockerfile)
			if tc.wantErr != "" {
				require.ErrorContains(t, err, tc.wantErr)
				require.Empty(t, version)
				return
			}
			require.NoError(t, err)
			require.Equal(t, tc.wantVersion, version)
		})
	}
}
