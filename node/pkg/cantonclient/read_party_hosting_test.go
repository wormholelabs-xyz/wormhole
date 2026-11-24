package cantonclient

import (
	"context"
	"errors"
	"testing"

	"github.com/stretchr/testify/require"
	"google.golang.org/grpc"

	apiv2 "github.com/certusone/wormhole/node/pkg/cantonclient/proto/gen/com/daml/ledger/api/v2"
)

type fakeConnectedSynchronizersService struct {
	apiv2.StateServiceClient
	response *apiv2.GetConnectedSynchronizersResponse
	err      error
	request  *apiv2.GetConnectedSynchronizersRequest
}

func (service *fakeConnectedSynchronizersService) GetConnectedSynchronizers(_ context.Context, request *apiv2.GetConnectedSynchronizersRequest, _ ...grpc.CallOption) (*apiv2.GetConnectedSynchronizersResponse, error) {
	service.request = request
	return service.response, service.err
}

func connectedSynchronizer(permission apiv2.ParticipantPermission) *apiv2.GetConnectedSynchronizersResponse_ConnectedSynchronizer {
	return &apiv2.GetConnectedSynchronizersResponse_ConnectedSynchronizer{
		SynchronizerAlias: "global",
		SynchronizerId:    testSynchronizerID,
		Permission:        permission,
	}
}

func TestReadPartyIsHosted(t *testing.T) {
	testCases := []struct {
		name           string
		synchronizers  []*apiv2.GetConnectedSynchronizersResponse_ConnectedSynchronizer
		expectedHosted bool
	}{
		{name: "no connected synchronizer hosts the party", expectedHosted: false},
		{name: "confirmation host", synchronizers: []*apiv2.GetConnectedSynchronizersResponse_ConnectedSynchronizer{
			connectedSynchronizer(apiv2.ParticipantPermission_PARTICIPANT_PERMISSION_CONFIRMATION)}, expectedHosted: true},
		{name: "observation host", synchronizers: []*apiv2.GetConnectedSynchronizersResponse_ConnectedSynchronizer{
			connectedSynchronizer(apiv2.ParticipantPermission_PARTICIPANT_PERMISSION_OBSERVATION)}, expectedHosted: true},
		{name: "submission host", synchronizers: []*apiv2.GetConnectedSynchronizersResponse_ConnectedSynchronizer{
			connectedSynchronizer(apiv2.ParticipantPermission_PARTICIPANT_PERMISSION_SUBMISSION)}, expectedHosted: true},
		{name: "unspecified permission only", synchronizers: []*apiv2.GetConnectedSynchronizersResponse_ConnectedSynchronizer{
			connectedSynchronizer(apiv2.ParticipantPermission_PARTICIPANT_PERMISSION_UNSPECIFIED)}, expectedHosted: false},
		{name: "unspecified and confirmation", synchronizers: []*apiv2.GetConnectedSynchronizersResponse_ConnectedSynchronizer{
			connectedSynchronizer(apiv2.ParticipantPermission_PARTICIPANT_PERMISSION_UNSPECIFIED),
			connectedSynchronizer(apiv2.ParticipantPermission_PARTICIPANT_PERMISSION_CONFIRMATION)}, expectedHosted: true},
	}
	for _, testCase := range testCases {
		t.Run(testCase.name, func(t *testing.T) {
			service := &fakeConnectedSynchronizersService{response: &apiv2.GetConnectedSynchronizersResponse{ConnectedSynchronizers: testCase.synchronizers}}
			decoder := testCoreContractDecoder(t)
			reader := &grpcCoreContractReader{state: service, decoder: decoder}
			hosted, err := reader.ReadPartyIsHosted(context.Background())
			require.NoError(t, err)
			require.Equal(t, testCase.expectedHosted, hosted)
			require.Equal(t, decoder.guardianGovernance.String(), service.request.GetParty())
			require.Empty(t, service.request.GetParticipantId())
		})
	}
}

func TestReadPartyIsHostedReturnsTheReadError(t *testing.T) {
	readErr := errors.New("unavailable")
	reader := &grpcCoreContractReader{state: &fakeConnectedSynchronizersService{err: readErr}, decoder: testCoreContractDecoder(t)}
	hosted, err := reader.ReadPartyIsHosted(context.Background())
	require.ErrorIs(t, err, readErr)
	require.False(t, hosted)
}
