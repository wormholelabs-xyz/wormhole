package cantonclient

import (
	"context"
	"net/http"
	"net/http/httptest"
	"sync/atomic"
	"testing"
	"time"

	"github.com/stretchr/testify/assert"
	"github.com/stretchr/testify/require"
)

// TestOAuthPerRPCCredentials proves the client-credentials grant is executed
// against the token endpoint and the resulting bearer token is attached as
// per-RPC authorization metadata.
func TestOAuthPerRPCCredentials(t *testing.T) {
	var gotGrant, gotID, gotSecret string
	tokenEndpoint := httptest.NewTLSServer(http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		require.NoError(t, r.ParseForm())
		gotGrant = r.PostFormValue("grant_type")
		var ok bool
		gotID, gotSecret, ok = r.BasicAuth()
		if !ok {
			gotID = r.PostFormValue("client_id")
			gotSecret = r.PostFormValue("client_secret")
		}
		w.Header().Set("Content-Type", "application/json")
		_, _ = w.Write([]byte(`{"access_token":"tok123","token_type":"Bearer","expires_in":300}`))
	}))
	defer tokenEndpoint.Close()

	tokenURL := mustParseOAuthTokenURL(t, tokenEndpoint.URL)
	tokenSource := newClientCredentialsTokenSource(context.Background(), OAuthClientCredentials{TokenURL: tokenURL, ClientID: "cid", ClientSecret: "csecret"}, tokenEndpoint.Client().Transport)
	token, err := tokenSource.Token()
	require.NoError(t, err)
	assert.Equal(t, "tok123", token.AccessToken)
	assert.Equal(t, "Bearer", token.Type())
	assert.Equal(t, "client_credentials", gotGrant)
	assert.Equal(t, "cid", gotID)
	assert.Equal(t, "csecret", gotSecret)

	// The gRPC wrapper must refuse plaintext transports: bearer tokens on an
	// unencrypted channel would leak credentials. (Its GetRequestMetadata can
	// only run inside a real TLS-backed RPC, so it is not exercised here.)
	perRPCCredentials := newOAuthPerRPCCredentials(context.Background(), OAuthClientCredentials{TokenURL: tokenURL, ClientID: "cid", ClientSecret: "csecret"})
	assert.True(t, perRPCCredentials.RequireTransportSecurity())
}

// tokenRequestForm is the form of one token request that the test endpoint received.
type tokenRequestForm struct {
	grantType        string
	clientID         string
	clientSecret     string
	audienceValues   []string
	hasAudienceField bool
}

func newRecordingTokenEndpoint(t *testing.T, recordedForm *tokenRequestForm) *httptest.Server {
	return httptest.NewTLSServer(http.HandlerFunc(func(responseWriter http.ResponseWriter, request *http.Request) {
		assert.NoError(t, request.ParseForm())
		recordedForm.grantType = request.PostFormValue("grant_type")
		var hasBasicAuth bool
		recordedForm.clientID, recordedForm.clientSecret, hasBasicAuth = request.BasicAuth()
		if !hasBasicAuth {
			recordedForm.clientID = request.PostFormValue("client_id")
			recordedForm.clientSecret = request.PostFormValue("client_secret")
		}
		recordedForm.audienceValues, recordedForm.hasAudienceField = request.PostForm["audience"]
		responseWriter.Header().Set("Content-Type", "application/json")
		_, _ = responseWriter.Write([]byte(`{"access_token":"tok123","token_type":"Bearer","expires_in":300}`))
	}))
}

func TestClientCredentialsTokenRequestAudience(t *testing.T) {
	testCases := []struct {
		name                   string
		rawAudience            string
		expectedAudienceValues []string
	}{
		{name: "no audience omits the audience parameter", rawAudience: ""},
		{name: "splice audience", rawAudience: "https://canton.network.global", expectedAudienceValues: []string{"https://canton.network.global"}},
		{name: "participant audience", rawAudience: "https://daml.com/jwt/aud/participant/participant1::1220abcd", expectedAudienceValues: []string{"https://daml.com/jwt/aud/participant/participant1::1220abcd"}},
	}
	for _, testCase := range testCases {
		t.Run(testCase.name, func(t *testing.T) {
			var recordedForm tokenRequestForm
			tokenEndpoint := newRecordingTokenEndpoint(t, &recordedForm)
			defer tokenEndpoint.Close()
			audience, err := ParseOAuthAudience(testCase.rawAudience)
			require.NoError(t, err)

			tokenSource := newClientCredentialsTokenSource(context.Background(), OAuthClientCredentials{
				TokenURL:     mustParseOAuthTokenURL(t, tokenEndpoint.URL),
				ClientID:     "cid",
				ClientSecret: "csecret",
				Audience:     audience,
			}, tokenEndpoint.Client().Transport)
			token, err := tokenSource.Token()
			require.NoError(t, err)

			assert.Equal(t, "tok123", token.AccessToken)
			assert.Equal(t, "client_credentials", recordedForm.grantType)
			assert.Equal(t, "cid", recordedForm.clientID)
			assert.Equal(t, "csecret", recordedForm.clientSecret)
			assert.Equal(t, testCase.expectedAudienceValues != nil, recordedForm.hasAudienceField)
			assert.Equal(t, testCase.expectedAudienceValues, recordedForm.audienceValues)
		})
	}
}

func TestParseOAuthAudience(t *testing.T) {
	testCases := []struct {
		name                 string
		rawAudience          string
		expectedIsSet        bool
		expectedErrorMessage string
	}{
		{name: "empty is no audience", rawAudience: ""},
		{name: "splice audience", rawAudience: "https://canton.network.global", expectedIsSet: true},
		{name: "participant audience", rawAudience: "https://daml.com/jwt/aud/participant/participant1::1220abcd", expectedIsSet: true},
		{name: "auth0 api identifier", rawAudience: "ledger-api", expectedIsSet: true},
		{name: "only spaces", rawAudience: "   ", expectedErrorMessage: "whitespace"},
		{name: "only a tab", rawAudience: "\t", expectedErrorMessage: "whitespace"},
		{name: "leading space", rawAudience: " https://canton.network.global", expectedErrorMessage: "whitespace"},
		{name: "trailing newline", rawAudience: "https://canton.network.global\n", expectedErrorMessage: "whitespace"},
		{name: "inner space", rawAudience: "https://canton.network .global", expectedErrorMessage: "whitespace"},
		{name: "inner tab", rawAudience: "https://canton.network\t.global", expectedErrorMessage: "whitespace"},
		{name: "inner carriage return", rawAudience: "https://canton.network\r.global", expectedErrorMessage: "whitespace"},
		{name: "inner no-break space", rawAudience: "https://canton.network\u00a0.global", expectedErrorMessage: "whitespace"},
		{name: "NUL character", rawAudience: "https://canton.network\x00.global", expectedErrorMessage: "control character"},
		{name: "DEL character", rawAudience: "https://canton.network\x7f.global", expectedErrorMessage: "control character"},
		{name: "escape character", rawAudience: "https://canton.network\x1b.global", expectedErrorMessage: "control character"},
		{name: "invalid UTF-8", rawAudience: "https://canton.network\xff.global", expectedErrorMessage: "UTF-8"},
	}
	for _, testCase := range testCases {
		t.Run(testCase.name, func(t *testing.T) {
			audience, err := ParseOAuthAudience(testCase.rawAudience)
			if testCase.expectedErrorMessage != "" {
				require.ErrorContains(t, err, testCase.expectedErrorMessage)
				require.False(t, audience.IsSet())
				return
			}
			require.NoError(t, err)
			require.Equal(t, testCase.expectedIsSet, audience.IsSet())
			require.Equal(t, testCase.rawAudience, audience.String())
		})
	}
}

func mustParseOAuthTokenURL(t *testing.T, rawTokenURL string) OAuthTokenURL {
	t.Helper()
	tokenURL, err := ParseOAuthTokenURL(rawTokenURL)
	require.NoError(t, err)
	return tokenURL
}

func TestParseOAuthTokenURL(t *testing.T) {
	const keycloakTokenURL = "https://keycloak.example/realms/canton/protocol/openid-connect/token"
	testCases := []struct {
		name                 string
		rawTokenURL          string
		expectedErrorMessage string
	}{
		{name: "keycloak token endpoint", rawTokenURL: keycloakTokenURL},
		{name: "explicit port", rawTokenURL: "https://keycloak.example:8443/realms/canton/protocol/openid-connect/token"},
		{name: "query string", rawTokenURL: "https://idp.example/oauth/token?tenant=canton"},
		{name: "uppercase https scheme", rawTokenURL: "HTTPS://idp.example/oauth/token"},
		{name: "IP address host", rawTokenURL: "https://10.0.0.5:8443/token"},
		{name: "IPv6 literal host", rawTokenURL: "https://[2001:db8::5]:8443/token"},
		{name: "internationalized host", rawTokenURL: "https://kéycloak.example/token"},
		{name: "host with a trailing dot", rawTokenURL: "https://keycloak.example./token"},
		{name: "empty", rawTokenURL: "", expectedErrorMessage: "empty"},
		{name: "http scheme", rawTokenURL: "http://keycloak.example/realms/canton/protocol/openid-connect/token", expectedErrorMessage: "must use https"},
		{name: "uppercase http scheme", rawTokenURL: "HTTP://keycloak.example/token", expectedErrorMessage: "must use https"},
		{name: "ftp scheme", rawTokenURL: "ftp://keycloak.example/token", expectedErrorMessage: "must use https"},
		{name: "no scheme", rawTokenURL: "keycloak.example/token", expectedErrorMessage: "must use https"},
		{name: "scheme-relative", rawTokenURL: "//keycloak.example/token", expectedErrorMessage: "must use https"},
		{name: "opaque https", rawTokenURL: "https:keycloak.example/token", expectedErrorMessage: "absolute"},
		{name: "no host", rawTokenURL: "https://", expectedErrorMessage: "host name"},
		{name: "port without host name", rawTokenURL: "https://:8443/token", expectedErrorMessage: "host name"},
		{name: "user information", rawTokenURL: "https://client:secret@keycloak.example/token", expectedErrorMessage: "user information"},
		{name: "user name only", rawTokenURL: "https://client@keycloak.example/token", expectedErrorMessage: "user information"},
		{name: "fragment", rawTokenURL: keycloakTokenURL + "#section", expectedErrorMessage: "fragment"},
		{name: "empty fragment", rawTokenURL: keycloakTokenURL + "#", expectedErrorMessage: "fragment"},
		{name: "leading space", rawTokenURL: " " + keycloakTokenURL, expectedErrorMessage: "whitespace"},
		{name: "trailing newline", rawTokenURL: keycloakTokenURL + "\n", expectedErrorMessage: "whitespace"},
		{name: "inner space", rawTokenURL: "https://keycloak.example/realms/can ton/token", expectedErrorMessage: "whitespace"},
		{name: "NUL character", rawTokenURL: "https://keycloak.example/\x00token", expectedErrorMessage: "control character"},
		{name: "DEL character", rawTokenURL: "https://keycloak.example/\x7ftoken", expectedErrorMessage: "control character"},
		{name: "invalid UTF-8", rawTokenURL: "https://keycloak.example/\xfftoken", expectedErrorMessage: "UTF-8"},
		{name: "invalid percent escape in host", rawTokenURL: "https://keycloak%zz.example/token", expectedErrorMessage: "does not parse"},
		{name: "unclosed IPv6 literal", rawTokenURL: "https://[::1/token", expectedErrorMessage: "does not parse"},
	}
	for _, testCase := range testCases {
		t.Run(testCase.name, func(t *testing.T) {
			tokenURL, err := ParseOAuthTokenURL(testCase.rawTokenURL)
			if testCase.expectedErrorMessage != "" {
				require.ErrorContains(t, err, testCase.expectedErrorMessage)
				require.NotContains(t, err.Error(), "keycloak.example", "the error must not repeat the token URL")
				require.Equal(t, OAuthTokenURL{}, tokenURL)
				return
			}
			require.NoError(t, err)
			require.Equal(t, testCase.rawTokenURL, tokenURL.String())
		})
	}
}

// TestParseOAuthTokenURLErrorOmitsTheSecret: guardiand logs the startup error.
func TestParseOAuthTokenURLErrorOmitsTheSecret(t *testing.T) {
	const clientSecret = "s3cretValue"
	rawTokenURLs := []string{
		"http://cid:" + clientSecret + "@idp.example/token",
		"https://cid:" + clientSecret + "@idp.example/token",
		"https://cid:" + clientSecret + "@idp.example/token ",
		"https://cid:" + clientSecret + "@idp.example/token#fragment",
		"https:cid:" + clientSecret + "@idp.example/token",
		"https://cid:" + clientSecret + "@[::1/token",
		clientSecret + ":cid@idp.example/token",
	}
	for _, rawTokenURL := range rawTokenURLs {
		_, err := ParseOAuthTokenURL(rawTokenURL)
		require.Error(t, err)
		require.NotContains(t, err.Error(), clientSecret)
	}
}

func TestClientCredentialsTokenSourceRefusesAnUnparsedTokenURL(t *testing.T) {
	require.Panics(t, func() {
		newClientCredentialsTokenSource(context.Background(), OAuthClientCredentials{ClientID: "cid", ClientSecret: "csecret"}, nil)
	})
}

// TestOAuthTokenRequestRefusesRedirects: a 307 or 308 redirect resends the client secret to the redirect target.
func TestOAuthTokenRequestRefusesRedirects(t *testing.T) {
	redirectStatusCodes := []int{
		http.StatusMovedPermanently,
		http.StatusFound,
		http.StatusSeeOther,
		http.StatusTemporaryRedirect,
		http.StatusPermanentRedirect,
	}
	for _, redirectStatusCode := range redirectStatusCodes {
		t.Run(http.StatusText(redirectStatusCode), func(t *testing.T) {
			var redirectTargetRequestCount atomic.Int32
			plaintextRedirectTarget := httptest.NewServer(http.HandlerFunc(func(responseWriter http.ResponseWriter, _ *http.Request) {
				redirectTargetRequestCount.Add(1)
				responseWriter.Header().Set("Content-Type", "application/json")
				_, _ = responseWriter.Write([]byte(`{"access_token":"tok123","token_type":"Bearer","expires_in":300}`))
			}))
			defer plaintextRedirectTarget.Close()
			redirectingTokenEndpoint := httptest.NewTLSServer(http.HandlerFunc(func(responseWriter http.ResponseWriter, request *http.Request) {
				http.Redirect(responseWriter, request, plaintextRedirectTarget.URL+"/token", redirectStatusCode)
			}))
			defer redirectingTokenEndpoint.Close()

			tokenSource := newClientCredentialsTokenSource(context.Background(), OAuthClientCredentials{
				TokenURL:     mustParseOAuthTokenURL(t, redirectingTokenEndpoint.URL),
				ClientID:     "cid",
				ClientSecret: "csecret",
			}, redirectingTokenEndpoint.Client().Transport)
			token, err := tokenSource.Token()

			require.ErrorContains(t, err, "redirect")
			require.Nil(t, token)
			require.Zero(t, redirectTargetRequestCount.Load())
		})
	}
}

func TestOAuthTokenHTTPClientBoundsTheRequest(t *testing.T) {
	tokenHTTPClient := newOAuthTokenHTTPClient(nil)
	require.Equal(t, 30*time.Second, oauthTokenRequestTimeout)
	require.Equal(t, oauthTokenRequestTimeout, tokenHTTPClient.Timeout)
	require.NotNil(t, tokenHTTPClient.CheckRedirect)
	require.Nil(t, tokenHTTPClient.Transport, "nil selects http.DefaultTransport and the system roots")
}

func TestOAuthTokenRequestTimesOut(t *testing.T) {
	releaseSlowTokenEndpoint := make(chan struct{})
	slowTokenEndpoint := httptest.NewTLSServer(http.HandlerFunc(func(_ http.ResponseWriter, request *http.Request) {
		select {
		case <-releaseSlowTokenEndpoint:
		case <-request.Context().Done():
		}
	}))
	defer slowTokenEndpoint.Close()
	defer close(releaseSlowTokenEndpoint)

	tokenHTTPClient := newOAuthTokenHTTPClient(slowTokenEndpoint.Client().Transport)
	tokenHTTPClient.Timeout = 50 * time.Millisecond
	tokenRequest, err := http.NewRequest(http.MethodPost, slowTokenEndpoint.URL, nil)
	require.NoError(t, err)
	tokenResponse, err := tokenHTTPClient.Do(tokenRequest)
	if tokenResponse != nil {
		_ = tokenResponse.Body.Close()
	}
	require.ErrorContains(t, err, "Client.Timeout")
}
