package canton

import (
	"fmt"
	"strings"
	"testing"

	"github.com/certusone/wormhole/node/pkg/cantonclient"
	"github.com/stretchr/testify/require"
)

// Helpers of the LocalNet harness (watcher_localnet_test.go). They carry no build tag, so CI tests them.

const localNetMaxReobservations = 64

// parseReobserveUpdateIDs parses a comma-separated list of Ledger API update ids.
func parseReobserveUpdateIDs(updateIDList string) ([]cantonclient.UpdateID, error) {
	var updateIDs []cantonclient.UpdateID
	seenUpdateIDs := make(map[cantonclient.UpdateID]struct{})
	for _, field := range strings.Split(updateIDList, ",") {
		updateIDText := strings.TrimSpace(field)
		if updateIDText == "" {
			continue
		}
		updateID, err := cantonclient.ParseUpdateID(updateIDText)
		if err != nil {
			return nil, err
		}
		if _, duplicate := seenUpdateIDs[updateID]; duplicate {
			return nil, fmt.Errorf("duplicate update id %s", updateIDText)
		}
		seenUpdateIDs[updateID] = struct{}{}
		updateIDs = append(updateIDs, updateID)
		if len(updateIDs) > localNetMaxReobservations {
			return nil, fmt.Errorf("at most %d update ids, got more", localNetMaxReobservations)
		}
	}
	return updateIDs, nil
}

func localNetTestUpdateIDText(index int) string {
	return fmt.Sprintf("1220%064x", index+1)
}

func TestParseReobserveUpdateIDs(t *testing.T) {
	firstUpdateID, secondUpdateID := localNetTestUpdateIDText(0), localNetTestUpdateIDText(1)
	sixtyFiveUpdateIDs := make([]string, localNetMaxReobservations+1)
	for index := range sixtyFiveUpdateIDs {
		sixtyFiveUpdateIDs[index] = localNetTestUpdateIDText(index)
	}
	testCases := []struct {
		name                  string
		updateIDList          string
		expectedUpdateIDTexts []string
		expectedErrorMessage  string
	}{
		{name: "empty list", updateIDList: "", expectedUpdateIDTexts: nil},
		{name: "one id", updateIDList: firstUpdateID, expectedUpdateIDTexts: []string{firstUpdateID}},
		{name: "two ids with spaces", updateIDList: " " + firstUpdateID + " , " + secondUpdateID + " ",
			expectedUpdateIDTexts: []string{firstUpdateID, secondUpdateID}},
		{name: "trailing comma", updateIDList: firstUpdateID + ",", expectedUpdateIDTexts: []string{firstUpdateID}},
		{name: "uppercase hex", updateIDList: "1220" + strings.Repeat("AB", 32), expectedErrorMessage: "not lowercase hex"},
		{name: "wrong header", updateIDList: "1320" + strings.Repeat("ab", 32), expectedErrorMessage: "multihash header"},
		{name: "33 bytes", updateIDList: "1220" + strings.Repeat("ab", 31), expectedErrorMessage: "got 66 characters, want 68"},
		{name: "offset text", updateIDList: "12345", expectedErrorMessage: "got 5 characters, want 68"},
		{name: "duplicate id", updateIDList: firstUpdateID + "," + firstUpdateID, expectedErrorMessage: "duplicate"},
		{name: "65 ids", updateIDList: strings.Join(sixtyFiveUpdateIDs, ","), expectedErrorMessage: "at most 64"},
	}
	for _, testCase := range testCases {
		t.Run(testCase.name, func(t *testing.T) {
			updateIDs, err := parseReobserveUpdateIDs(testCase.updateIDList)
			if testCase.expectedErrorMessage != "" {
				require.ErrorContains(t, err, testCase.expectedErrorMessage)
				require.Nil(t, updateIDs)
				return
			}
			require.NoError(t, err)
			require.Len(t, updateIDs, len(testCase.expectedUpdateIDTexts))
			for position, updateID := range updateIDs {
				require.Equal(t, testCase.expectedUpdateIDTexts[position], updateID.String())
			}
		})
	}
}
