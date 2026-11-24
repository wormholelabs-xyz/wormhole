package guardiand

import (
	"context"
	"encoding/json"
	"io"
	"net"
	"net/http"
	"testing"
	"time"

	"github.com/stretchr/testify/require"
	"go.uber.org/zap"

	"github.com/certusone/wormhole/node/pkg/cantonclient"
	"github.com/certusone/wormhole/node/pkg/cantondisclosure"
)

func validCantonDisclosureServiceFlags() cantonDisclosureServiceFlags {
	return cantonDisclosureServiceFlags{
		cantonRPC:          "participant.example:5001",
		readAsParty:        listedRegistrar,
		listenAddress:      "127.0.0.1:8080",
		statusAddress:      "127.0.0.1:6061",
		indexByteCapBytes:  cantondisclosure.ProductionIndexByteCapBytes,
		ledgerLagBound:     cantondisclosure.DefaultLedgerLagBound,
		resnapshotInterval: cantondisclosure.ProductionResnapshotInterval,
		logLevel:           "info",
	}
}

func TestParseCantonDisclosureServiceFlags(t *testing.T) {
	fullOAuth := cantonOAuthFlags{tokenURL: "https://idp.example/token", clientID: "cid", clientSecret: "sec"}
	testCases := []struct {
		name                 string
		mutate               func(flags *cantonDisclosureServiceFlags)
		expectedErrorMessage string
		expectOAuth          bool
	}{
		{name: "defaults", mutate: func(*cantonDisclosureServiceFlags) {}},
		{name: "OAuth", mutate: func(flags *cantonDisclosureServiceFlags) { flags.oauth = fullOAuth }, expectOAuth: true},
		{name: "OAuth with audience", mutate: func(flags *cantonDisclosureServiceFlags) {
			flags.oauth = fullOAuth
			flags.oauth.audience = "https://canton.network.global"
		}, expectOAuth: true},
		{name: "plaintext without OAuth", mutate: func(flags *cantonDisclosureServiceFlags) { flags.unsafePlaintext = true }},
		{name: "status server off", mutate: func(flags *cantonDisclosureServiceFlags) { flags.statusAddress = "" }},
		{name: "minimum ledger lag bound", mutate: func(flags *cantonDisclosureServiceFlags) { flags.ledgerLagBound = 10 * time.Second }},
		{name: "minimum resnapshot interval", mutate: func(flags *cantonDisclosureServiceFlags) { flags.resnapshotInterval = time.Minute }},

		{name: "empty cantonRPC", mutate: func(flags *cantonDisclosureServiceFlags) { flags.cantonRPC = "" }, expectedErrorMessage: "--cantonRPC"},
		{name: "cantonRPC without port", mutate: func(flags *cantonDisclosureServiceFlags) { flags.cantonRPC = "participant.example" }, expectedErrorMessage: "--cantonRPC"},
		{name: "cantonRPC with empty port", mutate: func(flags *cantonDisclosureServiceFlags) { flags.cantonRPC = "participant.example:" }, expectedErrorMessage: "missing port"},
		{name: "empty read party", mutate: func(flags *cantonDisclosureServiceFlags) { flags.readAsParty = "" }, expectedErrorMessage: "--cantonReadAsParty"},
		{name: "truncated read party", mutate: func(flags *cantonDisclosureServiceFlags) { flags.readAsParty = "guardianGovernance::1220abcd" }, expectedErrorMessage: "--cantonReadAsParty"},
		{name: "partial OAuth", mutate: func(flags *cantonDisclosureServiceFlags) {
			flags.oauth = cantonOAuthFlags{tokenURL: fullOAuth.tokenURL, clientID: fullOAuth.clientID}
		}, expectedErrorMessage: "must all be set together"},
		{name: "http token URL", mutate: func(flags *cantonDisclosureServiceFlags) {
			flags.oauth = fullOAuth
			flags.oauth.tokenURL = "http://idp.example/token"
		}, expectedErrorMessage: "--cantonAuthTokenURL"},
		{name: "OAuth over plaintext", mutate: func(flags *cantonDisclosureServiceFlags) {
			flags.oauth = fullOAuth
			flags.unsafePlaintext = true
		}, expectedErrorMessage: "OAuth requires TLS"},
		{name: "audience without OAuth", mutate: func(flags *cantonDisclosureServiceFlags) { flags.oauth = cantonOAuthFlags{audience: "aud"} }, expectedErrorMessage: "--cantonAuthAudience requires"},
		{name: "empty listen address", mutate: func(flags *cantonDisclosureServiceFlags) { flags.listenAddress = "" }, expectedErrorMessage: "--listenAddress"},
		{name: "listen address without port", mutate: func(flags *cantonDisclosureServiceFlags) { flags.listenAddress = "127.0.0.1" }, expectedErrorMessage: "--listenAddress"},
		{name: "malformed status address", mutate: func(flags *cantonDisclosureServiceFlags) { flags.statusAddress = "6061" }, expectedErrorMessage: "--statusAddr"},
		{name: "status address equal to the listen address", mutate: func(flags *cantonDisclosureServiceFlags) { flags.statusAddress = flags.listenAddress }, expectedErrorMessage: "--statusAddr must differ"},
		{name: "zero byte cap", mutate: func(flags *cantonDisclosureServiceFlags) { flags.indexByteCapBytes = 0 }, expectedErrorMessage: "--indexByteCapBytes"},
		{name: "negative byte cap", mutate: func(flags *cantonDisclosureServiceFlags) { flags.indexByteCapBytes = -1 }, expectedErrorMessage: "--indexByteCapBytes"},
		{name: "byte cap above maximum", mutate: func(flags *cantonDisclosureServiceFlags) { flags.indexByteCapBytes = 1<<40 + 1 }, expectedErrorMessage: "--indexByteCapBytes"},
		{name: "ledger lag bound below minimum", mutate: func(flags *cantonDisclosureServiceFlags) { flags.ledgerLagBound = 9 * time.Second }, expectedErrorMessage: "--ledgerLagBound"},
		{name: "ledger lag bound above maximum", mutate: func(flags *cantonDisclosureServiceFlags) { flags.ledgerLagBound = 2 * time.Hour }, expectedErrorMessage: "--ledgerLagBound"},
		{name: "resnapshot interval below minimum", mutate: func(flags *cantonDisclosureServiceFlags) { flags.resnapshotInterval = 59 * time.Second }, expectedErrorMessage: "--resnapshotInterval"},
		{name: "resnapshot interval above maximum", mutate: func(flags *cantonDisclosureServiceFlags) { flags.resnapshotInterval = 8 * 24 * time.Hour }, expectedErrorMessage: "--resnapshotInterval"},
		{name: "unknown log level", mutate: func(flags *cantonDisclosureServiceFlags) { flags.logLevel = "verbose" }, expectedErrorMessage: "--logLevel"},
	}
	for _, testCase := range testCases {
		t.Run(testCase.name, func(t *testing.T) {
			flags := validCantonDisclosureServiceFlags()
			testCase.mutate(&flags)
			config, err := parseCantonDisclosureServiceFlags(flags)
			if testCase.expectedErrorMessage != "" {
				require.ErrorContains(t, err, testCase.expectedErrorMessage)
				require.Equal(t, cantonDisclosureServiceConfig{}, config)
				return
			}
			require.NoError(t, err)
			require.Equal(t, listedRegistrar, config.guardianGovernance.String())
			party, isGuardianGovernance := config.readScope.GuardianGovernanceParty()
			require.True(t, isGuardianGovernance)
			require.Equal(t, config.guardianGovernance, party)
			require.Equal(t, testCase.expectOAuth, config.oauthClientCredentials != nil)
			require.Equal(t, flags.indexByteCapBytes, config.indexByteCapBytes)
		})
	}
}

func TestCantonDisclosureServiceCommandFlags(t *testing.T) {
	flags := CantonDisclosureServiceCmd.Flags()
	testCases := []struct {
		flagName     string
		defaultValue string
		required     bool
	}{
		{flagName: "cantonRPC", required: true},
		{flagName: "cantonReadAsParty", required: true},
		{flagName: "listenAddress", required: true},
		{flagName: "indexByteCapBytes", defaultValue: "4294967296"},
		{flagName: "ledgerLagBound", defaultValue: "1m0s"},
		{flagName: "resnapshotInterval", defaultValue: "1h0m0s"},
		{flagName: "statusAddr", defaultValue: "127.0.0.1:6061"},
		{flagName: "unsafePlaintext", defaultValue: "false"},
		{flagName: "cantonAuthTokenURL"},
		{flagName: "cantonAuthClientID"},
		{flagName: "cantonAuthAudience"},
	}
	for _, testCase := range testCases {
		t.Run(testCase.flagName, func(t *testing.T) {
			flag := flags.Lookup(testCase.flagName)
			require.NotNil(t, flag)
			require.Equal(t, testCase.defaultValue, flag.DefValue)
			_, isRequired := flag.Annotations["cobra_annotation_bash_completion_one_required_flag"]
			require.Equal(t, testCase.required, isRequired)
		})
	}
	require.Nil(t, flags.Lookup("cantonAuthClientSecret"))
}

// emptyLedgerReader is a participant with no core contract: the index bootstraps with every singleton missing.
type emptyLedgerReader struct{}

func (emptyLedgerReader) GetLedgerEnd(context.Context) (int64, error) { return 5, nil }

func (emptyLedgerReader) ReadActiveCoreContracts(context.Context, int64, int) (cantonclient.ActiveCoreContracts, error) {
	return cantonclient.ActiveCoreContracts{}, nil
}

func (emptyLedgerReader) SubscribeCoreContractUpdates(ctx context.Context, _ int64, _ chan<- cantonclient.CoreContractLedgerAdvance) (*cantonclient.Subscription, error) {
	streamCtx, cancel := context.WithCancel(ctx)
	subscription := cantonclient.NewSubscription(cancel)
	go func() {
		defer subscription.Close()
		<-streamCtx.Done()
	}()
	return subscription, nil
}

func getProbeStatus(t *testing.T, url string) (int, string) {
	t.Helper()
	response, err := http.Get(url)
	require.NoError(t, err)
	defer response.Body.Close()
	body, err := io.ReadAll(response.Body)
	require.NoError(t, err)
	var probe struct {
		Status string `json:"status"`
	}
	require.NoError(t, json.Unmarshal(body, &probe))
	return response.StatusCode, probe.Status
}

func TestServeCantonDisclosureShutsDownOnCancellation(t *testing.T) {
	config, err := parseCantonDisclosureServiceFlags(validCantonDisclosureServiceFlags())
	require.NoError(t, err)
	listener, err := net.Listen("tcp", "127.0.0.1:0")
	require.NoError(t, err)
	statusListener, err := net.Listen("tcp", "127.0.0.1:0")
	require.NoError(t, err)
	baseURL := "http://" + listener.Addr().String()

	ctx, cancel := context.WithCancel(context.Background())
	defer cancel()
	serveResult := make(chan error, 1)
	go func() {
		serveResult <- serveCantonDisclosure(ctx, config, emptyLedgerReader{}, listener, statusListener, zap.NewNop())
	}()

	require.Eventually(t, func() bool {
		statusCode, status := getProbeStatus(t, baseURL+cantondisclosure.RoutePathReadyz)
		return statusCode == http.StatusServiceUnavailable && status == "index_singleton_missing"
	}, 5*time.Second, 10*time.Millisecond)
	statusCode, status := getProbeStatus(t, baseURL+cantondisclosure.RoutePathHealthz)
	require.Equal(t, http.StatusOK, statusCode)
	require.Equal(t, "ok", status)

	metricsResponse, err := http.Get("http://" + statusListener.Addr().String() + "/metrics")
	require.NoError(t, err)
	metricsBody, err := io.ReadAll(metricsResponse.Body)
	require.NoError(t, err)
	_ = metricsResponse.Body.Close()
	require.Equal(t, http.StatusOK, metricsResponse.StatusCode)
	require.Contains(t, string(metricsBody), "wormhole_canton_disclosure_index_byte_cap_bytes 4.294967296e+09")

	cancel()
	select {
	case err := <-serveResult:
		require.NoError(t, err)
	case <-time.After(cantonDisclosureShutdownTimeout + time.Second):
		t.Fatal("serveCantonDisclosure did not return after cancellation")
	}
	_, err = http.Get(baseURL + cantondisclosure.RoutePathHealthz)
	require.Error(t, err)
}
