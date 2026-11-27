package cantondisclosure

import (
	"net/url"
	"slices"

	"github.com/certusone/wormhole/node/pkg/cantonclient"
)

// Query parameter names of the lookup routes.
const (
	queryParameterConsumer  = "consumer"
	queryParameterNamespace = "namespace"
	queryParameterDigest    = "digest"
)

// maxQueryParameterCount bounds the parameters that a query parse accepts.
const maxQueryParameterCount = 3

// queryRejection is a 400 answer with a fixed public message.
type queryRejection struct {
	publicMessage string
}

func (rejection queryRejection) Error() string { return rejection.publicMessage }

var (
	errQueryMalformed         = queryRejection{publicMessage: "malformed query"}
	errQueryUnknownParameter  = queryRejection{publicMessage: "unknown query parameter"}
	errQueryRepeatedParameter = queryRejection{publicMessage: "repeated query parameter"}
	errQueryMissingConsumer   = queryRejection{publicMessage: "missing query parameter consumer"}
	errQueryMissingNamespace  = queryRejection{publicMessage: "missing query parameter namespace"}
	errQueryMissingDigest     = queryRejection{publicMessage: "missing query parameter digest"}
	errQueryInvalidConsumer   = queryRejection{publicMessage: "invalid consumer: expected <hint>::1220<64 lowercase hex>"}
	errQueryInvalidNamespace  = queryRejection{publicMessage: "invalid namespace: expected 1 to 128 code points of UTF-8"}
	errQueryInvalidDigest     = queryRejection{publicMessage: "invalid digest: expected 64 lowercase hex characters"}
)

// parseExactQueryParameters decodes rawQuery and requires each parameter at most once, from allowedNames only.
func parseExactQueryParameters(rawQuery string, allowedNames []string) (map[string]string, error) {
	if len(allowedNames) > maxQueryParameterCount {
		panic("cantondisclosure: more allowed query parameters than maxQueryParameterCount")
	}
	decodedQuery, err := url.ParseQuery(rawQuery)
	if err != nil {
		return nil, errQueryMalformed
	}
	parameters := make(map[string]string, len(decodedQuery))
	for name, occurrences := range decodedQuery {
		if !slices.Contains(allowedNames, name) {
			return nil, errQueryUnknownParameter
		}
		if len(occurrences) != 1 {
			return nil, errQueryRepeatedParameter
		}
		parameters[name] = occurrences[0]
	}
	if len(parameters) > len(allowedNames) {
		panic("cantondisclosure: parsed more query parameters than allowed")
	}
	return parameters, nil
}

// replayNodeQuery is the parsed query of /v1/replay-node.
type replayNodeQuery struct {
	consumer  cantonclient.PartyID
	namespace cantonclient.ReplayNamespace
	digest    ReplayDigest
}

// claimedRootsNodeQuery is the parsed query of /v1/claimed-roots-node.
type claimedRootsNodeQuery struct {
	consumer  cantonclient.PartyID
	namespace cantonclient.ReplayNamespace
}

func parseConsumerAndNamespace(parameters map[string]string) (cantonclient.PartyID, cantonclient.ReplayNamespace, error) {
	consumerText, hasConsumer := parameters[queryParameterConsumer]
	if !hasConsumer {
		return cantonclient.PartyID{}, cantonclient.ReplayNamespace{}, errQueryMissingConsumer
	}
	namespaceText, hasNamespace := parameters[queryParameterNamespace]
	if !hasNamespace {
		return cantonclient.PartyID{}, cantonclient.ReplayNamespace{}, errQueryMissingNamespace
	}
	consumer, err := cantonclient.ParsePartyID(consumerText)
	if err != nil {
		return cantonclient.PartyID{}, cantonclient.ReplayNamespace{}, errQueryInvalidConsumer
	}
	namespace, err := cantonclient.ParseReplayNamespace(namespaceText)
	if err != nil {
		return cantonclient.PartyID{}, cantonclient.ReplayNamespace{}, errQueryInvalidNamespace
	}
	return consumer, namespace, nil
}

func parseReplayNodeQuery(rawQuery string) (replayNodeQuery, error) {
	parameters, err := parseExactQueryParameters(rawQuery, []string{queryParameterConsumer, queryParameterNamespace, queryParameterDigest})
	if err != nil {
		return replayNodeQuery{}, err
	}
	consumer, namespace, err := parseConsumerAndNamespace(parameters)
	if err != nil {
		return replayNodeQuery{}, err
	}
	digestText, hasDigest := parameters[queryParameterDigest]
	if !hasDigest {
		return replayNodeQuery{}, errQueryMissingDigest
	}
	digest, err := ParseReplayDigest(digestText)
	if err != nil {
		return replayNodeQuery{}, errQueryInvalidDigest
	}
	return replayNodeQuery{consumer: consumer, namespace: namespace, digest: digest}, nil
}

func parseClaimedRootsNodeQuery(rawQuery string) (claimedRootsNodeQuery, error) {
	parameters, err := parseExactQueryParameters(rawQuery, []string{queryParameterConsumer, queryParameterNamespace})
	if err != nil {
		return claimedRootsNodeQuery{}, err
	}
	consumer, namespace, err := parseConsumerAndNamespace(parameters)
	if err != nil {
		return claimedRootsNodeQuery{}, err
	}
	return claimedRootsNodeQuery{consumer: consumer, namespace: namespace}, nil
}

// parseEmptyQuery accepts only an empty query.
func parseEmptyQuery(rawQuery string) error {
	parameters, err := parseExactQueryParameters(rawQuery, nil)
	if err != nil {
		return err
	}
	if len(parameters) != 0 {
		panic("cantondisclosure: empty query parse returned parameters")
	}
	return nil
}
