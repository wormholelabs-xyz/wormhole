package guardiand

import (
	"testing"

	"github.com/stretchr/testify/assert"
	"github.com/stretchr/testify/require"
)

func TestCheckCantonFlagsNeedRPC(t *testing.T) {
	dependentFlagNames := []string{
		"cantonPackageID",
		"cantonPackageVersion",
		"cantonRetiringPackageID",
		"cantonRetiringPackageAcceptedUntil",
		"cantonRetiringPackageVersion",
		"cantonReadAsParty",
		"cantonAuthTokenURL",
		"cantonAuthClientID",
		"cantonAuthClientSecret",
	}
	allEmpty := func() map[string]string {
		dependentFlags := make(map[string]string, len(dependentFlagNames))
		for _, flagName := range dependentFlagNames {
			dependentFlags[flagName] = ""
		}
		return dependentFlags
	}

	t.Run("all empty", func(t *testing.T) {
		require.NoError(t, checkCantonFlagsNeedRPC("", allEmpty()))
	})
	t.Run("rpc set with every dependent flag", func(t *testing.T) {
		dependentFlags := allEmpty()
		for _, flagName := range dependentFlagNames {
			dependentFlags[flagName] = "value"
		}
		require.NoError(t, checkCantonFlagsNeedRPC("canton:5011", dependentFlags))
	})
	for _, flagName := range dependentFlagNames {
		t.Run(flagName+" without rpc", func(t *testing.T) {
			dependentFlags := allEmpty()
			dependentFlags[flagName] = "value"
			err := checkCantonFlagsNeedRPC("", dependentFlags)
			require.Error(t, err)
			assert.Contains(t, err.Error(), "--"+flagName)
			assert.Contains(t, err.Error(), "--cantonRPC")
		})
	}
	t.Run("every canton flag is checked", func(t *testing.T) {
		assert.ElementsMatch(t, dependentFlagNames, cantonFlagNamesThatNeedRPC())
	})
}
