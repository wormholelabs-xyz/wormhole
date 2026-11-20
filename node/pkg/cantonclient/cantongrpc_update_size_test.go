package cantonclient

import (
	"context"
	"net"
	"strings"
	"testing"
	"time"

	"github.com/stretchr/testify/assert"
	"github.com/stretchr/testify/require"
	"go.uber.org/zap"
	"google.golang.org/grpc"
	"google.golang.org/grpc/codes"
	"google.golang.org/grpc/credentials/insecure"
	"google.golang.org/grpc/status"
	"google.golang.org/grpc/test/bufconn"

	apiv2 "github.com/certusone/wormhole/node/pkg/cantonclient/proto/gen/com/daml/ledger/api/v2"
)

// gRPC-Go defaultClientMaxReceiveMessageSize.
const grpcDefaultMaxReceiveBytes = 4 << 20

// largeUpdateServer serves one transaction with a single publish of the given payload hex.
type largeUpdateServer struct {
	apiv2.UnimplementedUpdateServiceServer
	payloadHex string
}

func (s *largeUpdateServer) publishTransaction() *apiv2.Transaction {
	return &apiv2.Transaction{
		UpdateId: otherUpdateIDHex,
		Offset:   7,
		Events: []*apiv2.Event{{Event: &apiv2.Event_Exercised{Exercised: &apiv2.ExercisedEvent{
			TemplateId:     &apiv2.Identifier{PackageId: pinnedPackageID, ModuleName: "Wormhole.Core.State", EntityName: "Emitter"},
			Choice:         "PublishMessage",
			ExerciseResult: tupleResult("00deadbeef", wormholeRecord(map[string]*apiv2.Value{"payload": messagePayloadVal(s.payloadHex)})),
			Consuming:      true,
			AcsDelta:       true,
		}}}},
	}
}

// GetUpdates sends the transaction, then holds the stream open until the client cancels.
func (s *largeUpdateServer) GetUpdates(_ *apiv2.GetUpdatesRequest, stream apiv2.UpdateService_GetUpdatesServer) error {
	err := stream.Send(&apiv2.GetUpdatesResponse{Update: &apiv2.GetUpdatesResponse_Transaction{Transaction: s.publishTransaction()}})
	if err != nil {
		return err
	}
	<-stream.Context().Done()
	return nil
}

func (s *largeUpdateServer) GetUpdateById(context.Context, *apiv2.GetUpdateByIdRequest) (*apiv2.GetUpdateResponse, error) {
	return &apiv2.GetUpdateResponse{Update: &apiv2.GetUpdateResponse_Transaction{Transaction: s.publishTransaction()}}, nil
}

// dialLargeUpdateServer connects a client from NewCantonGrpcClient to an in-memory largeUpdateServer.
func dialLargeUpdateServer(t *testing.T, payloadHex string) CantonClient {
	t.Helper()
	listener := bufconn.Listen(1 << 20)
	server := grpc.NewServer()
	apiv2.RegisterUpdateServiceServer(server, &largeUpdateServer{payloadHex: payloadHex})
	go func() { _ = server.Serve(listener) }()
	t.Cleanup(server.Stop)

	client, err := NewCantonGrpcClient("passthrough:///bufconn", "", zap.NewNop(), insecure.NewCredentials(),
		grpc.WithContextDialer(func(ctx context.Context, _ string) (net.Conn, error) { return listener.DialContext(ctx) }))
	require.NoError(t, err)
	t.Cleanup(func() { _ = client.Close() })
	return client
}

type updateSizeCase struct {
	name                string
	payloadHexCharCount int
	isReceived          bool
}

// Each update carries the payload hex plus a small envelope.
func updateSizeCases() []updateSizeCase {
	return []updateSizeCase{
		{name: "below the gRPC default", payloadHexCharCount: 1 << 20, isReceived: true},
		{name: "above the gRPC default", payloadHexCharCount: 2 * grpcDefaultMaxReceiveBytes, isReceived: true},
		{name: "above maxLedgerUpdateBytes", payloadHexCharCount: maxLedgerUpdateBytes + 2, isReceived: false},
	}
}

func TestSubscribeUpdatesUpdateSizeLimit(t *testing.T) {
	for _, testCase := range updateSizeCases() {
		t.Run(testCase.name, func(t *testing.T) {
			client := dialLargeUpdateServer(t, strings.Repeat("ab", testCase.payloadHexCharCount/2))
			out := make(chan CantonMessageEvent, 1)
			subscription, err := client.SubscribeUpdates(context.Background(), 0, pinnedTmpl, "PublishMessage", out)
			require.NoError(t, err)
			defer subscription.Unsubscribe()

			select {
			case event := <-out:
				require.True(t, testCase.isReceived, "update above the limit was received")
				assert.Len(t, event.Message.Payload, testCase.payloadHexCharCount/2)
			case subscriptionErr := <-subscription.Err():
				require.False(t, testCase.isReceived, "update was not received: %v", subscriptionErr)
				assert.Equal(t, codes.ResourceExhausted, status.Code(subscriptionErr))
			case <-time.After(30 * time.Second):
				t.Fatal("no update and no subscription error")
			}
		})
	}
}

func TestGetTransactionByUpdateIDUpdateSizeLimit(t *testing.T) {
	updateID, err := ParseUpdateID(otherUpdateIDHex)
	require.NoError(t, err)
	for _, testCase := range updateSizeCases() {
		t.Run(testCase.name, func(t *testing.T) {
			client := dialLargeUpdateServer(t, strings.Repeat("ab", testCase.payloadHexCharCount/2))
			transaction, err := client.GetTransactionByUpdateID(context.Background(), updateID, pinnedTmpl, "PublishMessage")
			if !testCase.isReceived {
				require.Error(t, err)
				assert.Equal(t, codes.ResourceExhausted, status.Code(err))
				return
			}
			require.NoError(t, err)
			require.Len(t, transaction.Messages, 1)
			assert.Len(t, transaction.Messages[0].Payload, testCase.payloadHexCharCount/2)
		})
	}
}
