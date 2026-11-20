//go:build integration

package canton

import (
	"context"
	"fmt"
	"os"
	"path/filepath"
	"regexp"
	"strings"
	"testing"
	"time"

	"github.com/stretchr/testify/require"
	"google.golang.org/grpc"
	"google.golang.org/grpc/credentials/insecure"
	"google.golang.org/protobuf/types/known/emptypb"
	"google.golang.org/protobuf/types/known/wrapperspb"
)

var damlSDKVersionPattern = regexp.MustCompile(`^[0-9]+\.[0-9]+\.[0-9]+$`)

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

// pinnedDamlSDKVersion is the sdk-version of canton/core/daml.yaml; canton/test/daml.yaml must pin the same.
func pinnedDamlSDKVersion(t *testing.T, cantonDir string) string {
	t.Helper()
	pinnedVersions := make([]string, 0, 2)
	for _, packageDir := range []string{"core", "test"} {
		damlYAML, err := os.ReadFile(filepath.Join(cantonDir, packageDir, "daml.yaml"))
		require.NoError(t, err)
		version, err := parsePinnedDamlSDKVersion(string(damlYAML))
		require.NoErrorf(t, err, "canton/%s/daml.yaml", packageDir)
		pinnedVersions = append(pinnedVersions, version)
	}
	require.Len(t, pinnedVersions, 2)
	require.Equal(t, pinnedVersions[0], pinnedVersions[1], "canton/core and canton/test pin different SDK versions")
	return pinnedVersions[0]
}

// sandboxLedgerAPIVersion calls VersionService.GetLedgerApiVersion.
// GetLedgerApiVersionRequest is empty and the response carries `string version = 1`,
// so Empty and StringValue have the same wire format.
func sandboxLedgerAPIVersion(t *testing.T, port string) string {
	t.Helper()
	conn, err := grpc.NewClient("localhost:"+port, grpc.WithTransportCredentials(insecure.NewCredentials()))
	require.NoError(t, err)
	defer conn.Close()
	ctx, cancel := context.WithTimeout(context.Background(), 30*time.Second)
	defer cancel()
	response := &wrapperspb.StringValue{}
	require.NoError(t, conn.Invoke(ctx, "/com.daml.ledger.api.v2.VersionService/GetLedgerApiVersion", &emptypb.Empty{}, response))
	return response.GetValue()
}

// checkSandboxRunsPinnedSDKVersion fails the test when the sandbox runs another SDK than daml.yaml pins.
func checkSandboxRunsPinnedSDKVersion(t *testing.T, cantonDir, port string) {
	t.Helper()
	pinnedVersion := pinnedDamlSDKVersion(t, cantonDir)
	runningVersion := sandboxLedgerAPIVersion(t, port)
	require.NotEmpty(t, runningVersion)
	require.Equalf(t, pinnedVersion, runningVersion, "sandbox Ledger API version %s, daml.yaml pins SDK %s", runningVersion, pinnedVersion)
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
