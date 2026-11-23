package cantonclient

import (
	"context"
	"net/http"
	"net/http/httptest"
	"testing"

	"github.com/stretchr/testify/assert"
	"github.com/stretchr/testify/require"
	"golang.org/x/oauth2"
)

// TestOAuthPerRPCCredentials proves the client-credentials grant is executed
// against the token endpoint and the resulting bearer token is attached as
// per-RPC authorization metadata.
func TestOAuthPerRPCCredentials(t *testing.T) {
	var gotGrant, gotID, gotSecret string
	srv := httptest.NewTLSServer(http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
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
	defer srv.Close()

	tokenURL := mustParseOAuthTokenURL(t, srv.URL)
	ts := newClientCredentialsTokenSource(contextTrustingTokenEndpoint(srv), OAuthClientCredentials{TokenURL: tokenURL, ClientID: "cid", ClientSecret: "csecret"})
	tok, err := ts.Token()
	require.NoError(t, err)
	assert.Equal(t, "tok123", tok.AccessToken)
	assert.Equal(t, "Bearer", tok.Type())
	assert.Equal(t, "client_credentials", gotGrant)
	assert.Equal(t, "cid", gotID)
	assert.Equal(t, "csecret", gotSecret)

	// The gRPC wrapper must refuse plaintext transports: bearer tokens on an
	// unencrypted channel would leak credentials. (Its GetRequestMetadata can
	// only run inside a real TLS-backed RPC, so it is not exercised here.)
	creds := newOAuthPerRPCCredentials(context.Background(), OAuthClientCredentials{TokenURL: tokenURL, ClientID: "cid", ClientSecret: "csecret"})
	assert.True(t, creds.RequireTransportSecurity())
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

			tokenSource := newClientCredentialsTokenSource(contextTrustingTokenEndpoint(tokenEndpoint), OAuthClientCredentials{
				TokenURL:     mustParseOAuthTokenURL(t, tokenEndpoint.URL),
				ClientID:     "cid",
				ClientSecret: "csecret",
				Audience:     audience,
			})
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

// contextTrustingTokenEndpoint carries the HTTP client of an httptest TLS server, which trusts its certificate.
func contextTrustingTokenEndpoint(tokenEndpoint *httptest.Server) context.Context {
	return context.WithValue(context.Background(), oauth2.HTTPClient, tokenEndpoint.Client())
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
		{name: "invalid percent escape in host", rawTokenURL: "https://keycloak%zz.example/token", expectedErrorMessage: "invalid"},
	}
	for _, testCase := range testCases {
		t.Run(testCase.name, func(t *testing.T) {
			tokenURL, err := ParseOAuthTokenURL(testCase.rawTokenURL)
			if testCase.expectedErrorMessage != "" {
				require.ErrorContains(t, err, testCase.expectedErrorMessage)
				require.Equal(t, OAuthTokenURL{}, tokenURL)
				return
			}
			require.NoError(t, err)
			require.Equal(t, testCase.rawTokenURL, tokenURL.String())
		})
	}
}

func TestClientCredentialsTokenSourceRefusesAnUnparsedTokenURL(t *testing.T) {
	require.Panics(t, func() {
		newClientCredentialsTokenSource(context.Background(), OAuthClientCredentials{ClientID: "cid", ClientSecret: "csecret"})
	})
}
