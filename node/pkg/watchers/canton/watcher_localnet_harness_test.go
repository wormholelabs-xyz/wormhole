package canton

import (
	"errors"
	"fmt"
	"strings"
	"testing"

	"github.com/certusone/wormhole/node/pkg/cantonclient"
	"github.com/certusone/wormhole/node/pkg/common"
	"github.com/stretchr/testify/require"
)

// Helpers of the LocalNet harness (watcher_localnet_test.go). CI builds and tests this file.

const localNetMaxReobservations = 64

// localNetMaxReobservationRequestFileBytes allows one 68-character update id and a line end per request.
const localNetMaxReobservationRequestFileBytes = localNetMaxReobservations * 70

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

// reobservedUpdateTracker records which requested update ids produced a reobserved message.
type reobservedUpdateTracker struct {
	requested map[cantonclient.UpdateID]struct{}
	observed  map[cantonclient.UpdateID]struct{}
}

func newReobservedUpdateTracker(requested []cantonclient.UpdateID) reobservedUpdateTracker {
	tracker := reobservedUpdateTracker{
		requested: make(map[cantonclient.UpdateID]struct{}, len(requested)),
		observed:  make(map[cantonclient.UpdateID]struct{}, len(requested)),
	}
	for _, updateID := range requested {
		tracker.requested[updateID] = struct{}{}
	}
	return tracker
}

// recordReobservation returns an error for an update id that was not requested.
func (tracker *reobservedUpdateTracker) recordReobservation(updateID cantonclient.UpdateID) error {
	if _, requested := tracker.requested[updateID]; !requested {
		return fmt.Errorf("reobserved update %s was not requested", updateID)
	}
	tracker.observed[updateID] = struct{}{}
	return nil
}

func (tracker reobservedUpdateTracker) allRequestedUpdatesObserved() bool {
	return len(tracker.observed) == len(tracker.requested)
}

// readNewReobservationRequestIDs returns the update ids of a request file (one per line) that the tracker has not
// requested yet, in file order, and adds them to the requested set. On an error the requested set stays unchanged.
func (tracker *reobservedUpdateTracker) readNewReobservationRequestIDs(fileContent string) ([]cantonclient.UpdateID, error) {
	var newUpdateIDs []cantonclient.UpdateID
	newUpdateIDSet := make(map[cantonclient.UpdateID]struct{})
	for _, line := range strings.Split(fileContent, "\n") {
		updateIDText := strings.TrimSpace(line)
		if updateIDText == "" {
			continue
		}
		updateID, err := cantonclient.ParseUpdateID(updateIDText)
		if err != nil {
			return nil, err
		}
		if _, requested := tracker.requested[updateID]; requested {
			continue
		}
		if _, duplicate := newUpdateIDSet[updateID]; duplicate {
			continue
		}
		newUpdateIDSet[updateID] = struct{}{}
		newUpdateIDs = append(newUpdateIDs, updateID)
		if len(tracker.requested)+len(newUpdateIDs) > localNetMaxReobservations {
			return nil, fmt.Errorf("at most %d requested update ids, got more", localNetMaxReobservations)
		}
	}
	for _, updateID := range newUpdateIDs {
		tracker.requested[updateID] = struct{}{}
	}
	return newUpdateIDs, nil
}

// drainPublications writes each queued publication, at most maxDrained, and returns the count.
func drainPublications(msgC <-chan *common.MessagePublication, maxDrained int,
	writePublication func(*common.MessagePublication) error) (int, error) {
	for drainedCount := 0; drainedCount < maxDrained; drainedCount++ {
		select {
		case publication := <-msgC:
			if err := writePublication(publication); err != nil {
				return drainedCount, err
			}
		default:
			return drainedCount, nil
		}
	}
	return maxDrained, nil
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

func mustParseLocalNetTestUpdateID(t *testing.T, index int) cantonclient.UpdateID {
	t.Helper()
	updateID, err := cantonclient.ParseUpdateID(localNetTestUpdateIDText(index))
	require.NoError(t, err)
	return updateID
}

func TestReobservedUpdateTracker(t *testing.T) {
	firstUpdateID := mustParseLocalNetTestUpdateID(t, 0)
	secondUpdateID := mustParseLocalNetTestUpdateID(t, 1)
	unrequestedUpdateID := mustParseLocalNetTestUpdateID(t, 2)
	testCases := []struct {
		name                 string
		requested            []cantonclient.UpdateID
		recorded             []cantonclient.UpdateID
		expectedAllObserved  bool
		expectedErrorMessage string
	}{
		{name: "no requests", expectedAllObserved: true},
		{name: "one of two observed", requested: []cantonclient.UpdateID{firstUpdateID, secondUpdateID},
			recorded: []cantonclient.UpdateID{firstUpdateID}, expectedAllObserved: false},
		{name: "one update with two messages", requested: []cantonclient.UpdateID{firstUpdateID, secondUpdateID},
			recorded: []cantonclient.UpdateID{firstUpdateID, firstUpdateID}, expectedAllObserved: false},
		{name: "both observed", requested: []cantonclient.UpdateID{firstUpdateID, secondUpdateID},
			recorded: []cantonclient.UpdateID{firstUpdateID, secondUpdateID}, expectedAllObserved: true},
		{name: "unrequested id", requested: []cantonclient.UpdateID{firstUpdateID},
			recorded: []cantonclient.UpdateID{unrequestedUpdateID}, expectedErrorMessage: "not requested"},
	}
	for _, testCase := range testCases {
		t.Run(testCase.name, func(t *testing.T) {
			tracker := newReobservedUpdateTracker(testCase.requested)
			var recordErr error
			for _, updateID := range testCase.recorded {
				if recordErr = tracker.recordReobservation(updateID); recordErr != nil {
					break
				}
			}
			if testCase.expectedErrorMessage != "" {
				require.ErrorContains(t, recordErr, testCase.expectedErrorMessage)
				return
			}
			require.NoError(t, recordErr)
			require.Equal(t, testCase.expectedAllObserved, tracker.allRequestedUpdatesObserved())
		})
	}
}

func TestDrainPublications(t *testing.T) {
	writeFailure := errors.New("disk full")
	testCases := []struct {
		name                  string
		queuedCount           int
		maxDrained            int
		failOnWrite           int
		expectedDrainedCount  int
		expectedRemainingSize int
		expectedError         error
	}{
		{name: "empty channel", queuedCount: 0, maxDrained: 64, expectedDrainedCount: 0, expectedRemainingSize: 0},
		{name: "three queued", queuedCount: 3, maxDrained: 64, expectedDrainedCount: 3, expectedRemainingSize: 0},
		{name: "drain bound", queuedCount: 5, maxDrained: 2, expectedDrainedCount: 2, expectedRemainingSize: 3},
		{name: "writer error", queuedCount: 3, maxDrained: 64, failOnWrite: 2, expectedDrainedCount: 1,
			expectedRemainingSize: 1, expectedError: writeFailure},
	}
	for _, testCase := range testCases {
		t.Run(testCase.name, func(t *testing.T) {
			msgC := make(chan *common.MessagePublication, 64)
			for sequence := 1; sequence <= testCase.queuedCount; sequence++ {
				msgC <- &common.MessagePublication{Sequence: uint64(sequence)} // #nosec G115 -- small test count
			}
			var writtenSequences []uint64
			drainedCount, err := drainPublications(msgC, testCase.maxDrained, func(publication *common.MessagePublication) error {
				if len(writtenSequences)+1 == testCase.failOnWrite {
					return writeFailure
				}
				writtenSequences = append(writtenSequences, publication.Sequence)
				return nil
			})
			if testCase.expectedError != nil {
				require.ErrorIs(t, err, testCase.expectedError)
			} else {
				require.NoError(t, err)
			}
			require.Equal(t, testCase.expectedDrainedCount, drainedCount)
			require.Len(t, writtenSequences, testCase.expectedDrainedCount)
			for position, sequence := range writtenSequences {
				require.Equal(t, uint64(position+1), sequence) // #nosec G115 -- small test count
			}
			require.Len(t, msgC, testCase.expectedRemainingSize)
		})
	}
}

func TestReadNewReobservationRequestIDs(t *testing.T) {
	updateIDTexts := make([]string, localNetMaxReobservations+1)
	for index := range updateIDTexts {
		updateIDTexts[index] = localNetTestUpdateIDText(index)
	}
	type requestFileRead struct {
		fileContent           string
		expectedUpdateIDTexts []string
		expectedErrorMessage  string
	}
	testCases := []struct {
		name             string
		startUpdateIDs   []string
		requestFileReads []requestFileRead
	}{
		{name: "empty file", requestFileReads: []requestFileRead{{fileContent: "", expectedUpdateIDTexts: nil}}},
		{name: "two ids", requestFileReads: []requestFileRead{
			{fileContent: updateIDTexts[0] + "\n" + updateIDTexts[1] + "\n", expectedUpdateIDTexts: updateIDTexts[0:2]}}},
		{name: "same two ids again", requestFileReads: []requestFileRead{
			{fileContent: updateIDTexts[0] + "\n" + updateIDTexts[1] + "\n", expectedUpdateIDTexts: updateIDTexts[0:2]},
			{fileContent: updateIDTexts[0] + "\n" + updateIDTexts[1] + "\n", expectedUpdateIDTexts: nil}}},
		{name: "appended id", requestFileReads: []requestFileRead{
			{fileContent: updateIDTexts[0] + "\n", expectedUpdateIDTexts: updateIDTexts[0:1]},
			{fileContent: updateIDTexts[0] + "\n" + updateIDTexts[1] + "\n", expectedUpdateIDTexts: updateIDTexts[1:2]}}},
		{name: "start id skipped", startUpdateIDs: updateIDTexts[0:1], requestFileReads: []requestFileRead{
			{fileContent: updateIDTexts[0] + "\n" + updateIDTexts[1] + "\n", expectedUpdateIDTexts: updateIDTexts[1:2]}}},
		{name: "duplicate line", requestFileReads: []requestFileRead{
			{fileContent: updateIDTexts[0] + "\n" + updateIDTexts[0] + "\n", expectedUpdateIDTexts: updateIDTexts[0:1]}}},
		{name: "blank lines and spaces", requestFileReads: []requestFileRead{
			{fileContent: "\n  " + updateIDTexts[0] + " \r\n\n", expectedUpdateIDTexts: updateIDTexts[0:1]}}},
		{name: "invalid id", requestFileReads: []requestFileRead{
			{fileContent: updateIDTexts[0] + "\n12345\n", expectedErrorMessage: "got 5 characters, want 68"}}},
		{name: "invalid id leaves the state unchanged", requestFileReads: []requestFileRead{
			{fileContent: updateIDTexts[0] + "\nnot-an-id\n", expectedErrorMessage: "want 68"},
			{fileContent: updateIDTexts[0] + "\n", expectedUpdateIDTexts: updateIDTexts[0:1]}}},
		{name: "more than the maximum", requestFileReads: []requestFileRead{
			{fileContent: strings.Join(updateIDTexts, "\n"), expectedErrorMessage: "at most 64"}}},
		{name: "maximum reached with the start ids", startUpdateIDs: updateIDTexts[0:localNetMaxReobservations],
			requestFileReads: []requestFileRead{
				{fileContent: updateIDTexts[localNetMaxReobservations], expectedErrorMessage: "at most 64"}}},
	}
	for _, testCase := range testCases {
		t.Run(testCase.name, func(t *testing.T) {
			startUpdateIDs, err := parseReobserveUpdateIDs(strings.Join(testCase.startUpdateIDs, ","))
			require.NoError(t, err)
			tracker := newReobservedUpdateTracker(startUpdateIDs)
			require.NotEmpty(t, testCase.requestFileReads)
			for _, read := range testCase.requestFileReads {
				newUpdateIDs, err := tracker.readNewReobservationRequestIDs(read.fileContent)
				if read.expectedErrorMessage != "" {
					require.ErrorContains(t, err, read.expectedErrorMessage)
					require.Nil(t, newUpdateIDs)
					continue
				}
				require.NoError(t, err)
				require.Len(t, newUpdateIDs, len(read.expectedUpdateIDTexts))
				for position, updateID := range newUpdateIDs {
					require.Equal(t, read.expectedUpdateIDTexts[position], updateID.String())
				}
			}
		})
	}
}

func TestReobservationRequestFromFileMustBeObservedBeforeStop(t *testing.T) {
	tracker := newReobservedUpdateTracker(nil)
	require.True(t, tracker.allRequestedUpdatesObserved())
	newUpdateIDs, err := tracker.readNewReobservationRequestIDs(localNetTestUpdateIDText(0) + "\n")
	require.NoError(t, err)
	require.Len(t, newUpdateIDs, 1)
	require.False(t, tracker.allRequestedUpdatesObserved())
	require.NoError(t, tracker.recordReobservation(newUpdateIDs[0]))
	require.True(t, tracker.allRequestedUpdatesObserved())
}
