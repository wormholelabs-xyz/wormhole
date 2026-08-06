package cantonclient

import (
	"context"

	"golang.org/x/oauth2"
	"golang.org/x/oauth2/clientcredentials"
	"google.golang.org/grpc"
	"google.golang.org/grpc/credentials"
	"google.golang.org/grpc/credentials/oauth"
)

// newOAuthPerRPCCredentials builds per-RPC credentials that attach an OAuth2
// client-credentials bearer token to every call. The oauth2 TokenSource caches
// the token and refreshes it before expiry, so no manual refresh loop is
// needed. PerRPCCredentials only cover NEW calls: Canton terminates long-lived
// streams when the token they were opened with expires, which surfaces as a
// stream error and is handled by the watcher's resubscribe-from-last-offset
// logic.
func newOAuthPerRPCCredentials(ctx context.Context, tokenURL, clientID, clientSecret string) credentials.PerRPCCredentials {
	return oauth.TokenSource{TokenSource: newClientCredentialsTokenSource(ctx, tokenURL, clientID, clientSecret)}
}

// newClientCredentialsTokenSource is the testable seam beneath the gRPC
// wrapper: a caching, self-refreshing token source for the client-credentials
// grant.
func newClientCredentialsTokenSource(ctx context.Context, tokenURL, clientID, clientSecret string) oauth2.TokenSource {
	cfg := &clientcredentials.Config{
		TokenURL:     tokenURL,
		ClientID:     clientID,
		ClientSecret: clientSecret,
	}
	return cfg.TokenSource(ctx)
}

// NewOAuthDialOption enables OAuth2 client-credentials authentication (e.g. a
// Keycloak-fronted Ledger API) on the connection. Requires TLS transport;
// dialing plaintext with these credentials fails.
func NewOAuthDialOption(ctx context.Context, tokenURL, clientID, clientSecret string) grpc.DialOption {
	return grpc.WithPerRPCCredentials(newOAuthPerRPCCredentials(ctx, tokenURL, clientID, clientSecret))
}
