package cantonclient

import (
	"context"
	"fmt"
	"net/url"
	"strings"

	"golang.org/x/oauth2"
	"golang.org/x/oauth2/clientcredentials"
	"google.golang.org/grpc"
	"google.golang.org/grpc/credentials"
	"google.golang.org/grpc/credentials/oauth"
)

// OAuthAudience is the optional `audience` parameter of the client-credentials token request. The zero value sends
// no audience. Set it to the participant's Ledger API target audience when the identity provider adds `aud` only on
// request (Auth0 style).
type OAuthAudience struct {
	value string
}

// ParseOAuthAudience returns the zero OAuthAudience for an empty string. It rejects a value with leading or
// trailing whitespace, which includes a whitespace-only value.
func ParseOAuthAudience(rawAudience string) (OAuthAudience, error) {
	if rawAudience == "" {
		return OAuthAudience{}, nil
	}
	if strings.TrimSpace(rawAudience) != rawAudience {
		return OAuthAudience{}, fmt.Errorf("OAuth audience %q has leading or trailing whitespace", rawAudience)
	}
	return OAuthAudience{value: rawAudience}, nil
}

func (audience OAuthAudience) IsSet() bool {
	return audience.value != ""
}

func (audience OAuthAudience) String() string {
	return audience.value
}

// OAuthClientCredentials are the parameters of the OAuth2 client-credentials grant for the Ledger API.
type OAuthClientCredentials struct {
	TokenURL     string
	ClientID     string
	ClientSecret string
	Audience     OAuthAudience
}

// newOAuthPerRPCCredentials builds per-RPC credentials that attach an OAuth2
// client-credentials bearer token to every call. The oauth2 TokenSource caches
// the token and refreshes it before expiry, so no manual refresh loop is
// needed. PerRPCCredentials only cover NEW calls: Canton terminates long-lived
// streams when the token they were opened with expires, which surfaces as a
// stream error and is handled by the watcher's resubscribe-from-last-offset
// logic.
func newOAuthPerRPCCredentials(ctx context.Context, clientCredentials OAuthClientCredentials) credentials.PerRPCCredentials {
	return oauth.TokenSource{TokenSource: newClientCredentialsTokenSource(ctx, clientCredentials)}
}

// newClientCredentialsTokenSource is the testable seam beneath the gRPC
// wrapper: a caching, self-refreshing token source for the client-credentials
// grant.
func newClientCredentialsTokenSource(ctx context.Context, clientCredentials OAuthClientCredentials) oauth2.TokenSource {
	clientCredentialsConfig := &clientcredentials.Config{
		TokenURL:     clientCredentials.TokenURL,
		ClientID:     clientCredentials.ClientID,
		ClientSecret: clientCredentials.ClientSecret,
	}
	if clientCredentials.Audience.IsSet() {
		clientCredentialsConfig.EndpointParams = url.Values{"audience": {clientCredentials.Audience.String()}}
	}
	return clientCredentialsConfig.TokenSource(ctx)
}

// NewOAuthDialOption enables OAuth2 client-credentials authentication (e.g. a
// Keycloak-fronted Ledger API) on the connection. Requires TLS transport;
// dialing plaintext with these credentials fails.
func NewOAuthDialOption(ctx context.Context, clientCredentials OAuthClientCredentials) grpc.DialOption {
	return grpc.WithPerRPCCredentials(newOAuthPerRPCCredentials(ctx, clientCredentials))
}
