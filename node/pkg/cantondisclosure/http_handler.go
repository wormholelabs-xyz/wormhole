package cantondisclosure

import (
	"crypto/rand"
	"encoding/base64"
	"encoding/hex"
	"encoding/json"
	"errors"
	"fmt"
	"net/http"
	"slices"
	"strconv"
	"strings"
	"sync"
	"time"

	"go.uber.org/zap"

	"github.com/certusone/wormhole/node/pkg/cantonclient"
)

// CoreContractLookups is the read surface of the HTTP handler. ServedIndex implements it.
type CoreContractLookups interface {
	LookupSingleton(template cantonclient.CoreContractTemplate) (CoreContractLookupResult, error)
	LookupCoveringReplayNode(consumer cantonclient.PartyID, namespace cantonclient.ReplayNamespace, digest ReplayDigest) (CoreContractLookupResult, error)
	LookupClaimedRootsNode(consumer cantonclient.PartyID, namespace cantonclient.ReplayNamespace) (CoreContractLookupResult, error)
	ServiceReadiness() ServiceReadiness
}

// Routes of the disclosure service.
const (
	RoutePathCoreState          = "/v1/core-state"
	RoutePathEmitterRegistry    = "/v1/emitter-registry"
	RoutePathReplayRootRegistry = "/v1/replay-root-registry"
	RoutePathReplayNode         = "/v1/replay-node"
	RoutePathClaimedRootsNode   = "/v1/claimed-roots-node"
	RoutePathHealthz            = "/v1/healthz"
	RoutePathReadyz             = "/v1/readyz"
)

// HTTP limits.
const (
	// maxRequestURIBytes fits the longest valid query: a 255-byte party, a 512-byte namespace and a digest, each percent-encoded.
	maxRequestURIBytes = 4096
	// DefaultMaxConcurrentLookupRequests bounds the lookups in progress; probes are outside this bound.
	DefaultMaxConcurrentLookupRequests = 256
	maxConcurrentLookupRequestsLimit   = 1 << 16
	httpReadHeaderTimeout              = 5 * time.Second
	httpReadTimeout                    = 10 * time.Second
	httpWriteTimeout                   = 10 * time.Second
	httpIdleTimeout                    = 60 * time.Second
	httpMaxHeaderBytes                 = 16 << 10
	// httpRetryAfterSeconds is the Retry-After of the busy 503: a lookup slot frees within one write timeout.
	// An index or hosting 503 has no Retry-After, because a rebuild, a ledger change or an operator action clears it.
	httpRetryAfterSeconds = 1
	// unavailableLogInterval bounds the 503 warn logs to one per interval and cause.
	unavailableLogInterval = time.Second
	// maxUnavailableLogCauses bounds the causes that the limiter tracks; the causes come from fixed enumerations.
	maxUnavailableLogCauses = 32
	// unavailableCauseServiceBusy and unavailableCauseOther are the causes without a readiness.
	unavailableCauseServiceBusy = "service_busy"
	unavailableCauseOther       = "other"
)

// Fixed public messages of the error bodies.
const (
	publicMessageNotFound          = "not found"
	publicMessageMethodNotAllowed  = "method not allowed"
	publicMessageRequestURITooLong = "request URI too long"
	publicMessageBodyNotAllowed    = "request body not allowed"
	publicMessageServiceBusy       = "service busy, retry"
	publicMessageIndexNotReady     = "index not ready, retry"
	publicMessageNoCoveringNode    = "no live replay node covers the digest"
	publicMessageReservedNamespace = "namespace is reserved for the claimed-roots tree"
	publicMessageInternalError     = "internal error"
	healthzStatusOK                = "ok"
	responseContentTypeJSON        = "application/json"
	requestIDHeaderName            = "X-Request-Id"
	requestIDRandomByteCount       = 16
	requestIDHexCharacterCount     = 2 * requestIDRandomByteCount
)

// coreContractDisclosureResponse is the body of each lookup route. The field names follow the NTT disclosure service.
type coreContractDisclosureResponse struct {
	TemplateID       string `json:"templateId"`
	ContractID       string `json:"contractId"`
	CreatedEventBlob string `json:"createdEventBlob"`
	SynchronizerID   string `json:"synchronizerId"`
	ActiveAtOffset   int64  `json:"activeAtOffset"`
}

// publicErrorBody is the only error shape that a client sees.
type publicErrorBody struct {
	Error     string `json:"error"`
	RequestID string `json:"requestId"`
}

// probeResponse is the body of /v1/healthz and /v1/readyz.
type probeResponse struct {
	Status string `json:"status"`
}

type disclosureRoute struct {
	isProbe bool
	serve   func(handler *DisclosureHTTPHandler, writer http.ResponseWriter, request *http.Request)
}

var disclosureRoutes = map[string]disclosureRoute{
	RoutePathCoreState:          {serve: singletonRouteOf(cantonclient.CoreContractTemplateCoreState)},
	RoutePathEmitterRegistry:    {serve: singletonRouteOf(cantonclient.CoreContractTemplateEmitterRegistry)},
	RoutePathReplayRootRegistry: {serve: singletonRouteOf(cantonclient.CoreContractTemplateReplayRootRegistry)},
	RoutePathReplayNode:         {serve: (*DisclosureHTTPHandler).serveReplayNode},
	RoutePathClaimedRootsNode:   {serve: (*DisclosureHTTPHandler).serveClaimedRootsNode},
	RoutePathHealthz:            {isProbe: true, serve: (*DisclosureHTTPHandler).serveHealthz},
	RoutePathReadyz:             {isProbe: true, serve: (*DisclosureHTTPHandler).serveReadyz},
}

// DisclosureHTTPHandler serves the disclosure routes over GET only.
//
// SECURITY: the service is untrusted for safety. Error bodies carry fixed messages; internal errors go to the log only.
type DisclosureHTTPHandler struct {
	lookups            CoreContractLookups
	lookupRequestSlots chan struct{}
	logger             *zap.Logger
	unavailableLogs    *unavailableLogLimiter
}

// unavailableLogLimiter admits one 503 warn log per unavailableLogInterval and cause, and counts the others.
type unavailableLogLimiter struct {
	now          func() time.Time
	mutex        sync.Mutex
	stateByCause map[string]*unavailableCauseLogState
}

type unavailableCauseLogState struct {
	lastAdmittedAt  time.Time
	suppressedCount uint64
}

// suppressedUnavailableLogCount is the count of one cause suppressed since its last admitted log.
type suppressedUnavailableLogCount struct {
	cause           string
	suppressedCount uint64
}

func newUnavailableLogLimiter(now func() time.Time) *unavailableLogLimiter {
	return &unavailableLogLimiter{now: now, stateByCause: make(map[string]*unavailableCauseLogState, maxUnavailableLogCauses+1)}
}

// admitLog returns true at most once per unavailableLogInterval for cause, with the count suppressed since its last
// admitted log. Above maxUnavailableLogCauses tracked causes, a new cause counts as unavailableCauseOther.
func (limiter *unavailableLogLimiter) admitLog(cause string) (admitted bool, suppressedCount uint64) {
	limiter.mutex.Lock()
	defer limiter.mutex.Unlock()
	now := limiter.now()
	if _, isTracked := limiter.stateByCause[cause]; !isTracked && len(limiter.stateByCause) >= maxUnavailableLogCauses {
		cause = unavailableCauseOther
	}
	state, isTracked := limiter.stateByCause[cause]
	if !isTracked {
		state = &unavailableCauseLogState{}
		limiter.stateByCause[cause] = state
	}
	if len(limiter.stateByCause) > maxUnavailableLogCauses+1 {
		panic("cantondisclosure: unavailable log causes exceed their bound")
	}
	if !state.lastAdmittedAt.IsZero() && now.Sub(state.lastAdmittedAt) < unavailableLogInterval {
		state.suppressedCount++
		return false, 0
	}
	suppressedCount = state.suppressedCount
	state.lastAdmittedAt = now
	state.suppressedCount = 0
	return true, suppressedCount
}

// takeSuppressedCounts clears and returns the suppressed counts whose interval ended, or every count with allCauses.
// A taken count starts a new interval of its cause.
func (limiter *unavailableLogLimiter) takeSuppressedCounts(allCauses bool) []suppressedUnavailableLogCount {
	limiter.mutex.Lock()
	defer limiter.mutex.Unlock()
	now := limiter.now()
	taken := []suppressedUnavailableLogCount{}
	for cause, state := range limiter.stateByCause {
		if state.suppressedCount == 0 || (!allCauses && now.Sub(state.lastAdmittedAt) < unavailableLogInterval) {
			continue
		}
		taken = append(taken, suppressedUnavailableLogCount{cause: cause, suppressedCount: state.suppressedCount})
		state.suppressedCount = 0
		state.lastAdmittedAt = now
	}
	slices.SortFunc(taken, func(first, second suppressedUnavailableLogCount) int {
		return strings.Compare(first.cause, second.cause)
	})
	return taken
}

// unavailableCauseOf is the readiness of a not-ready lookup error.
func unavailableCauseOf(err error) string {
	var indexNotReady IndexNotReadyError
	if errors.As(err, &indexNotReady) {
		return indexNotReady.Readiness.String()
	}
	var hostingNotReady ReadPartyHostingNotReadyError
	if errors.As(err, &hostingNotReady) {
		return string(hostingNotReady.Status)
	}
	return unavailableCauseOther
}

// NewDisclosureHTTPHandler bounds the lookups in progress to maxConcurrentLookupRequests.
func NewDisclosureHTTPHandler(lookups CoreContractLookups, maxConcurrentLookupRequests int, logger *zap.Logger) (*DisclosureHTTPHandler, error) {
	if lookups == nil || logger == nil {
		return nil, errors.New("cantondisclosure: HTTP handler needs lookups and a logger")
	}
	if maxConcurrentLookupRequests < 1 || maxConcurrentLookupRequests > maxConcurrentLookupRequestsLimit {
		return nil, fmt.Errorf("cantondisclosure: concurrent lookup request limit %d is outside [1, %d]", maxConcurrentLookupRequests, maxConcurrentLookupRequestsLimit)
	}
	return &DisclosureHTTPHandler{
		lookups:            lookups,
		lookupRequestSlots: make(chan struct{}, maxConcurrentLookupRequests),
		logger:             logger,
		unavailableLogs:    newUnavailableLogLimiter(time.Now),
	}, nil
}

// NewDisclosureHTTPServer applies the read, write and idle timeouts and the header byte limit.
func NewDisclosureHTTPServer(listenAddress string, handler http.Handler) *http.Server {
	if handler == nil {
		panic("cantondisclosure: HTTP server without a handler")
	}
	return &http.Server{
		Addr:              listenAddress,
		Handler:           handler,
		ReadHeaderTimeout: httpReadHeaderTimeout,
		ReadTimeout:       httpReadTimeout,
		WriteTimeout:      httpWriteTimeout,
		IdleTimeout:       httpIdleTimeout,
		MaxHeaderBytes:    httpMaxHeaderBytes,
	}
}

func (handler *DisclosureHTTPHandler) ServeHTTP(writer http.ResponseWriter, request *http.Request) {
	handler.logSuppressedUnavailableLogs(false)
	writer.Header().Set("Cache-Control", "no-store")
	writer.Header().Set("X-Content-Type-Options", "nosniff")
	if len(request.RequestURI) > maxRequestURIBytes || len(request.URL.RawQuery) > maxRequestURIBytes {
		handler.writeClientError(writer, http.StatusRequestURITooLong, publicMessageRequestURITooLong)
		return
	}
	route, isRoute := disclosureRoutes[request.URL.Path]
	if !isRoute {
		handler.writeClientError(writer, http.StatusNotFound, publicMessageNotFound)
		return
	}
	if request.Method != http.MethodGet {
		writer.Header().Set("Allow", http.MethodGet)
		handler.writeClientError(writer, http.StatusMethodNotAllowed, publicMessageMethodNotAllowed)
		return
	}
	if request.ContentLength != 0 || len(request.TransferEncoding) != 0 {
		handler.writeClientError(writer, http.StatusBadRequest, publicMessageBodyNotAllowed)
		return
	}
	if route.isProbe {
		route.serve(handler, writer, request)
		return
	}
	select {
	case handler.lookupRequestSlots <- struct{}{}:
	default:
		writer.Header().Set("Retry-After", strconv.Itoa(httpRetryAfterSeconds))
		handler.writeUnavailable(writer, request, publicMessageServiceBusy, unavailableCauseServiceBusy, errors.New("concurrent lookup request limit reached"))
		return
	}
	defer func() { <-handler.lookupRequestSlots }()
	route.serve(handler, writer, request)
}

func singletonRouteOf(template cantonclient.CoreContractTemplate) func(*DisclosureHTTPHandler, http.ResponseWriter, *http.Request) {
	if !isSingletonTemplate(template) {
		panic(fmt.Sprintf("cantondisclosure: %s is not a singleton template", template))
	}
	return func(handler *DisclosureHTTPHandler, writer http.ResponseWriter, request *http.Request) {
		if err := parseEmptyQuery(request.URL.RawQuery); err != nil {
			handler.writeQueryRejection(writer, err)
			return
		}
		result, err := handler.lookups.LookupSingleton(template)
		handler.writeLookupResult(writer, request, result, err)
	}
}

func (handler *DisclosureHTTPHandler) serveReplayNode(writer http.ResponseWriter, request *http.Request) {
	query, err := parseReplayNodeQuery(request.URL.RawQuery)
	if err != nil {
		handler.writeQueryRejection(writer, err)
		return
	}
	result, err := handler.lookups.LookupCoveringReplayNode(query.consumer, query.namespace, query.digest)
	handler.writeLookupResult(writer, request, result, err)
}

func (handler *DisclosureHTTPHandler) serveClaimedRootsNode(writer http.ResponseWriter, request *http.Request) {
	query, err := parseClaimedRootsNodeQuery(request.URL.RawQuery)
	if err != nil {
		handler.writeQueryRejection(writer, err)
		return
	}
	result, err := handler.lookups.LookupClaimedRootsNode(query.consumer, query.namespace)
	handler.writeLookupResult(writer, request, result, err)
}

func (handler *DisclosureHTTPHandler) serveHealthz(writer http.ResponseWriter, request *http.Request) {
	if err := parseEmptyQuery(request.URL.RawQuery); err != nil {
		handler.writeQueryRejection(writer, err)
		return
	}
	writeJSONBody(writer, http.StatusOK, probeResponse{Status: healthzStatusOK})
}

// serveReadyz is 200 for a ready service; 503 otherwise. The body is the fixed readiness status.
func (handler *DisclosureHTTPHandler) serveReadyz(writer http.ResponseWriter, request *http.Request) {
	if err := parseEmptyQuery(request.URL.RawQuery); err != nil {
		handler.writeQueryRejection(writer, err)
		return
	}
	readiness := handler.lookups.ServiceReadiness()
	statusCode := http.StatusServiceUnavailable
	if readiness.IsReady() {
		statusCode = http.StatusOK
	}
	writeJSONBody(writer, statusCode, probeResponse{Status: string(readiness.Status())})
}

// writeLookupResult maps a lookup error to a fixed public answer.
func (handler *DisclosureHTTPHandler) writeLookupResult(writer http.ResponseWriter, request *http.Request, result CoreContractLookupResult, lookupErr error) {
	switch {
	case lookupErr == nil:
	case errors.Is(lookupErr, ErrIndexNotReady):
		handler.writeUnavailable(writer, request, publicMessageIndexNotReady, unavailableCauseOf(lookupErr), lookupErr)
		return
	case errors.Is(lookupErr, ErrNoCoveringReplayNode):
		handler.writeClientError(writer, http.StatusNotFound, publicMessageNoCoveringNode)
		return
	case errors.Is(lookupErr, ErrReservedReplayNamespace):
		handler.writeClientError(writer, http.StatusBadRequest, publicMessageReservedNamespace)
		return
	default:
		handler.writeServerError(writer, request, http.StatusInternalServerError, publicMessageInternalError, lookupErr)
		return
	}
	contract := result.Contract()
	if contract.ContractID() == (cantonclient.ContractID{}) || result.LiveAtIndexOffset() < 0 {
		handler.writeServerError(writer, request, http.StatusInternalServerError, publicMessageInternalError, errors.New("lookup returned no contract"))
		return
	}
	writeJSONBody(writer, http.StatusOK, coreContractDisclosureResponse{
		TemplateID:       contract.TemplateID(),
		ContractID:       contract.ContractID().String(),
		CreatedEventBlob: base64.StdEncoding.EncodeToString(contract.CreatedEventBlob()),
		SynchronizerID:   contract.SynchronizerID().String(),
		ActiveAtOffset:   result.LiveAtIndexOffset(),
	})
}

func (handler *DisclosureHTTPHandler) writeQueryRejection(writer http.ResponseWriter, err error) {
	var rejection queryRejection
	if !errors.As(err, &rejection) {
		panic(fmt.Sprintf("cantondisclosure: query parse returned a non-rejection error: %v", err))
	}
	handler.writeClientError(writer, http.StatusBadRequest, rejection.publicMessage)
}

func (handler *DisclosureHTTPHandler) writeClientError(writer http.ResponseWriter, statusCode int, publicMessage string) {
	if statusCode < 400 || statusCode > 499 {
		panic(fmt.Sprintf("cantondisclosure: client error with status %d", statusCode))
	}
	requestID := newRequestID()
	writer.Header().Set(requestIDHeaderName, requestID)
	writeJSONBody(writer, statusCode, publicErrorBody{Error: publicMessage, RequestID: requestID})
}

// writeServerError logs internalErr with the request id and writes publicMessage only. A 503 goes through writeUnavailable.
func (handler *DisclosureHTTPHandler) writeServerError(writer http.ResponseWriter, request *http.Request, statusCode int, publicMessage string, internalErr error) {
	if statusCode < 500 || statusCode > 599 || statusCode == http.StatusServiceUnavailable {
		panic(fmt.Sprintf("cantondisclosure: server error with status %d", statusCode))
	}
	requestID := writePublicServerError(writer, statusCode, publicMessage)
	handler.logger.Warn("disclosure request failed", zap.String("requestId", requestID), zap.String("path", request.URL.Path), zap.Int("status", statusCode), zap.Error(internalErr))
}

// writeUnavailable writes a 503 with publicMessage; unavailableLogs limits its log per cause.
func (handler *DisclosureHTTPHandler) writeUnavailable(writer http.ResponseWriter, request *http.Request, publicMessage string, cause string, internalErr error) {
	requestID := writePublicServerError(writer, http.StatusServiceUnavailable, publicMessage)
	admitted, suppressedCount := handler.unavailableLogs.admitLog(cause)
	if !admitted {
		return
	}
	handler.logger.Warn("disclosure request failed", zap.String("requestId", requestID), zap.String("path", request.URL.Path),
		zap.Int("status", http.StatusServiceUnavailable), zap.String("cause", cause), zap.Uint64("suppressedUnavailableLogs", suppressedCount), zap.Error(internalErr))
}

// writePublicServerError writes the fixed public body and returns its request id.
func writePublicServerError(writer http.ResponseWriter, statusCode int, publicMessage string) string {
	requestID := newRequestID()
	writer.Header().Set(requestIDHeaderName, requestID)
	writeJSONBody(writer, statusCode, publicErrorBody{Error: publicMessage, RequestID: requestID})
	return requestID
}

// FlushSuppressedUnavailableLogs logs every suppressed 503 count. Call it at shutdown.
func (handler *DisclosureHTTPHandler) FlushSuppressedUnavailableLogs() {
	handler.logSuppressedUnavailableLogs(true)
}

// logSuppressedUnavailableLogs logs the suppressed counts whose interval ended, or every count with allCauses.
func (handler *DisclosureHTTPHandler) logSuppressedUnavailableLogs(allCauses bool) {
	for _, taken := range handler.unavailableLogs.takeSuppressedCounts(allCauses) {
		handler.logger.Warn("disclosure 503 logs suppressed", zap.String("cause", taken.cause), zap.Uint64("suppressedUnavailableLogs", taken.suppressedCount))
	}
}

func writeJSONBody(writer http.ResponseWriter, statusCode int, body any) {
	encodedBody, err := json.Marshal(body)
	if err != nil {
		panic(fmt.Sprintf("cantondisclosure: encode a response body: %v", err))
	}
	writer.Header().Set("Content-Type", responseContentTypeJSON)
	writer.Header().Set("Content-Length", strconv.Itoa(len(encodedBody)))
	writer.WriteHeader(statusCode)
	_, _ = writer.Write(encodedBody)
}

func newRequestID() string {
	var randomBytes [requestIDRandomByteCount]byte
	if _, err := rand.Read(randomBytes[:]); err != nil {
		panic(fmt.Sprintf("cantondisclosure: crypto/rand failed: %v", err))
	}
	requestID := hex.EncodeToString(randomBytes[:])
	if len(requestID) != requestIDHexCharacterCount {
		panic("cantondisclosure: request id has a wrong length")
	}
	return requestID
}
