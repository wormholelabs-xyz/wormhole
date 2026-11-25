package cantondisclosure

import (
	"encoding/base64"
	"encoding/json"
	"errors"
	"fmt"
	"io"
	"net"
	"net/http"
	"net/http/httptest"
	"net/url"
	"strings"
	"sync"
	"testing"
	"time"

	"github.com/stretchr/testify/require"
	"go.uber.org/zap"
	"go.uber.org/zap/zapcore"
	"go.uber.org/zap/zaptest/observer"

	"github.com/certusone/wormhole/node/pkg/cantonclient"
)

// testInternalErrorDetail must never reach a response body.
const testInternalErrorDetail = "participant 10.1.2.3:5011 refused token for /var/run/secret"

// fakeCoreContractLookups returns fixed answers and records the parsed arguments.
type fakeCoreContractLookups struct {
	mutex            sync.Mutex
	result           CoreContractLookupResult
	lookupErr        error
	readiness        ServiceReadiness
	blockLookups     chan struct{}
	lookupStarted    chan struct{}
	singletonCalls   []cantonclient.CoreContractTemplate
	replayNodeCalls  []replayNodeQuery
	claimedRootCalls []claimedRootsNodeQuery
}

func (lookups *fakeCoreContractLookups) waitIfBlocked() {
	if lookups.lookupStarted != nil {
		lookups.lookupStarted <- struct{}{}
	}
	if lookups.blockLookups != nil {
		<-lookups.blockLookups
	}
}

func (lookups *fakeCoreContractLookups) LookupSingleton(template cantonclient.CoreContractTemplate) (CoreContractLookupResult, error) {
	lookups.waitIfBlocked()
	lookups.mutex.Lock()
	defer lookups.mutex.Unlock()
	lookups.singletonCalls = append(lookups.singletonCalls, template)
	return lookups.result, lookups.lookupErr
}

func (lookups *fakeCoreContractLookups) LookupCoveringReplayNode(consumer cantonclient.PartyID, namespace cantonclient.ReplayNamespace, digest ReplayDigest) (CoreContractLookupResult, error) {
	lookups.waitIfBlocked()
	lookups.mutex.Lock()
	defer lookups.mutex.Unlock()
	lookups.replayNodeCalls = append(lookups.replayNodeCalls, replayNodeQuery{consumer: consumer, namespace: namespace, digest: digest})
	return lookups.result, lookups.lookupErr
}

func (lookups *fakeCoreContractLookups) LookupClaimedRootsNode(consumer cantonclient.PartyID, namespace cantonclient.ReplayNamespace) (CoreContractLookupResult, error) {
	lookups.waitIfBlocked()
	lookups.mutex.Lock()
	defer lookups.mutex.Unlock()
	lookups.claimedRootCalls = append(lookups.claimedRootCalls, claimedRootsNodeQuery{consumer: consumer, namespace: namespace})
	return lookups.result, lookups.lookupErr
}

func (lookups *fakeCoreContractLookups) ServiceReadiness() ServiceReadiness {
	lookups.mutex.Lock()
	defer lookups.mutex.Unlock()
	return lookups.readiness
}

func newTestHTTPHandler(t *testing.T, lookups CoreContractLookups, maxConcurrentLookupRequests int) *DisclosureHTTPHandler {
	t.Helper()
	handler, err := NewDisclosureHTTPHandler(lookups, maxConcurrentLookupRequests, zap.NewNop())
	require.NoError(t, err)
	return handler
}

func serveTestRequest(handler http.Handler, method string, target string) *httptest.ResponseRecorder {
	recorder := httptest.NewRecorder()
	handler.ServeHTTP(recorder, httptest.NewRequest(method, target, nil))
	return recorder
}

func testLookupResult(t *testing.T) CoreContractLookupResult {
	t.Helper()
	contract := newTestReplayNodeContract(t, 7, testConsumer, testNamespace, "a7", testReplayNodeBlobBytes)
	return CoreContractLookupResult{contract: contract, liveAtIndexOffset: 42}
}

func replayNodeTarget(consumerText string, namespaceText string, digestText string) string {
	query := url.Values{}
	query.Set(queryParameterConsumer, consumerText)
	query.Set(queryParameterNamespace, namespaceText)
	query.Set(queryParameterDigest, digestText)
	return RoutePathReplayNode + "?" + query.Encode()
}

func claimedRootsNodeTarget(consumerText string, namespaceText string) string {
	query := url.Values{}
	query.Set(queryParameterConsumer, consumerText)
	query.Set(queryParameterNamespace, namespaceText)
	return RoutePathClaimedRootsNode + "?" + query.Encode()
}

func decodePublicErrorBody(t *testing.T, recorder *httptest.ResponseRecorder) publicErrorBody {
	t.Helper()
	require.Equal(t, responseContentTypeJSON, recorder.Header().Get("Content-Type"))
	decoder := json.NewDecoder(recorder.Body)
	decoder.DisallowUnknownFields()
	var body publicErrorBody
	require.NoError(t, decoder.Decode(&body))
	require.Len(t, body.RequestID, requestIDHexCharacterCount)
	require.Equal(t, body.RequestID, recorder.Header().Get(requestIDHeaderName))
	return body
}

func TestLookupRoutesServeTheDisclosure(t *testing.T) {
	testCases := []struct {
		name                     string
		target                   string
		expectedSingletonCalls   []cantonclient.CoreContractTemplate
		expectedReplayNodeCalls  int
		expectedClaimedRootCalls int
	}{
		{name: "core state", target: RoutePathCoreState, expectedSingletonCalls: []cantonclient.CoreContractTemplate{cantonclient.CoreContractTemplateCoreState}},
		{name: "emitter registry", target: RoutePathEmitterRegistry, expectedSingletonCalls: []cantonclient.CoreContractTemplate{cantonclient.CoreContractTemplateEmitterRegistry}},
		{name: "replay root registry", target: RoutePathReplayRootRegistry, expectedSingletonCalls: []cantonclient.CoreContractTemplate{cantonclient.CoreContractTemplateReplayRootRegistry}},
		{name: "replay node", target: replayNodeTarget(testConsumer, testNamespace, testCoveredDigestHexText), expectedReplayNodeCalls: 1},
		{name: "claimed-roots node", target: claimedRootsNodeTarget(testConsumer, testNamespace), expectedClaimedRootCalls: 1},
	}
	for _, testCase := range testCases {
		t.Run(testCase.name, func(t *testing.T) {
			lookups := &fakeCoreContractLookups{result: testLookupResult(t)}
			recorder := serveTestRequest(newTestHTTPHandler(t, lookups, 4), http.MethodGet, testCase.target)
			require.Equal(t, http.StatusOK, recorder.Code)
			require.Equal(t, responseContentTypeJSON, recorder.Header().Get("Content-Type"))
			require.Equal(t, "no-store", recorder.Header().Get("Cache-Control"))
			require.Equal(t, "nosniff", recorder.Header().Get("X-Content-Type-Options"))

			expectedBody := fmt.Sprintf(`{"templateId":"%s:Wormhole.Core.Replay:ReplayNode","contractId":"%s","createdEventBlob":"%s","synchronizerId":"%s","activeAtOffset":42}`,
				strings.Repeat("3a", 32), testContractIDHex(7), base64.StdEncoding.EncodeToString(testBlob(7, testReplayNodeBlobBytes)), testSynchronizer)
			require.JSONEq(t, expectedBody, recorder.Body.String())

			require.Equal(t, testCase.expectedSingletonCalls, lookups.singletonCalls)
			require.Len(t, lookups.replayNodeCalls, testCase.expectedReplayNodeCalls)
			require.Len(t, lookups.claimedRootCalls, testCase.expectedClaimedRootCalls)
			if testCase.expectedReplayNodeCalls == 1 {
				require.Equal(t, testConsumer, lookups.replayNodeCalls[0].consumer.String())
				require.Equal(t, testNamespace, lookups.replayNodeCalls[0].namespace.String())
				require.Equal(t, testCoveredDigestHexText, lookups.replayNodeCalls[0].digest.String())
			}
			if testCase.expectedClaimedRootCalls == 1 {
				require.Equal(t, testConsumer, lookups.claimedRootCalls[0].consumer.String())
				require.Equal(t, testNamespace, lookups.claimedRootCalls[0].namespace.String())
			}
		})
	}
}

func TestLookupErrorsHaveFixedPublicBodies(t *testing.T) {
	testCases := []struct {
		name               string
		target             string
		lookupErr          error
		expectedStatus     int
		expectedMessage    string
		expectedRetryAfter string
	}{
		{name: "singleton conflict", target: RoutePathCoreState, lookupErr: fmt.Errorf("wrapped: %w", IndexNotReadyError{Readiness: IndexReadinessSingletonConflict}), expectedStatus: http.StatusServiceUnavailable, expectedMessage: publicMessageIndexNotReady},
		{name: "singleton missing", target: RoutePathCoreState, lookupErr: IndexNotReadyError{Readiness: IndexReadinessSingletonMissing}, expectedStatus: http.StatusServiceUnavailable, expectedMessage: publicMessageIndexNotReady},
		{name: "inconsistent index", target: RoutePathCoreState, lookupErr: IndexNotReadyError{Readiness: IndexReadinessInconsistent}, expectedStatus: http.StatusServiceUnavailable, expectedMessage: publicMessageIndexNotReady},
		{name: "bootstrapping", target: RoutePathCoreState, lookupErr: IndexNotReadyError{Readiness: IndexReadinessBootstrapping}, expectedStatus: http.StatusServiceUnavailable, expectedMessage: publicMessageIndexNotReady},
		{name: "read party not hosted", target: RoutePathCoreState, lookupErr: ReadPartyHostingNotReadyError{Status: ServiceReadinessStatusReadPartyNotHosted}, expectedStatus: http.StatusServiceUnavailable, expectedMessage: publicMessageIndexNotReady},
		{name: "replay nodes dropped at the byte cap", target: replayNodeTarget(testConsumer, testNamespace, testCoveredDigestHexText), lookupErr: IndexNotReadyError{Readiness: IndexReadinessByteCapReached}, expectedStatus: http.StatusServiceUnavailable, expectedMessage: publicMessageIndexNotReady},
		{name: "lookups suspended after a resnapshot difference", target: RoutePathCoreState, lookupErr: IndexNotReadyError{Readiness: IndexReadinessResnapshotDifference}, expectedStatus: http.StatusServiceUnavailable, expectedMessage: publicMessageIndexNotReady},
		{name: "no covering node", target: replayNodeTarget(testConsumer, testNamespace, testCoveredDigestHexText), lookupErr: ErrNoCoveringReplayNode, expectedStatus: http.StatusNotFound, expectedMessage: publicMessageNoCoveringNode},
		{name: "two covering nodes", target: replayNodeTarget(testConsumer, testNamespace, testCoveredDigestHexText), lookupErr: MultipleCoveringReplayNodesError{CoveringNodeCount: 2}, expectedStatus: http.StatusInternalServerError, expectedMessage: publicMessageInternalError},
		{name: "reserved namespace", target: claimedRootsNodeTarget(testConsumer, claimedRootsNamespace), lookupErr: ErrReservedReplayNamespace, expectedStatus: http.StatusBadRequest, expectedMessage: publicMessageReservedNamespace},
		{name: "internal error", target: RoutePathEmitterRegistry, lookupErr: errors.New(testInternalErrorDetail), expectedStatus: http.StatusInternalServerError, expectedMessage: publicMessageInternalError},
	}
	for _, testCase := range testCases {
		t.Run(testCase.name, func(t *testing.T) {
			lookups := &fakeCoreContractLookups{lookupErr: fmt.Errorf("%w: %s", testCase.lookupErr, testInternalErrorDetail)}
			recorder := serveTestRequest(newTestHTTPHandler(t, lookups, 4), http.MethodGet, testCase.target)
			require.Equal(t, testCase.expectedStatus, recorder.Code)
			require.NotContains(t, recorder.Body.String(), "10.1.2.3")
			require.NotContains(t, recorder.Body.String(), "cantondisclosure")
			body := decodePublicErrorBody(t, recorder)
			require.Equal(t, testCase.expectedMessage, body.Error)
			require.Equal(t, testCase.expectedRetryAfter, recorder.Header().Get("Retry-After"))
		})
	}
}

func TestLookupResultWithoutAContractIsAnInternalError(t *testing.T) {
	lookups := &fakeCoreContractLookups{}
	recorder := serveTestRequest(newTestHTTPHandler(t, lookups, 4), http.MethodGet, RoutePathCoreState)
	require.Equal(t, http.StatusInternalServerError, recorder.Code)
	require.Equal(t, publicMessageInternalError, decodePublicErrorBody(t, recorder).Error)
}

func TestRequestShapeChecks(t *testing.T) {
	longNamespace := strings.Repeat("n", maxRequestURIBytes)
	testCases := []struct {
		name            string
		method          string
		target          string
		expectedStatus  int
		expectedMessage string
		expectedAllow   string
	}{
		{name: "unknown path", method: http.MethodGet, target: "/v1/emitter", expectedStatus: http.StatusNotFound, expectedMessage: publicMessageNotFound},
		{name: "trailing slash", method: http.MethodGet, target: RoutePathCoreState + "/", expectedStatus: http.StatusNotFound, expectedMessage: publicMessageNotFound},
		{name: "root", method: http.MethodGet, target: "/", expectedStatus: http.StatusNotFound, expectedMessage: publicMessageNotFound},
		{name: "upper case path", method: http.MethodGet, target: "/v1/Core-State", expectedStatus: http.StatusNotFound, expectedMessage: publicMessageNotFound},
		{name: "POST", method: http.MethodPost, target: RoutePathCoreState, expectedStatus: http.StatusMethodNotAllowed, expectedMessage: publicMessageMethodNotAllowed, expectedAllow: http.MethodGet},
		{name: "HEAD", method: http.MethodHead, target: RoutePathReplayNode, expectedStatus: http.StatusMethodNotAllowed, expectedMessage: publicMessageMethodNotAllowed, expectedAllow: http.MethodGet},
		{name: "PUT on a probe", method: http.MethodPut, target: RoutePathReadyz, expectedStatus: http.StatusMethodNotAllowed, expectedMessage: publicMessageMethodNotAllowed, expectedAllow: http.MethodGet},
		{name: "OPTIONS", method: http.MethodOptions, target: RoutePathClaimedRootsNode, expectedStatus: http.StatusMethodNotAllowed, expectedMessage: publicMessageMethodNotAllowed, expectedAllow: http.MethodGet},
		{name: "URI too long", method: http.MethodGet, target: replayNodeTarget(testConsumer, longNamespace, testCoveredDigestHexText), expectedStatus: http.StatusRequestURITooLong, expectedMessage: publicMessageRequestURITooLong},
		{name: "query on a singleton route", method: http.MethodGet, target: RoutePathCoreState + "?consumer=" + url.QueryEscape(testConsumer), expectedStatus: http.StatusBadRequest, expectedMessage: errQueryUnknownParameter.publicMessage},
		{name: "query on healthz", method: http.MethodGet, target: RoutePathHealthz + "?x=1", expectedStatus: http.StatusBadRequest, expectedMessage: errQueryUnknownParameter.publicMessage},
		{name: "query on readyz", method: http.MethodGet, target: RoutePathReadyz + "?verbose", expectedStatus: http.StatusBadRequest, expectedMessage: errQueryUnknownParameter.publicMessage},
		{name: "unknown parameter", method: http.MethodGet, target: replayNodeTarget(testConsumer, testNamespace, testCoveredDigestHexText) + "&limit=1", expectedStatus: http.StatusBadRequest, expectedMessage: errQueryUnknownParameter.publicMessage},
		{name: "repeated parameter", method: http.MethodGet, target: replayNodeTarget(testConsumer, testNamespace, testCoveredDigestHexText) + "&digest=" + testCoveredDigestHexText, expectedStatus: http.StatusBadRequest, expectedMessage: errQueryRepeatedParameter.publicMessage},
		{name: "semicolon separator", method: http.MethodGet, target: RoutePathClaimedRootsNode + "?consumer=" + url.QueryEscape(testConsumer) + ";namespace=" + testNamespace, expectedStatus: http.StatusBadRequest, expectedMessage: errQueryMalformed.publicMessage},
		{name: "bad percent encoding", method: http.MethodGet, target: RoutePathClaimedRootsNode + "?consumer=%zz&namespace=" + testNamespace, expectedStatus: http.StatusBadRequest, expectedMessage: errQueryMalformed.publicMessage},
	}
	for _, testCase := range testCases {
		t.Run(testCase.name, func(t *testing.T) {
			lookups := &fakeCoreContractLookups{result: testLookupResult(t), readiness: ServiceReadiness{BootstrapCompleted: true, StreamConnected: true, LedgerLagBound: time.Minute, IndexReadiness: IndexReadinessReady}}
			recorder := serveTestRequest(newTestHTTPHandler(t, lookups, 4), testCase.method, testCase.target)
			require.Equal(t, testCase.expectedStatus, recorder.Code)
			require.Equal(t, testCase.expectedMessage, decodePublicErrorBody(t, recorder).Error)
			require.Equal(t, testCase.expectedAllow, recorder.Header().Get("Allow"))
			require.Empty(t, lookups.singletonCalls)
			require.Empty(t, lookups.replayNodeCalls)
			require.Empty(t, lookups.claimedRootCalls)
		})
	}
}

func TestRequestWithABodyIsRefused(t *testing.T) {
	lookups := &fakeCoreContractLookups{result: testLookupResult(t)}
	handler := newTestHTTPHandler(t, lookups, 4)
	testCases := []struct {
		name    string
		request func() *http.Request
	}{
		{name: "content length", request: func() *http.Request {
			return httptest.NewRequest(http.MethodGet, RoutePathCoreState, strings.NewReader("{}"))
		}},
		{name: "chunked", request: func() *http.Request {
			request := httptest.NewRequest(http.MethodGet, RoutePathCoreState, io.NopCloser(strings.NewReader("{}")))
			request.ContentLength = -1
			request.TransferEncoding = []string{"chunked"}
			return request
		}},
	}
	for _, testCase := range testCases {
		t.Run(testCase.name, func(t *testing.T) {
			recorder := httptest.NewRecorder()
			handler.ServeHTTP(recorder, testCase.request())
			require.Equal(t, http.StatusBadRequest, recorder.Code)
			require.Equal(t, publicMessageBodyNotAllowed, decodePublicErrorBody(t, recorder).Error)
		})
	}
	require.Empty(t, lookups.singletonCalls)
}

func TestReplayNodeQueryParsing(t *testing.T) {
	maximumNamespace := strings.Repeat("é", 128)
	testCases := []struct {
		name            string
		rawQuery        string
		expectedMessage string
	}{
		{name: "valid", rawQuery: replayNodeQueryText(testConsumer, testNamespace, testCoveredDigestHexText)},
		{name: "namespace of 128 two-byte code points", rawQuery: replayNodeQueryText(testConsumer, maximumNamespace, testCoveredDigestHexText)},
		{name: "namespace of one code point", rawQuery: replayNodeQueryText(testConsumer, "n", testCoveredDigestHexText)},
		{name: "namespace with a space", rawQuery: replayNodeQueryText(testConsumer, "ntt manager", testCoveredDigestHexText)},
		{name: "missing consumer", rawQuery: "namespace=" + testNamespace + "&digest=" + testCoveredDigestHexText, expectedMessage: errQueryMissingConsumer.publicMessage},
		{name: "missing namespace", rawQuery: "consumer=" + url.QueryEscape(testConsumer) + "&digest=" + testCoveredDigestHexText, expectedMessage: errQueryMissingNamespace.publicMessage},
		{name: "missing digest", rawQuery: "consumer=" + url.QueryEscape(testConsumer) + "&namespace=" + testNamespace, expectedMessage: errQueryMissingDigest.publicMessage},
		{name: "empty query", rawQuery: "", expectedMessage: errQueryMissingConsumer.publicMessage},
		{name: "empty consumer", rawQuery: replayNodeQueryText("", testNamespace, testCoveredDigestHexText), expectedMessage: errQueryInvalidConsumer.publicMessage},
		{name: "consumer without delimiter", rawQuery: replayNodeQueryText("consumer", testNamespace, testCoveredDigestHexText), expectedMessage: errQueryInvalidConsumer.publicMessage},
		{name: "consumer with upper case fingerprint", rawQuery: replayNodeQueryText("consumer::"+strings.ToUpper(testFingerprint), testNamespace, testCoveredDigestHexText), expectedMessage: errQueryInvalidConsumer.publicMessage},
		{name: "consumer with a short fingerprint", rawQuery: replayNodeQueryText("consumer::1220ab", testNamespace, testCoveredDigestHexText), expectedMessage: errQueryInvalidConsumer.publicMessage},
		{name: "consumer hint with a space", rawQuery: replayNodeQueryText("con sumer::"+testFingerprint, testNamespace, testCoveredDigestHexText), expectedMessage: errQueryInvalidConsumer.publicMessage},
		{name: "empty namespace", rawQuery: replayNodeQueryText(testConsumer, "", testCoveredDigestHexText), expectedMessage: errQueryInvalidNamespace.publicMessage},
		{name: "namespace of 129 code points", rawQuery: replayNodeQueryText(testConsumer, maximumNamespace+"x", testCoveredDigestHexText), expectedMessage: errQueryInvalidNamespace.publicMessage},
		{name: "namespace with invalid UTF-8", rawQuery: "consumer=" + url.QueryEscape(testConsumer) + "&namespace=%ff&digest=" + testCoveredDigestHexText, expectedMessage: errQueryInvalidNamespace.publicMessage},
		{name: "digest of 63 characters", rawQuery: replayNodeQueryText(testConsumer, testNamespace, testCoveredDigestHexText[:63]), expectedMessage: errQueryInvalidDigest.publicMessage},
		{name: "digest of 65 characters", rawQuery: replayNodeQueryText(testConsumer, testNamespace, testCoveredDigestHexText+"0"), expectedMessage: errQueryInvalidDigest.publicMessage},
		{name: "upper case digest", rawQuery: replayNodeQueryText(testConsumer, testNamespace, strings.ToUpper(testCoveredDigestHexText)), expectedMessage: errQueryInvalidDigest.publicMessage},
		{name: "digest with 0x prefix", rawQuery: replayNodeQueryText(testConsumer, testNamespace, "0x"+testCoveredDigestHexText[2:]), expectedMessage: errQueryInvalidDigest.publicMessage},
		{name: "non-hex digest", rawQuery: replayNodeQueryText(testConsumer, testNamespace, strings.Repeat("g", 64)), expectedMessage: errQueryInvalidDigest.publicMessage},
		{name: "empty digest", rawQuery: replayNodeQueryText(testConsumer, testNamespace, ""), expectedMessage: errQueryInvalidDigest.publicMessage},
		{name: "repeated consumer", rawQuery: replayNodeQueryText(testConsumer, testNamespace, testCoveredDigestHexText) + "&consumer=" + url.QueryEscape(testOtherConsumer), expectedMessage: errQueryRepeatedParameter.publicMessage},
		{name: "repeated namespace with an empty value", rawQuery: replayNodeQueryText(testConsumer, testNamespace, testCoveredDigestHexText) + "&namespace=", expectedMessage: errQueryRepeatedParameter.publicMessage},
		{name: "unknown parameter", rawQuery: replayNodeQueryText(testConsumer, testNamespace, testCoveredDigestHexText) + "&Digest=" + testCoveredDigestHexText, expectedMessage: errQueryUnknownParameter.publicMessage},
		{name: "empty parameter name", rawQuery: replayNodeQueryText(testConsumer, testNamespace, testCoveredDigestHexText) + "&=1", expectedMessage: errQueryUnknownParameter.publicMessage},
	}
	for _, testCase := range testCases {
		t.Run(testCase.name, func(t *testing.T) {
			query, err := parseReplayNodeQuery(testCase.rawQuery)
			if testCase.expectedMessage != "" {
				require.EqualError(t, err, testCase.expectedMessage)
				require.Equal(t, replayNodeQuery{}, query)
				return
			}
			require.NoError(t, err)
			values, parseErr := url.ParseQuery(testCase.rawQuery)
			require.NoError(t, parseErr)
			require.Equal(t, values.Get(queryParameterConsumer), query.consumer.String())
			require.Equal(t, values.Get(queryParameterNamespace), query.namespace.String())
			require.Equal(t, values.Get(queryParameterDigest), query.digest.String())
		})
	}
}

func replayNodeQueryText(consumerText string, namespaceText string, digestText string) string {
	return strings.TrimPrefix(replayNodeTarget(consumerText, namespaceText, digestText), RoutePathReplayNode+"?")
}

func TestClaimedRootsNodeQueryParsing(t *testing.T) {
	testCases := []struct {
		name            string
		rawQuery        string
		expectedMessage string
	}{
		{name: "valid", rawQuery: "consumer=" + url.QueryEscape(testConsumer) + "&namespace=" + testNamespace},
		{name: "reserved namespace parses", rawQuery: "consumer=" + url.QueryEscape(testConsumer) + "&namespace=" + url.QueryEscape(claimedRootsNamespace)},
		{name: "digest is unknown here", rawQuery: "consumer=" + url.QueryEscape(testConsumer) + "&namespace=" + testNamespace + "&digest=" + testCoveredDigestHexText, expectedMessage: errQueryUnknownParameter.publicMessage},
		{name: "missing consumer", rawQuery: "namespace=" + testNamespace, expectedMessage: errQueryMissingConsumer.publicMessage},
		{name: "missing namespace", rawQuery: "consumer=" + url.QueryEscape(testConsumer), expectedMessage: errQueryMissingNamespace.publicMessage},
		{name: "invalid consumer", rawQuery: "consumer=x&namespace=" + testNamespace, expectedMessage: errQueryInvalidConsumer.publicMessage},
		{name: "invalid namespace", rawQuery: "consumer=" + url.QueryEscape(testConsumer) + "&namespace=" + strings.Repeat("n", 129), expectedMessage: errQueryInvalidNamespace.publicMessage},
		{name: "repeated namespace", rawQuery: "consumer=" + url.QueryEscape(testConsumer) + "&namespace=a&namespace=b", expectedMessage: errQueryRepeatedParameter.publicMessage},
	}
	for _, testCase := range testCases {
		t.Run(testCase.name, func(t *testing.T) {
			query, err := parseClaimedRootsNodeQuery(testCase.rawQuery)
			if testCase.expectedMessage != "" {
				require.EqualError(t, err, testCase.expectedMessage)
				require.Equal(t, claimedRootsNodeQuery{}, query)
				return
			}
			require.NoError(t, err)
			require.Equal(t, testConsumer, query.consumer.String())
		})
	}
}

func TestReadyzStates(t *testing.T) {
	readyReadiness := ServiceReadiness{BootstrapCompleted: true, ReadPartyHosted: true, StreamConnected: true, LedgerLag: time.Second, LedgerLagBound: time.Minute, IndexReadiness: IndexReadinessReady}
	testCases := []struct {
		name           string
		mutate         func(readiness *ServiceReadiness)
		expectedStatus int
		expectedBody   string
	}{
		{name: "ready", mutate: func(*ServiceReadiness) {}, expectedStatus: http.StatusOK, expectedBody: `{"status":"ready"}`},
		{name: "ready near the byte cap", mutate: func(readiness *ServiceReadiness) { readiness.IndexReadiness = IndexReadinessReadyNearByteCap }, expectedStatus: http.StatusOK, expectedBody: `{"status":"ready"}`},
		{name: "byte cap reached", mutate: func(readiness *ServiceReadiness) { readiness.IndexReadiness = IndexReadinessByteCapReached }, expectedStatus: http.StatusOK, expectedBody: `{"status":"ready_byte_cap_reached"}`},
		{name: "bootstrapping", mutate: func(readiness *ServiceReadiness) { *readiness = ServiceReadiness{} }, expectedStatus: http.StatusServiceUnavailable, expectedBody: `{"status":"bootstrapping"}`},
		{name: "stream disconnected", mutate: func(readiness *ServiceReadiness) { readiness.StreamConnected = false }, expectedStatus: http.StatusServiceUnavailable, expectedBody: `{"status":"stream_disconnected"}`},
		{name: "lag at the bound", mutate: func(readiness *ServiceReadiness) { readiness.LedgerLag = time.Minute }, expectedStatus: http.StatusOK, expectedBody: `{"status":"ready"}`},
		{name: "lag above the bound", mutate: func(readiness *ServiceReadiness) { readiness.LedgerLag = time.Minute + 1 }, expectedStatus: http.StatusServiceUnavailable, expectedBody: `{"status":"ledger_lag_above_bound"}`},
		{name: "singleton conflict", mutate: func(readiness *ServiceReadiness) { readiness.IndexReadiness = IndexReadinessSingletonConflict }, expectedStatus: http.StatusServiceUnavailable, expectedBody: `{"status":"index_singleton_conflict"}`},
		{name: "singleton missing", mutate: func(readiness *ServiceReadiness) { readiness.IndexReadiness = IndexReadinessSingletonMissing }, expectedStatus: http.StatusServiceUnavailable, expectedBody: `{"status":"index_singleton_missing"}`},
		{name: "inconsistent", mutate: func(readiness *ServiceReadiness) { readiness.IndexReadiness = IndexReadinessInconsistent }, expectedStatus: http.StatusServiceUnavailable, expectedBody: `{"status":"index_inconsistent"}`},
		{name: "resnapshot difference", mutate: func(readiness *ServiceReadiness) { readiness.IndexReadiness = IndexReadinessResnapshotDifference }, expectedStatus: http.StatusServiceUnavailable, expectedBody: `{"status":"index_resnapshot_difference"}`},
		{name: "read party not hosted", mutate: func(readiness *ServiceReadiness) { readiness.ReadPartyHosted = false }, expectedStatus: http.StatusServiceUnavailable, expectedBody: `{"status":"read_party_not_hosted"}`},
		{name: "hosting read at the bound", mutate: func(readiness *ServiceReadiness) { readiness.ReadPartyHostingReadAge = time.Minute }, expectedStatus: http.StatusOK, expectedBody: `{"status":"ready"}`},
		{name: "hosting read above the bound", mutate: func(readiness *ServiceReadiness) { readiness.ReadPartyHostingReadAge = time.Minute + 1 }, expectedStatus: http.StatusServiceUnavailable, expectedBody: `{"status":"read_party_hosting_unknown"}`},
		{name: "read party rehosted", mutate: func(readiness *ServiceReadiness) { readiness.ReadPartyRehostedBeforeServedSnapshot = true }, expectedStatus: http.StatusServiceUnavailable, expectedBody: `{"status":"read_party_rehosted"}`},
	}
	for _, testCase := range testCases {
		t.Run(testCase.name, func(t *testing.T) {
			readiness := readyReadiness
			testCase.mutate(&readiness)
			lookups := &fakeCoreContractLookups{readiness: readiness}
			recorder := serveTestRequest(newTestHTTPHandler(t, lookups, 4), http.MethodGet, RoutePathReadyz)
			require.Equal(t, testCase.expectedStatus, recorder.Code)
			require.JSONEq(t, testCase.expectedBody, recorder.Body.String())
		})
	}
}

func TestHealthzIsAlwaysOK(t *testing.T) {
	recorder := serveTestRequest(newTestHTTPHandler(t, &fakeCoreContractLookups{}, 4), http.MethodGet, RoutePathHealthz)
	require.Equal(t, http.StatusOK, recorder.Code)
	require.JSONEq(t, `{"status":"ok"}`, recorder.Body.String())
}

func TestConcurrentLookupLimitAnswers503AndKeepsProbes(t *testing.T) {
	lookups := &fakeCoreContractLookups{
		result:        testLookupResult(t),
		readiness:     ServiceReadiness{BootstrapCompleted: true, ReadPartyHosted: true, StreamConnected: true, LedgerLagBound: time.Minute, IndexReadiness: IndexReadinessReady},
		blockLookups:  make(chan struct{}),
		lookupStarted: make(chan struct{}, 1),
	}
	handler := newTestHTTPHandler(t, lookups, 1)
	firstDone := make(chan *httptest.ResponseRecorder, 1)
	go func() { firstDone <- serveTestRequest(handler, http.MethodGet, RoutePathCoreState) }()
	<-lookups.lookupStarted

	busy := serveTestRequest(handler, http.MethodGet, RoutePathEmitterRegistry)
	require.Equal(t, http.StatusServiceUnavailable, busy.Code)
	require.Equal(t, "1", busy.Header().Get("Retry-After"))
	require.Equal(t, publicMessageServiceBusy, decodePublicErrorBody(t, busy).Error)

	require.Equal(t, http.StatusOK, serveTestRequest(handler, http.MethodGet, RoutePathReadyz).Code)
	require.Equal(t, http.StatusOK, serveTestRequest(handler, http.MethodGet, RoutePathHealthz).Code)

	close(lookups.blockLookups)
	require.Equal(t, http.StatusOK, (<-firstDone).Code)
	lookups.blockLookups = nil
	lookups.lookupStarted = nil
	require.Equal(t, http.StatusOK, serveTestRequest(handler, http.MethodGet, RoutePathEmitterRegistry).Code)
}

func TestNewDisclosureHTTPHandlerRejects(t *testing.T) {
	testCases := []struct {
		name         string
		lookups      CoreContractLookups
		requestLimit int
		logger       *zap.Logger
	}{
		{name: "missing lookups", requestLimit: 1, logger: zap.NewNop()},
		{name: "missing logger", lookups: &fakeCoreContractLookups{}, requestLimit: 1},
		{name: "zero request limit", lookups: &fakeCoreContractLookups{}, requestLimit: 0, logger: zap.NewNop()},
		{name: "request limit above maximum", lookups: &fakeCoreContractLookups{}, requestLimit: maxConcurrentLookupRequestsLimit + 1, logger: zap.NewNop()},
	}
	for _, testCase := range testCases {
		t.Run(testCase.name, func(t *testing.T) {
			handler, err := NewDisclosureHTTPHandler(testCase.lookups, testCase.requestLimit, testCase.logger)
			require.Error(t, err)
			require.Nil(t, handler)
		})
	}
}

func TestDisclosureHTTPServerLimits(t *testing.T) {
	handler := newTestHTTPHandler(t, &fakeCoreContractLookups{}, 4)
	server := NewDisclosureHTTPServer("127.0.0.1:0", handler)
	require.Equal(t, httpReadHeaderTimeout, server.ReadHeaderTimeout)
	require.Equal(t, httpReadTimeout, server.ReadTimeout)
	require.Equal(t, httpWriteTimeout, server.WriteTimeout)
	require.Equal(t, httpIdleTimeout, server.IdleTimeout)
	require.Equal(t, httpMaxHeaderBytes, server.MaxHeaderBytes)

	listener, err := net.Listen("tcp", "127.0.0.1:0")
	require.NoError(t, err)
	go func() { _ = server.Serve(listener) }()
	t.Cleanup(func() { _ = server.Close() })
	baseURL := "http://" + listener.Addr().String()

	healthResponse, err := http.Get(baseURL + RoutePathHealthz)
	require.NoError(t, err)
	_ = healthResponse.Body.Close()
	require.Equal(t, http.StatusOK, healthResponse.StatusCode)

	request, err := http.NewRequest(http.MethodGet, baseURL+RoutePathHealthz, nil)
	require.NoError(t, err)
	request.Header.Set("X-Padding", strings.Repeat("p", 2*httpMaxHeaderBytes))
	oversizedResponse, err := http.DefaultClient.Do(request)
	require.NoError(t, err)
	_ = oversizedResponse.Body.Close()
	require.Equal(t, http.StatusRequestHeaderFieldsTooLarge, oversizedResponse.StatusCode)
}

func TestHandlerWithAServedIndexAtTheByteCap(t *testing.T) {
	served, clock := newTestServedIndex(t)
	const byteCapBytes = int64(20_000)
	oversizedNode := newTestReplayNodeContract(t, 1, testConsumer, testNamespace, "", int(byteCapBytes))
	swapTestIndex(served, newBootstrappedTestIndex(t, byteCapBytes, oversizedNode), clock)
	served.setStreamConnected(true)
	handler := newTestHTTPHandler(t, served, 4)

	testCases := []struct {
		name               string
		target             string
		expectedStatus     int
		expectedRetryAfter string
	}{
		{name: "core state", target: RoutePathCoreState, expectedStatus: http.StatusOK},
		{name: "emitter registry", target: RoutePathEmitterRegistry, expectedStatus: http.StatusOK},
		{name: "replay root registry", target: RoutePathReplayRootRegistry, expectedStatus: http.StatusOK},
		{name: "replay node", target: replayNodeTarget(testConsumer, testNamespace, testCoveredDigestHexText), expectedStatus: http.StatusServiceUnavailable},
		{name: "claimed-roots node", target: claimedRootsNodeTarget(testConsumer, testNamespace), expectedStatus: http.StatusServiceUnavailable},
	}
	for _, testCase := range testCases {
		t.Run(testCase.name, func(t *testing.T) {
			recorder := serveTestRequest(handler, http.MethodGet, testCase.target)
			require.Equal(t, testCase.expectedStatus, recorder.Code)
			require.Equal(t, testCase.expectedRetryAfter, recorder.Header().Get("Retry-After"))
			if testCase.expectedStatus == http.StatusServiceUnavailable {
				require.Equal(t, publicMessageIndexNotReady, decodePublicErrorBody(t, recorder).Error)
			}
		})
	}
	readyz := serveTestRequest(handler, http.MethodGet, RoutePathReadyz)
	require.Equal(t, http.StatusOK, readyz.Code)
	require.JSONEq(t, `{"status":"ready_byte_cap_reached"}`, readyz.Body.String())
}

func TestUnavailableLogsAreRateLimitedPerCause(t *testing.T) {
	observedCore, observedLogs := observer.New(zapcore.WarnLevel)
	lookups := &fakeCoreContractLookups{lookupErr: IndexNotReadyError{Readiness: IndexReadinessSingletonMissing}}
	handler, err := NewDisclosureHTTPHandler(lookups, 4, zap.New(observedCore))
	require.NoError(t, err)
	clock := &testClock{current: time.Date(2026, 11, 24, 12, 0, 0, 0, time.UTC)}
	handler.unavailableLogs.now = clock.now

	for requestIndex := 0; requestIndex < 5; requestIndex++ {
		require.Equal(t, http.StatusServiceUnavailable, serveTestRequest(handler, http.MethodGet, RoutePathCoreState).Code)
	}
	require.Equal(t, 1, observedLogs.Len())
	require.Equal(t, uint64(0), observedLogs.All()[0].ContextMap()["suppressedUnavailableLogs"])
	require.Equal(t, "singleton_missing", observedLogs.All()[0].ContextMap()["cause"])

	lookups.lookupErr = ReadPartyHostingNotReadyError{Status: ServiceReadinessStatusReadPartyNotHosted}
	require.Equal(t, http.StatusServiceUnavailable, serveTestRequest(handler, http.MethodGet, RoutePathCoreState).Code)
	require.Equal(t, 2, observedLogs.Len(), "another cause has its own interval")
	require.Equal(t, "read_party_not_hosted", observedLogs.All()[1].ContextMap()["cause"])

	lookups.lookupErr = IndexNotReadyError{Readiness: IndexReadinessSingletonMissing}
	clock.current = clock.current.Add(unavailableLogInterval - time.Nanosecond)
	require.Equal(t, http.StatusServiceUnavailable, serveTestRequest(handler, http.MethodGet, RoutePathCoreState).Code)
	require.Equal(t, 2, observedLogs.Len())

	clock.current = clock.current.Add(time.Nanosecond)
	require.Equal(t, http.StatusServiceUnavailable, serveTestRequest(handler, http.MethodGet, RoutePathCoreState).Code)
	require.Equal(t, 3, observedLogs.Len())
	require.Equal(t, uint64(5), observedLogs.All()[2].ContextMap()["suppressedUnavailableLogs"])

	lookups.lookupErr = errors.New(testInternalErrorDetail)
	for requestIndex := 0; requestIndex < 3; requestIndex++ {
		require.Equal(t, http.StatusInternalServerError, serveTestRequest(handler, http.MethodGet, RoutePathCoreState).Code)
	}
	require.Equal(t, 6, observedLogs.Len(), "internal errors are logged every time")
}

func TestSuppressedUnavailableLogCountsAreFlushed(t *testing.T) {
	observedCore, observedLogs := observer.New(zapcore.WarnLevel)
	lookups := &fakeCoreContractLookups{lookupErr: IndexNotReadyError{Readiness: IndexReadinessInconsistent}}
	handler, err := NewDisclosureHTTPHandler(lookups, 4, zap.New(observedCore))
	require.NoError(t, err)
	clock := &testClock{current: time.Date(2026, 11, 24, 12, 0, 0, 0, time.UTC)}
	handler.unavailableLogs.now = clock.now
	for requestIndex := 0; requestIndex < 4; requestIndex++ {
		serveTestRequest(handler, http.MethodGet, RoutePathCoreState)
	}
	require.Equal(t, 1, observedLogs.Len())

	serveTestRequest(handler, http.MethodGet, RoutePathHealthz)
	require.Equal(t, 1, observedLogs.Len(), "a count within its interval waits")
	clock.current = clock.current.Add(unavailableLogInterval)
	serveTestRequest(handler, http.MethodGet, RoutePathHealthz)
	require.Equal(t, 2, observedLogs.Len(), "any request after the interval logs the last suppressed count")
	flushed := observedLogs.All()[1]
	require.Equal(t, "disclosure 503 logs suppressed", flushed.Message)
	require.Equal(t, uint64(3), flushed.ContextMap()["suppressedUnavailableLogs"])
	require.Equal(t, "inconsistent", flushed.ContextMap()["cause"])
	serveTestRequest(handler, http.MethodGet, RoutePathHealthz)
	require.Equal(t, 2, observedLogs.Len(), "a flushed count logs once")

	serveTestRequest(handler, http.MethodGet, RoutePathCoreState)
	require.Equal(t, 2, observedLogs.Len(), "the flush starts a new interval")
	handler.FlushSuppressedUnavailableLogs()
	require.Equal(t, 3, observedLogs.Len(), "the shutdown flush logs counts within their interval")
	require.Equal(t, uint64(1), observedLogs.All()[2].ContextMap()["suppressedUnavailableLogs"])
	handler.FlushSuppressedUnavailableLogs()
	require.Equal(t, 3, observedLogs.Len())
}

func TestUnavailableLogCausesAreBounded(t *testing.T) {
	limiter := newUnavailableLogLimiter(time.Now)
	for causeIndex := 0; causeIndex < 3*maxUnavailableLogCauses; causeIndex++ {
		limiter.admitLog(fmt.Sprintf("cause_%d", causeIndex))
	}
	require.Len(t, limiter.stateByCause, maxUnavailableLogCauses+1)
	require.Contains(t, limiter.stateByCause, unavailableCauseOther)
	require.Equal(t, uint64(2*maxUnavailableLogCauses-1), limiter.stateByCause[unavailableCauseOther].suppressedCount)
}
