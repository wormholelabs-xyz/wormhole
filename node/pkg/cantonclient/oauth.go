package cantonclient

import (
	"context"
	"fmt"
	"net/url"
	"strings"
	"unicode"
	"unicode/utf8"

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

// ParseOAuthAudience returns the zero OAuthAudience for an empty string. It rejects invalid UTF-8, whitespace and
// control characters at any position.
func ParseOAuthAudience(rawAudience string) (OAuthAudience, error) {
	if rawAudience == "" {
		return OAuthAudience{}, nil
	}
	if !utf8.ValidString(rawAudience) {
		return OAuthAudience{}, fmt.Errorf("OAuth audience %q is not valid UTF-8", rawAudience)
	}
	for _, audienceRune := range rawAudience {
		if unicode.IsSpace(audienceRune) {
			return OAuthAudience{}, fmt.Errorf("OAuth audience %q contains whitespace", rawAudience)
		}
		if unicode.IsControl(audienceRune) {
			return OAuthAudience{}, fmt.Errorf("OAuth audience %q contains a control character", rawAudience)
		}
	}
	return OAuthAudience{value: rawAudience}, nil
}

func (audience OAuthAudience) IsSet() bool {
	return audience.value != ""
}

func (audience OAuthAudience) String() string {
	return audience.value
}

// OAuthTokenURL is the absolute https URL of an OAuth2 token endpoint. Each token request sends the client secret to it.
type OAuthTokenURL struct {
	value string
}

// ParseOAuthTokenURL accepts an absolute https URL with a host name and an optional port, path and query.
// SECURITY: an http token URL sends the client secret in plaintext. User information in the URL leaks a credential
// into logs. RFC 6749 section 3.2 forbids a fragment.
func ParseOAuthTokenURL(rawTokenURL string) (OAuthTokenURL, error) {
	if rawTokenURL == "" {
		return OAuthTokenURL{}, fmt.Errorf("OAuth token URL is empty")
	}
	if !utf8.ValidString(rawTokenURL) {
		return OAuthTokenURL{}, fmt.Errorf("OAuth token URL %q is not valid UTF-8", rawTokenURL)
	}
	for _, tokenURLRune := range rawTokenURL {
		if unicode.IsSpace(tokenURLRune) {
			return OAuthTokenURL{}, fmt.Errorf("OAuth token URL %q contains whitespace", rawTokenURL)
		}
		if unicode.IsControl(tokenURLRune) {
			return OAuthTokenURL{}, fmt.Errorf("OAuth token URL %q contains a control character", rawTokenURL)
		}
	}
	parsedTokenURL, err := url.Parse(rawTokenURL)
	if err != nil {
		return OAuthTokenURL{}, fmt.Errorf("OAuth token URL %q is invalid: %w", rawTokenURL, err)
	}
	if parsedTokenURL.Scheme != "https" {
		return OAuthTokenURL{}, fmt.Errorf("OAuth token URL %q must use https, not scheme %q", rawTokenURL, parsedTokenURL.Scheme)
	}
	if parsedTokenURL.Opaque != "" {
		return OAuthTokenURL{}, fmt.Errorf("OAuth token URL %q must be an absolute URL with //host", rawTokenURL)
	}
	if parsedTokenURL.User != nil {
		return OAuthTokenURL{}, fmt.Errorf("OAuth token URL %q contains user information; pass the client id and secret in their own flags", parsedTokenURL.Redacted())
	}
	if parsedTokenURL.Hostname() == "" {
		return OAuthTokenURL{}, fmt.Errorf("OAuth token URL %q has no host name", rawTokenURL)
	}
	if strings.ContainsRune(rawTokenURL, '#') {
		return OAuthTokenURL{}, fmt.Errorf("OAuth token URL %q contains a fragment", rawTokenURL)
	}
	return OAuthTokenURL{value: rawTokenURL}, nil
}

func (tokenURL OAuthTokenURL) String() string {
	return tokenURL.value
}

// OAuthClientCredentials are the parameters of the OAuth2 client-credentials grant for the Ledger API.
type OAuthClientCredentials struct {
	TokenURL     OAuthTokenURL
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
	// SECURITY: ParseOAuthTokenURL is the only constructor; the zero value has no https check.
	if clientCredentials.TokenURL.value == "" {
		panic("cantonclient: OAuth token URL was not made by ParseOAuthTokenURL")
	}
	clientCredentialsConfig := &clientcredentials.Config{
		TokenURL:     clientCredentials.TokenURL.String(),
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
