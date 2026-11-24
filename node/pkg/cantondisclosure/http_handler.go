package cantondisclosure

import (
	"crypto/rand"
	"encoding/base64"
	"encoding/hex"
	"encoding/json"
	"errors"
	"fmt"
	"net/http"
	"strconv"
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
	// httpRetryAfterSeconds is the Retry-After of a 503 that a short wait can clear.
	httpRetryAfterSeconds = 1
	// unavailableLogInterval bounds the 503 warn logs to one per interval.
	unavailableLogInterval = time.Second
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

// unavailableLogLimiter admits one 503 warn log per unavailableLogInterval and counts the others.
type unavailableLogLimiter struct {
	now             func() time.Time
	mutex           sync.Mutex
	lastAdmittedAt  time.Time
	suppressedCount uint64
}

// admitLog returns true at most once per unavailableLogInterval, with the count suppressed since the last admitted log.
func (limiter *unavailableLogLimiter) admitLog() (admitted bool, suppressedCount uint64) {
	limiter.mutex.Lock()
	defer limiter.mutex.Unlock()
	now := limiter.now()
	if !limiter.lastAdmittedAt.IsZero() && now.Sub(limiter.lastAdmittedAt) < unavailableLogInterval {
		limiter.suppressedCount++
		return false, 0
	}
	suppressedCount = limiter.suppressedCount
	limiter.lastAdmittedAt = now
	limiter.suppressedCount = 0
	return true, suppressedCount
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
		unavailableLogs:    &unavailableLogLimiter{now: time.Now},
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
		handler.writeServerError(writer, request, http.StatusServiceUnavailable, publicMessageServiceBusy, errors.New("concurrent lookup request limit reached"))
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
		// A byte cap lasts until the rebuild at the next resnapshot interval.
		var notReady IndexNotReadyError
		if !errors.As(lookupErr, &notReady) || notReady.Readiness != IndexReadinessByteCapReached {
			writer.Header().Set("Retry-After", strconv.Itoa(httpRetryAfterSeconds))
		}
		handler.writeServerError(writer, request, http.StatusServiceUnavailable, publicMessageIndexNotReady, lookupErr)
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

// writeServerError logs internalErr with the request id and writes publicMessage only.
// The 503 logs are rate limited by unavailableLogs.
func (handler *DisclosureHTTPHandler) writeServerError(writer http.ResponseWriter, request *http.Request, statusCode int, publicMessage string, internalErr error) {
	if statusCode < 500 || statusCode > 599 {
		panic(fmt.Sprintf("cantondisclosure: server error with status %d", statusCode))
	}
	requestID := newRequestID()
	writer.Header().Set(requestIDHeaderName, requestID)
	writeJSONBody(writer, statusCode, publicErrorBody{Error: publicMessage, RequestID: requestID})
	logFields := []zap.Field{zap.String("requestId", requestID), zap.String("path", request.URL.Path), zap.Int("status", statusCode), zap.Error(internalErr)}
	if statusCode == http.StatusServiceUnavailable {
		admitted, suppressedCount := handler.unavailableLogs.admitLog()
		if !admitted {
			return
		}
		logFields = append(logFields, zap.Uint64("suppressedUnavailableLogs", suppressedCount))
	}
	handler.logger.Warn("disclosure request failed", logFields...)
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
