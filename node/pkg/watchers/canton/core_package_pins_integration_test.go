//go:build integration

package canton

import (
	"encoding/json"
	"os"
	"os/exec"
	"path/filepath"
	"testing"

	"github.com/stretchr/testify/require"
)

// TestBuiltCoreDARMatchesThePins builds canton/ with the daml.yaml SDK and compares the core main package id with the pins.
func TestBuiltCoreDARMatchesThePins(t *testing.T) {
	dpm := findDpm(t)
	cantonDir, err := filepath.Abs("../../../../canton")
	require.NoError(t, err)
	runCmd(t, cantonDir, dpm, "build", "--all")

	coreDamlYAML, err := os.ReadFile(filepath.Join(cantonDir, "core", "daml.yaml"))
	require.NoError(t, err)
	coreVersion, err := parseDamlYAMLPackageVersion(string(coreDamlYAML))
	require.NoError(t, err)
	coreDAR := filepath.Join(cantonDir, "core", ".daml", "dist", "wormhole-core-"+coreVersion+".dar")
	inspectCommand := exec.Command(dpm, "damlc", "inspect-dar", "--json", coreDAR)
	inspectCommand.Env = append(inspectCommand.Environ(), "DPM_SDK_VERSION="+pinnedDamlSDKVersion(t, cantonDir))
	inspectJSON, err := inspectCommand.Output()
	require.NoError(t, err)
	var inspected struct {
		MainPackageID string `json:"main_package_id"`
	}
	require.NoError(t, json.Unmarshal(inspectJSON, &inspected))
	require.Len(t, inspected.MainPackageID, 64)
	t.Logf("built wormhole-core %s main package id %s", coreVersion, inspected.MainPackageID)

	require.NoError(t, checkRepositoryCorePackagePins(t, cantonDir, inspected.MainPackageID))
}
