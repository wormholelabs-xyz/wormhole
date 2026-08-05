package cantonclient

import (
	"context"
	"errors"
	"io"
	"testing"
	"time"

	"github.com/stretchr/testify/assert"
	"github.com/stretchr/testify/require"
	"go.uber.org/zap"
	"google.golang.org/grpc"

	apiv2 "github.com/certusone/wormhole/node/pkg/cantonclient/proto/gen/com/daml/ledger/api/v2"
)

// fakeUpdateService embeds the generated interface (nil) and overrides only
// GetUpdates, returning a scripted stream.
type fakeUpdateService struct {
	apiv2.UpdateServiceClient
	stream apiv2.UpdateService_GetUpdatesClient
}

func (f *fakeUpdateService) GetUpdates(_ context.Context, _ *apiv2.GetUpdatesRequest, _ ...grpc.CallOption) (apiv2.UpdateService_GetUpdatesClient, error) {
	return f.stream, nil
}

// fakeStream embeds grpc.ClientStream (nil) and implements Recv via a hook.
type fakeStream struct {
	grpc.ClientStream
	recv func() (*apiv2.GetUpdatesResponse, error)
}

func (s *fakeStream) Recv() (*apiv2.GetUpdatesResponse, error) { return s.recv() }

func newTestGrpcClient(stream apiv2.UpdateService_GetUpdatesClient) *grpcClient {
	return &grpcClient{
		update: &fakeUpdateService{stream: stream},
		logger: zap.NewNop(),
	}
}

var testTmpl = TemplateID{ModuleName: "Wormhole.Core.State", EntityName: "Emitter"}

// TestSubscribeUpdatesTerminalErrorFailsSubscription is the transport-layer
// regression for the io.EOF silent-wedge bug: a terminal Recv error, including
// a clean io.EOF, must surface on Err() (previously io.EOF returned silently).
func TestSubscribeUpdatesTerminalErrorFailsSubscription(t *testing.T) {
	for name, recvErr := range map[string]error{
		"clean EOF":    io.EOF,
		"stream error": errors.New("connection reset by peer"),
	} {
		t.Run(name, func(t *testing.T) {
			c := newTestGrpcClient(&fakeStream{recv: func() (*apiv2.GetUpdatesResponse, error) {
				return nil, recvErr
			}})
			out := make(chan CantonMessageEvent, 1)
			sub, err := c.SubscribeUpdates(context.Background(), 0, testTmpl, "PublishMessage", out)
			require.NoError(t, err)
			defer sub.Unsubscribe()

			select {
			case e := <-sub.Err():
				require.Error(t, e)
				assert.Contains(t, e.Error(), "GetUpdates stream ended")
			case <-time.After(2 * time.Second):
				t.Fatal("terminal Recv error did not surface on Err()")
			}
		})
	}
}

// TestSubscribeUpdatesCancellationIsSilent proves a Recv error from cancellation
// (Unsubscribe / parent ctx) is not reported as a failure: Done closes but Err()
// stays empty, so a normal shutdown is not mistaken for a stream fault.
func TestSubscribeUpdatesCancellationIsSilent(t *testing.T) {
	release := make(chan struct{})
	c := newTestGrpcClient(&fakeStream{recv: func() (*apiv2.GetUpdatesResponse, error) {
		<-release
		return nil, context.Canceled
	}})
	ctx, cancel := context.WithCancel(context.Background())
	out := make(chan CantonMessageEvent, 1)
	sub, err := c.SubscribeUpdates(ctx, 0, testTmpl, "PublishMessage", out)
	require.NoError(t, err)

	cancel()       // cancels streamCtx → streamCtx.Err() != nil
	close(release) // let Recv return context.Canceled

	select {
	case <-sub.Done():
		select {
		case e := <-sub.Err():
			t.Fatalf("cancellation must not fail the subscription: %v", e)
		default:
		}
	case <-time.After(2 * time.Second):
		t.Fatal("subscription did not finish on cancellation")
	}
}
