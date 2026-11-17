package cantonclient

import (
	"strings"
	"testing"
	"time"

	"github.com/stretchr/testify/assert"
	"github.com/stretchr/testify/require"
)

var (
	currentPackageIDHex  = strings.Repeat("a1", PackageIDHexLen/2)
	retiringPackageIDHex = strings.Repeat("b2", PackageIDHexLen/2)
	otherPackageIDHex    = strings.Repeat("c3", PackageIDHexLen/2)
)

var testStartupWallClock = time.Date(2026, 11, 17, 12, 0, 0, 0, time.UTC)

const testRetiringDeadlineText = "2026-11-18T12:00:00Z"

var testRetiringDeadline = time.Date(2026, 11, 18, 12, 0, 0, 0, time.UTC)

func TestParsePackageID(t *testing.T) {
	cases := []struct {
		name         string
		packageIDHex string
		wantErr      string
	}{
		{name: "lowercase 64-hex id", packageIDHex: currentPackageIDHex},
		{name: "empty", packageIDHex: "", wantErr: "64 lowercase hex"},
		{name: "63 hex characters", packageIDHex: currentPackageIDHex[:63], wantErr: "64 lowercase hex"},
		{name: "65 hex characters", packageIDHex: currentPackageIDHex + "a", wantErr: "64 lowercase hex"},
		{name: "uppercase hex", packageIDHex: strings.ToUpper(currentPackageIDHex), wantErr: "64 lowercase hex"},
		{name: "0x prefix", packageIDHex: "0x" + currentPackageIDHex[2:], wantErr: "64 lowercase hex"},
		{name: "non-hex character", packageIDHex: currentPackageIDHex[:63] + "g", wantErr: "64 lowercase hex"},
		{name: "surrounding whitespace", packageIDHex: " " + currentPackageIDHex[1:], wantErr: "64 lowercase hex"},
	}
	for _, tc := range cases {
		t.Run(tc.name, func(t *testing.T) {
			packageID, err := ParsePackageID(tc.packageIDHex)
			if tc.wantErr != "" {
				require.ErrorContains(t, err, tc.wantErr)
				assert.Equal(t, PackageID{}, packageID)
				return
			}
			require.NoError(t, err)
			assert.Equal(t, tc.packageIDHex, packageID.String())
		})
	}
}

func TestParseAcceptedPackageIDs(t *testing.T) {
	maximumDeadline := testStartupWallClock.Add(MaxRetiringPackageWindow)
	cases := []struct {
		name               string
		currentHex         string
		retiring           RetiringPackageConfig
		wantErr            string
		wantDescription    string
		wantRetiringHex    string
		wantRetiringBefore time.Time
	}{
		{name: "current only", currentHex: currentPackageIDHex, wantDescription: currentPackageIDHex},
		{
			name:               "retiring id with deadline",
			currentHex:         currentPackageIDHex,
			retiring:           RetiringPackageConfig{PackageIDHex: retiringPackageIDHex, AcceptedUntilUTC: testRetiringDeadlineText},
			wantDescription:    currentPackageIDHex + "," + retiringPackageIDHex,
			wantRetiringHex:    retiringPackageIDHex,
			wantRetiringBefore: testRetiringDeadline,
		},
		{
			name:               "deadline exactly the maximum window after startup",
			currentHex:         currentPackageIDHex,
			retiring:           RetiringPackageConfig{PackageIDHex: retiringPackageIDHex, AcceptedUntilUTC: maximumDeadline.Format(RetiringPackageDeadlineLayout)},
			wantDescription:    currentPackageIDHex + "," + retiringPackageIDHex,
			wantRetiringHex:    retiringPackageIDHex,
			wantRetiringBefore: maximumDeadline,
		},
		{
			name:               "deadline in the past",
			currentHex:         currentPackageIDHex,
			retiring:           RetiringPackageConfig{PackageIDHex: retiringPackageIDHex, AcceptedUntilUTC: "2026-11-01T00:00:00Z"},
			wantDescription:    currentPackageIDHex + "," + retiringPackageIDHex,
			wantRetiringHex:    retiringPackageIDHex,
			wantRetiringBefore: time.Date(2026, 11, 1, 0, 0, 0, 0, time.UTC),
		},
		{
			name:       "deadline one second over the maximum window",
			currentHex: currentPackageIDHex,
			retiring:   RetiringPackageConfig{PackageIDHex: retiringPackageIDHex, AcceptedUntilUTC: maximumDeadline.Add(time.Second).Format(RetiringPackageDeadlineLayout)},
			wantErr:    "more than",
		},
		{name: "retiring id without deadline", currentHex: currentPackageIDHex, retiring: RetiringPackageConfig{PackageIDHex: retiringPackageIDHex}, wantErr: "requires an accepted-until deadline"},
		{name: "deadline without retiring id", currentHex: currentPackageIDHex, retiring: RetiringPackageConfig{AcceptedUntilUTC: testRetiringDeadlineText}, wantErr: "requires a retiring core package id"},
		{name: "deadline with offset", currentHex: currentPackageIDHex, retiring: RetiringPackageConfig{PackageIDHex: retiringPackageIDHex, AcceptedUntilUTC: "2026-11-18T13:00:00+01:00"}, wantErr: "RFC 3339 UTC"},
		{name: "deadline without seconds", currentHex: currentPackageIDHex, retiring: RetiringPackageConfig{PackageIDHex: retiringPackageIDHex, AcceptedUntilUTC: "2026-11-18T12:00Z"}, wantErr: "RFC 3339 UTC"},
		{name: "deadline without zone", currentHex: currentPackageIDHex, retiring: RetiringPackageConfig{PackageIDHex: retiringPackageIDHex, AcceptedUntilUTC: "2026-11-18T12:00:00"}, wantErr: "RFC 3339 UTC"},
		{name: "deadline with fractional seconds", currentHex: currentPackageIDHex, retiring: RetiringPackageConfig{PackageIDHex: retiringPackageIDHex, AcceptedUntilUTC: "2026-11-18T12:00:00.5Z"}, wantErr: "RFC 3339 UTC"},
		{name: "deadline with lowercase zone", currentHex: currentPackageIDHex, retiring: RetiringPackageConfig{PackageIDHex: retiringPackageIDHex, AcceptedUntilUTC: "2026-11-18t12:00:00z"}, wantErr: "RFC 3339 UTC"},
		{name: "retiring equals current", currentHex: currentPackageIDHex, retiring: RetiringPackageConfig{PackageIDHex: currentPackageIDHex, AcceptedUntilUTC: testRetiringDeadlineText}, wantErr: "must differ"},
		{name: "malformed current", currentHex: "pkg", wantErr: "current core package id"},
		{name: "malformed retiring", currentHex: currentPackageIDHex, retiring: RetiringPackageConfig{PackageIDHex: "pkg", AcceptedUntilUTC: testRetiringDeadlineText}, wantErr: "retiring core package id"},
		{name: "retiring without current", retiring: RetiringPackageConfig{PackageIDHex: retiringPackageIDHex, AcceptedUntilUTC: testRetiringDeadlineText}, wantErr: "current core package id"},
		{name: "missing current", wantErr: "current core package id"},
		{name: "two ids in retiring", currentHex: currentPackageIDHex, retiring: RetiringPackageConfig{PackageIDHex: retiringPackageIDHex + "," + otherPackageIDHex, AcceptedUntilUTC: testRetiringDeadlineText}, wantErr: "retiring core package id"},
	}
	for _, tc := range cases {
		t.Run(tc.name, func(t *testing.T) {
			accepted, err := ParseAcceptedPackageIDs(tc.currentHex, tc.retiring, testStartupWallClock)
			if tc.wantErr != "" {
				require.ErrorContains(t, err, tc.wantErr)
				assert.Equal(t, AcceptedPackageIDs{}, accepted)
				return
			}
			require.NoError(t, err)
			assert.False(t, accepted.MatchesAnyPackage())
			assert.False(t, accepted.IsEmpty())
			assert.Equal(t, tc.wantDescription, accepted.String())
			current, hasCurrent := accepted.CurrentPackageID()
			require.True(t, hasCurrent)
			assert.Equal(t, tc.currentHex, current.String())
			retiring, hasRetiring := accepted.RetiringPackageID()
			assert.Equal(t, tc.wantRetiringHex != "", hasRetiring)
			assert.Equal(t, tc.wantRetiringHex, retiring.PackageID().String())
			assert.True(t, tc.wantRetiringBefore.Equal(retiring.AcceptedBeforeRecordTime()))
		})
	}
}

func TestVerdictForPublish(t *testing.T) {
	window, err := ParseAcceptedPackageIDs(currentPackageIDHex, RetiringPackageConfig{PackageIDHex: retiringPackageIDHex, AcceptedUntilUTC: testRetiringDeadlineText}, testStartupWallClock)
	require.NoError(t, err)
	currentOnly, err := ParseAcceptedPackageIDs(currentPackageIDHex, RetiringPackageConfig{}, testStartupWallClock)
	require.NoError(t, err)
	beforeDeadline := testRetiringDeadline.Add(-time.Nanosecond)
	cases := []struct {
		name                  string
		accepted              AcceptedPackageIDs
		executingPackageIDHex string
		recordTime            time.Time
		want                  PublishPackageVerdict
	}{
		{name: "current id before the deadline", accepted: window, executingPackageIDHex: currentPackageIDHex, recordTime: beforeDeadline, want: PublishPackageVerdictAcceptedCurrent},
		{name: "current id after the deadline", accepted: window, executingPackageIDHex: currentPackageIDHex, recordTime: testRetiringDeadline.Add(time.Hour), want: PublishPackageVerdictAcceptedCurrent},
		{name: "current id without record time", accepted: window, executingPackageIDHex: currentPackageIDHex, want: PublishPackageVerdictAcceptedCurrent},
		{name: "retiring id one nanosecond before the deadline", accepted: window, executingPackageIDHex: retiringPackageIDHex, recordTime: beforeDeadline, want: PublishPackageVerdictAcceptedRetiring},
		{name: "retiring id at the deadline", accepted: window, executingPackageIDHex: retiringPackageIDHex, recordTime: testRetiringDeadline, want: PublishPackageVerdictRejectedRetiringAfterDeadline},
		{name: "retiring id after the deadline", accepted: window, executingPackageIDHex: retiringPackageIDHex, recordTime: testRetiringDeadline.Add(time.Second), want: PublishPackageVerdictRejectedRetiringAfterDeadline},
		{name: "retiring id without record time", accepted: window, executingPackageIDHex: retiringPackageIDHex, want: PublishPackageVerdictRejectedMissingRecordTime},
		{name: "unknown id", accepted: window, executingPackageIDHex: otherPackageIDHex, recordTime: beforeDeadline, want: PublishPackageVerdictRejectedUnpinned},
		{name: "empty id", accepted: window, executingPackageIDHex: "", recordTime: beforeDeadline, want: PublishPackageVerdictRejectedUnpinned},
		{name: "retiring id without a window", accepted: currentOnly, executingPackageIDHex: retiringPackageIDHex, recordTime: beforeDeadline, want: PublishPackageVerdictRejectedUnpinned},
		{name: "wildcard", accepted: AcceptAnyPackageID(), executingPackageIDHex: "lookalike-package", want: PublishPackageVerdictAcceptedCurrent},
		{name: "wildcard with empty id", accepted: AcceptAnyPackageID(), executingPackageIDHex: "", want: PublishPackageVerdictAcceptedCurrent},
		{name: "zero value", accepted: AcceptedPackageIDs{}, executingPackageIDHex: currentPackageIDHex, recordTime: beforeDeadline, want: PublishPackageVerdictRejectedUnpinned},
	}
	for _, tc := range cases {
		t.Run(tc.name, func(t *testing.T) {
			assert.Equal(t, tc.want, tc.accepted.VerdictForPublish(tc.executingPackageIDHex, tc.recordTime))
		})
	}
}

func TestAcceptAnyPackageID(t *testing.T) {
	accepted := AcceptAnyPackageID()

	assert.True(t, accepted.MatchesAnyPackage())
	assert.False(t, accepted.IsEmpty())
	_, hasCurrent := accepted.CurrentPackageID()
	assert.False(t, hasCurrent)
	_, hasRetiring := accepted.RetiringPackageID()
	assert.False(t, hasRetiring)
	assert.Equal(t, "*", accepted.String())
}

func TestZeroAcceptedPackageIDsAcceptsNothing(t *testing.T) {
	var accepted AcceptedPackageIDs

	assert.False(t, accepted.MatchesAnyPackage())
	assert.True(t, accepted.IsEmpty())
	_, hasCurrent := accepted.CurrentPackageID()
	assert.False(t, hasCurrent)
	assert.Equal(t, "", accepted.String())
}
