package guardiand

import (
	"fmt"
	"slices"
)

// cantonFlagNamesThatNeedRPC lists the canton flags that have no effect without --cantonRPC.
func cantonFlagNamesThatNeedRPC() []string {
	return []string{
		"cantonPackageID",
		"cantonPackageVersion",
		"cantonRetiringPackageID",
		"cantonRetiringPackageAcceptedUntil",
		"cantonRetiringPackageVersion",
		"cantonReadAsParty",
		"cantonAuthTokenURL",
		"cantonAuthClientID",
		"cantonAuthClientSecret",
		"cantonAuthAudience",
	}
}

// cantonFlagValuesThatNeedRPC maps each name of cantonFlagNamesThatNeedRPC to its value.
func cantonFlagValuesThatNeedRPC() map[string]string {
	return map[string]string{
		"cantonPackageID":                    *cantonPackageID,
		"cantonPackageVersion":               *cantonPackageVersion,
		"cantonRetiringPackageID":            *cantonRetiringPackageID,
		"cantonRetiringPackageAcceptedUntil": *cantonRetiringPackageAcceptedUntil,
		"cantonRetiringPackageVersion":       *cantonRetiringPackageVersion,
		"cantonReadAsParty":                  *cantonReadAsParty,
		"cantonAuthTokenURL":                 *cantonAuthTokenURL,
		"cantonAuthClientID":                 *cantonAuthClientID,
		"cantonAuthClientSecret":             *cantonAuthClientSecret,
		"cantonAuthAudience":                 *cantonAuthAudience,
	}
}

// checkCantonFlagsNeedRPC rejects any set dependent flag when cantonRPC is empty or "none" (see shouldStart).
// The error names the first such flag in sorted order.
func checkCantonFlagsNeedRPC(cantonRPC string, dependentFlags map[string]string) error {
	if len(dependentFlags) != len(cantonFlagNamesThatNeedRPC()) {
		panic("guardiand: canton dependent flag map does not match cantonFlagNamesThatNeedRPC")
	}
	if shouldStart(&cantonRPC) {
		return nil
	}
	flagNames := make([]string, 0, len(dependentFlags))
	for flagName := range dependentFlags {
		flagNames = append(flagNames, flagName)
	}
	slices.Sort(flagNames)
	for _, flagName := range flagNames {
		if dependentFlags[flagName] != "" {
			return fmt.Errorf("if --cantonRPC is not set, --%s may not be set", flagName)
		}
	}
	return nil
}
