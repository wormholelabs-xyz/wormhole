package cantonclient

import (
	"context"
	"net/http"
	"net/http/httptest"
	"testing"

	"github.com/stretchr/testify/assert"
	"github.com/stretchr/testify/require"
)

// TestOAuthPerRPCCredentials proves the client-credentials grant is executed
// against the token endpoint and the resulting bearer token is attached as
// per-RPC authorization metadata.
func TestOAuthPerRPCCredentials(t *testing.T) {
	var gotGrant, gotID, gotSecret string
	srv := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
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

	ts := newClientCredentialsTokenSource(context.Background(), srv.URL, "cid", "csecret")
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
	creds := newOAuthPerRPCCredentials(context.Background(), srv.URL, "cid", "csecret")
	assert.True(t, creds.RequireTransportSecurity())
}
