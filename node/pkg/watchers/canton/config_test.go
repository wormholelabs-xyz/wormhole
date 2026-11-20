package canton

import (
	"context"
	"encoding/json"
	"os"
	"reflect"
	"strings"
	"testing"
	"time"

	"github.com/certusone/wormhole/node/pkg/cantonclient"
	"github.com/certusone/wormhole/node/pkg/common"
	gossipv1 "github.com/certusone/wormhole/node/pkg/proto/gossip/v1"
	"github.com/stretchr/testify/assert"
	"github.com/stretchr/testify/require"
	"github.com/wormhole-foundation/wormhole/sdk/vaa"
	"google.golang.org/grpc/credentials/insecure"
)

const (
	testGovernanceParty   = readAsGovernanceParty
	testCurrentPackageID  = "1111111111111111111111111111111111111111111111111111111111111111"
	testRetiringPackageID = "2222222222222222222222222222222222222222222222222222222222222222"
	testOtherPackageID    = "3333333333333333333333333333333333333333333333333333333333333333"

	testCurrentPackageVersion  = "0.8.1"
	testRetiringPackageVersion = "0.8.0"
)

func createWith(t *testing.T, pkgID string, env common.Environment) error {
	t.Helper()
	return createWithReadAs(t, pkgID, testGovernanceParty, env)
}

func createWithReadAs(t *testing.T, pkgID string, readAsParty string, env common.Environment) error {
	t.Helper()
	flags := corePackageFlags{currentPackageID: pkgID}
	if pkgID != "" {
		flags.currentPackageVersion = testCurrentPackageVersion
	}
	return createWithPackageFlags(t, flags, readAsParty, env)
}

// corePackageFlags mirrors the --cantonPackageID and --cantonRetiringPackage* flags.
type corePackageFlags struct {
	currentPackageID             string
	currentPackageVersion        string
	retiringPackageID            string
	retiringPackageAcceptedUntil string
	retiringPackageVersion       string
}

// testApprovedCorePackages approves the test current and retiring ids at their test versions.
func testApprovedCorePackages(t *testing.T) *cantonclient.ApprovedCorePackages {
	t.Helper()
	approved, err := cantonclient.ParseApprovedCorePackages([]byte(`[` +
		`{"packageId":"` + testCurrentPackageID + `","version":"` + testCurrentPackageVersion + `"},` +
		`{"packageId":"` + testRetiringPackageID + `","version":"` + testRetiringPackageVersion + `"}]`))
	require.NoError(t, err)
	return &approved
}

func createWithPackageFlags(t *testing.T, flags corePackageFlags, readAsParty string, env common.Environment) error {
	t.Helper()
	wc := &WatcherConfig{
		NetworkID:                    "canton",
		ChainID:                      vaa.ChainIDCanton,
		Rpc:                          "canton:6865",
		PackageID:                    flags.currentPackageID,
		PackageVersion:               flags.currentPackageVersion,
		RetiringPackageID:            flags.retiringPackageID,
		RetiringPackageAcceptedUntil: flags.retiringPackageAcceptedUntil,
		RetiringPackageVersion:       flags.retiringPackageVersion,
		ReadAsParty:                  readAsParty,
	}
	_, err := wc.buildWatcher(
		*testApprovedCorePackages(t),
		make(chan *common.MessagePublication, 1),
		make(chan *gossipv1.ObservationRequest, 1),
		env,
	)
	return err
}

// TestCreateRequiresPackageIDOutsideDevMode locks in the anti-spoofing guard: outside dev mode the package id is
// exactly 64 lowercase hex characters. Dev mode also accepts the empty (wildcard) package id.
func TestCreateRequiresPackageIDOutsideDevMode(t *testing.T) {
	testCases := []struct {
		name                 string
		packageIDText        string
		environment          common.Environment
		expectedErrorMessage string
	}{
		{name: "MainNet empty", packageIDText: "", environment: common.MainNet, expectedErrorMessage: "PackageID must be set"},
		{name: "TestNet empty", packageIDText: "", environment: common.TestNet, expectedErrorMessage: "PackageID must be set"},
		{name: "MainNet short", packageIDText: "abc123", environment: common.MainNet, expectedErrorMessage: "got 6 characters"},
		{name: "TestNet short", packageIDText: "abc123", environment: common.TestNet, expectedErrorMessage: "got 6 characters"},
		{name: "MainNet 63 hex", packageIDText: testCurrentPackageID[:63], environment: common.MainNet, expectedErrorMessage: "got 63 characters"},
		{name: "MainNet uppercase", packageIDText: strings.Repeat("A", 64), environment: common.MainNet, expectedErrorMessage: "found 'A'"},
		{name: "MainNet 64 hex", packageIDText: testCurrentPackageID, environment: common.MainNet},
		{name: "TestNet 64 hex", packageIDText: testCurrentPackageID, environment: common.TestNet},
		{name: "UnsafeDevNet empty", packageIDText: "", environment: common.UnsafeDevNet},
		{name: "UnsafeDevNet 64 hex", packageIDText: testCurrentPackageID, environment: common.UnsafeDevNet},
		{name: "UnsafeDevNet short", packageIDText: "abc123", environment: common.UnsafeDevNet, expectedErrorMessage: "got 6 characters"},
	}
	for _, testCase := range testCases {
		t.Run(testCase.name, func(t *testing.T) {
			err := createWith(t, testCase.packageIDText, testCase.environment)
			if testCase.expectedErrorMessage != "" {
				require.ErrorContains(t, err, testCase.expectedErrorMessage)
				return
			}
			require.NoError(t, err)
		})
	}
}

// retiringDeadlineFromNow is a deadline offset from the wall clock, in the flag format.
func retiringDeadlineFromNow(offset time.Duration) string {
	return time.Now().UTC().Add(offset).Truncate(time.Second).Format("2006-01-02T15:04:05Z")
}

func TestCreateValidatesCorePackageIDs(t *testing.T) {
	inOneDay := retiringDeadlineFromNow(24 * time.Hour)
	current := corePackageFlags{currentPackageID: testCurrentPackageID, currentPackageVersion: testCurrentPackageVersion}
	window := func(retiringPackageID string, retiringPackageAcceptedUntil string) corePackageFlags {
		flags := current
		flags.retiringPackageID = retiringPackageID
		flags.retiringPackageAcceptedUntil = retiringPackageAcceptedUntil
		flags.retiringPackageVersion = testRetiringPackageVersion
		return flags
	}
	withoutVersions := func(flags corePackageFlags) corePackageFlags {
		flags.currentPackageVersion = ""
		flags.retiringPackageVersion = ""
		return flags
	}
	withCurrentID := func(flags corePackageFlags, currentPackageID string) corePackageFlags {
		flags.currentPackageID = currentPackageID
		return flags
	}
	cases := []struct {
		name    string
		flags   corePackageFlags
		env     common.Environment
		wantErr string
	}{
		{name: "valid single", flags: current, env: common.MainNet},
		{name: "valid window", flags: window(testRetiringPackageID, inOneDay), env: common.MainNet},
		{name: "valid window in dev mode", flags: window(testRetiringPackageID, inOneDay), env: common.UnsafeDevNet},
		{name: "window with a past deadline", flags: window(testRetiringPackageID, retiringDeadlineFromNow(-time.Hour)), env: common.MainNet},
		{name: "window longer than the maximum", flags: window(testRetiringPackageID, retiringDeadlineFromNow(8*24*time.Hour)), env: common.MainNet, wantErr: "more than"},
		{name: "retiring id without deadline", flags: window(testRetiringPackageID, ""), env: common.MainNet, wantErr: "requires an accepted-until deadline"},
		{name: "deadline without retiring id", flags: corePackageFlags{currentPackageID: testCurrentPackageID, currentPackageVersion: testCurrentPackageVersion, retiringPackageAcceptedUntil: inOneDay}, env: common.MainNet, wantErr: "requires a retiring core package id"},
		{name: "deadline with offset", flags: window(testRetiringPackageID, "2026-11-18T13:00:00+01:00"), env: common.MainNet, wantErr: "RFC 3339 UTC"},
		{name: "duplicate", flags: window(testCurrentPackageID, inOneDay), env: common.MainNet, wantErr: "must differ"},
		{name: "malformed current", flags: withCurrentID(current, "abc123"), env: common.MainNet, wantErr: "current core package id"},
		{name: "malformed current in dev mode", flags: withCurrentID(current, "pkg"), env: common.UnsafeDevNet, wantErr: "current core package id"},
		{name: "uppercase current", flags: withCurrentID(current, "AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA"), env: common.MainNet, wantErr: "current core package id"},
		{name: "malformed retiring", flags: window("abc123", inOneDay), env: common.MainNet, wantErr: "retiring core package id"},
		{name: "three ids", flags: window(testRetiringPackageID+","+testOtherPackageID, inOneDay), env: common.MainNet, wantErr: "retiring core package id"},
		{name: "three ids in current", flags: withCurrentID(window(testOtherPackageID, inOneDay), testCurrentPackageID+","+testRetiringPackageID), env: common.MainNet, wantErr: "current core package id"},
		{name: "missing current outside dev mode", flags: withCurrentID(window(testRetiringPackageID, inOneDay), ""), env: common.MainNet, wantErr: "PackageID must be set"},
		{name: "missing current in testnet", flags: withCurrentID(window(testRetiringPackageID, inOneDay), ""), env: common.TestNet, wantErr: "PackageID must be set"},
		{name: "retiring without current in dev mode", flags: withCurrentID(window(testRetiringPackageID, inOneDay), ""), env: common.UnsafeDevNet, wantErr: "current core package id"},
		{name: "deadline alone in dev mode", flags: corePackageFlags{retiringPackageAcceptedUntil: inOneDay}, env: common.UnsafeDevNet, wantErr: "current core package id"},
		{name: "current version alone in dev mode", flags: corePackageFlags{currentPackageVersion: testCurrentPackageVersion}, env: common.UnsafeDevNet, wantErr: "current core package id"},
		{name: "missing current version outside dev mode", flags: corePackageFlags{currentPackageID: testCurrentPackageID}, env: common.MainNet, wantErr: "PackageVersion must be set"},
		{name: "missing retiring version outside dev mode", flags: corePackageFlags{currentPackageID: testCurrentPackageID, currentPackageVersion: testCurrentPackageVersion, retiringPackageID: testRetiringPackageID, retiringPackageAcceptedUntil: inOneDay}, env: common.TestNet, wantErr: "RetiringPackageVersion must be set"},
		{name: "malformed current version", flags: corePackageFlags{currentPackageID: testCurrentPackageID, currentPackageVersion: "0.8"}, env: common.MainNet, wantErr: "MAJOR.MINOR.PATCH"},
		{name: "malformed retiring version", flags: corePackageFlags{currentPackageID: testCurrentPackageID, currentPackageVersion: testCurrentPackageVersion, retiringPackageID: testRetiringPackageID, retiringPackageAcceptedUntil: inOneDay, retiringPackageVersion: "0.8.0.1"}, env: common.MainNet, wantErr: "MAJOR.MINOR.PATCH"},
		{name: "retiring version without retiring id", flags: corePackageFlags{currentPackageID: testCurrentPackageID, currentPackageVersion: testCurrentPackageVersion, retiringPackageVersion: testRetiringPackageVersion}, env: common.MainNet, wantErr: "requires a retiring core package id"},
		{name: "window without versions in dev mode", flags: withoutVersions(window(testRetiringPackageID, inOneDay)), env: common.UnsafeDevNet},
		{name: "unapproved current outside dev mode", flags: withCurrentID(current, testOtherPackageID), env: common.MainNet, wantErr: "not in the approved"},
		{name: "current at an unapproved version outside dev mode", flags: corePackageFlags{currentPackageID: testCurrentPackageID, currentPackageVersion: "0.8.9"}, env: common.TestNet, wantErr: "approved as"},
		{name: "unapproved retiring outside dev mode", flags: window(testOtherPackageID, inOneDay), env: common.MainNet, wantErr: "not in the approved"},
		{name: "unapproved current in dev mode", flags: withCurrentID(current, testOtherPackageID), env: common.UnsafeDevNet},
		{name: "wildcard in dev mode", env: common.UnsafeDevNet},
	}
	for _, tc := range cases {
		t.Run(tc.name, func(t *testing.T) {
			err := createWithPackageFlags(t, tc.flags, testGovernanceParty, tc.env)
			if tc.wantErr == "" {
				require.NoError(t, err)
				return
			}
			require.ErrorContains(t, err, tc.wantErr)
		})
	}
}

// TestCreateParsesReadAsParty: the wildcard stream is dev-only; a malformed party is refused in every mode.
func TestCreateParsesReadAsParty(t *testing.T) {
	cases := []struct {
		name        string
		readAsParty string
		env         common.Environment
		wantErr     string
	}{
		{name: "party on MainNet", readAsParty: testGovernanceParty, env: common.MainNet},
		{name: "party on TestNet", readAsParty: testGovernanceParty, env: common.TestNet},
		{name: "party in dev mode", readAsParty: testGovernanceParty, env: common.UnsafeDevNet},
		{name: "empty in dev mode", readAsParty: "", env: common.UnsafeDevNet},
		{name: "empty on MainNet", readAsParty: "", env: common.MainNet, wantErr: "ReadAsParty: cantonclient: reading as any party requires unsafe dev mode"},
		{name: "empty on TestNet", readAsParty: "", env: common.TestNet, wantErr: "ReadAsParty: cantonclient: reading as any party requires unsafe dev mode"},
		{name: "truncated fingerprint on MainNet", readAsParty: "guardianGovernance::1220abcd", env: common.MainNet, wantErr: "fingerprint length 8"},
		{name: "truncated fingerprint in dev mode", readAsParty: "guardianGovernance::1220abcd", env: common.UnsafeDevNet, wantErr: "fingerprint length 8"},
		{name: "trailing newline on MainNet", readAsParty: testGovernanceParty + "\n", env: common.MainNet, wantErr: "fingerprint length 69"},
		{name: "hint only on MainNet", readAsParty: "guardianGovernance", env: common.MainNet, wantErr: "no '::'"},
	}
	for _, tc := range cases {
		t.Run(tc.name, func(t *testing.T) {
			err := createWithReadAs(t, testCurrentPackageID, tc.readAsParty, tc.env)

			if tc.wantErr != "" {
				require.ErrorContains(t, err, tc.wantErr)
				return
			}
			require.NoError(t, err)
		})
	}
}

func createWithAuth(t *testing.T, auth AuthConfig, env common.Environment) error {
	t.Helper()
	wc := &WatcherConfig{
		NetworkID:      "canton",
		ChainID:        vaa.ChainIDCanton,
		Rpc:            "canton:6865",
		PackageID:      testCurrentPackageID,
		PackageVersion: testCurrentPackageVersion,
		ReadAsParty:    testGovernanceParty,
		Auth:           auth,
	}
	_, err := wc.buildWatcher(
		*testApprovedCorePackages(t),
		make(chan *common.MessagePublication, 1),
		make(chan *gossipv1.ObservationRequest, 1),
		env,
	)
	return err
}

// TestCreateAuthConfigValidation locks in the all-or-nothing OAuth contract
// and the TLS requirement (no bearer auth over dev-mode plaintext).
func TestCreateAuthConfigValidation(t *testing.T) {
	full := AuthConfig{TokenURL: "https://kc.example/token", ClientID: "cid", ClientSecret: "sec"}

	// Complete config is accepted outside dev mode.
	require.NoError(t, createWithAuth(t, full, common.MainNet))
	// No auth remains fine everywhere.
	require.NoError(t, createWithAuth(t, AuthConfig{}, common.MainNet))
	require.NoError(t, createWithAuth(t, AuthConfig{}, common.UnsafeDevNet))

	// Partial configs are rejected.
	partials := []AuthConfig{
		{TokenURL: full.TokenURL},
		{ClientID: full.ClientID},
		{ClientSecret: full.ClientSecret},
		{TokenURL: full.TokenURL, ClientID: full.ClientID},
	}
	for i, p := range partials {
		err := createWithAuth(t, p, common.MainNet)
		require.Error(t, err, "partial auth config %d must be rejected", i)
		assert.Contains(t, err.Error(), "all be set together")
	}

	// Complete config is still rejected in dev mode (plaintext transport).
	err := createWithAuth(t, full, common.UnsafeDevNet)
	require.Error(t, err)
	assert.Contains(t, err.Error(), "unsafe dev mode")
}

// TestWatcherConfigFieldsAreExactlyTheFlagFields: each WatcherConfig field comes from a guardiand flag. A package
// test cannot set a transport through the production config.
func TestWatcherConfigFieldsAreExactlyTheFlagFields(t *testing.T) {
	configType := reflect.TypeOf(WatcherConfig{})
	fieldNames := make([]string, 0, configType.NumField())
	for fieldIndex := 0; fieldIndex < configType.NumField(); fieldIndex++ {
		fieldNames = append(fieldNames, configType.Field(fieldIndex).Name)
	}
	require.ElementsMatch(t, []string{
		"NetworkID", "ChainID", "Rpc", "PackageID", "RetiringPackageID", "RetiringPackageAcceptedUntil",
		"PackageVersion", "RetiringPackageVersion", "ReadAsParty", "Auth",
	}, fieldNames)
}

// TestCreateChecksTheCompiledApprovedList: Create approves only the ids in approved_core_packages.json.
func TestCreateChecksTheCompiledApprovedList(t *testing.T) {
	approvedListJSON, err := os.ReadFile("../../cantonclient/approved_core_packages.json")
	require.NoError(t, err)
	var compiledApprovedPackages []struct {
		PackageID string `json:"packageId"`
		Version   string `json:"version"`
	}
	require.NoError(t, json.Unmarshal(approvedListJSON, &compiledApprovedPackages))
	require.NotEmpty(t, compiledApprovedPackages)
	testCases := []struct {
		name                 string
		packageID            string
		packageVersion       string
		expectedErrorMessage string
	}{
		{name: "compiled id at its version", packageID: compiledApprovedPackages[0].PackageID, packageVersion: compiledApprovedPackages[0].Version},
		{name: "test id outside the compiled list", packageID: testCurrentPackageID, packageVersion: testCurrentPackageVersion, expectedErrorMessage: "not in the approved"},
	}
	for _, testCase := range testCases {
		t.Run(testCase.name, func(t *testing.T) {
			watcherConfig := &WatcherConfig{
				NetworkID:      "canton",
				ChainID:        vaa.ChainIDCanton,
				Rpc:            "canton:6865",
				PackageID:      testCase.packageID,
				PackageVersion: testCase.packageVersion,
				ReadAsParty:    testGovernanceParty,
			}
			runnable, reobserver, err := watcherConfig.Create(make(chan *common.MessagePublication, 1),
				make(chan *gossipv1.ObservationRequest, 1), nil, nil, nil, common.MainNet)
			require.Nil(t, reobserver)
			if testCase.expectedErrorMessage != "" {
				require.ErrorContains(t, err, testCase.expectedErrorMessage)
				require.Nil(t, runnable)
				return
			}
			require.NoError(t, err)
			require.NotNil(t, runnable)
		})
	}
}

func TestBuildWatcherSelectsTransportByEnvironment(t *testing.T) {
	completeAuth := AuthConfig{TokenURL: "https://kc.example/token", ClientID: "cid", ClientSecret: "sec"}
	testCases := []struct {
		name                    string
		environment             common.Environment
		auth                    AuthConfig
		expectedPlaintext       bool
		expectedDialOptionCount int
		expectedErrorMessage    string
	}{
		{name: "MainNet uses the TLS default", environment: common.MainNet},
		{name: "TestNet uses the TLS default", environment: common.TestNet},
		{name: "UnsafeDevNet uses plaintext", environment: common.UnsafeDevNet, expectedPlaintext: true},
		{name: "MainNet with OAuth adds one dial option", environment: common.MainNet, auth: completeAuth, expectedDialOptionCount: 1},
		{name: "UnsafeDevNet with OAuth fails", environment: common.UnsafeDevNet, auth: completeAuth, expectedErrorMessage: "unsafe dev mode"},
	}
	for _, testCase := range testCases {
		t.Run(testCase.name, func(t *testing.T) {
			watcherConfig := &WatcherConfig{
				NetworkID:      "canton",
				ChainID:        vaa.ChainIDCanton,
				Rpc:            "canton:6865",
				PackageID:      testCurrentPackageID,
				PackageVersion: testCurrentPackageVersion,
				ReadAsParty:    testGovernanceParty,
				Auth:           testCase.auth,
			}
			watcher, err := watcherConfig.buildWatcher(*testApprovedCorePackages(t), make(chan *common.MessagePublication, 1),
				make(chan *gossipv1.ObservationRequest, 1), testCase.environment)
			if testCase.expectedErrorMessage != "" {
				require.ErrorContains(t, err, testCase.expectedErrorMessage)
				require.Nil(t, watcher)
				return
			}
			require.NoError(t, err)
			if testCase.expectedPlaintext {
				require.Equal(t, insecure.NewCredentials().Info().SecurityProtocol, watcher.cantonTransportCreds().Info().SecurityProtocol)
			} else {
				require.Nil(t, watcher.cantonTransportCreds())
			}
			require.Len(t, watcher.cantonDialOpts(context.Background()), testCase.expectedDialOptionCount)
			currentPackageID, isPinned := watcher.acceptedPackageIDs.CurrentPackageID()
			require.True(t, isPinned)
			require.Equal(t, testCurrentPackageID, currentPackageID.String())
		})
	}
}
