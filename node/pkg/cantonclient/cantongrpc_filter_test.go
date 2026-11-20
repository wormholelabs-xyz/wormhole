package cantonclient

import (
	"testing"

	"github.com/stretchr/testify/assert"
	"github.com/stretchr/testify/require"
)

func TestUpdateFormatNarrowsToReadAsParty(t *testing.T) {
	client := &grpcClient{readScope: governanceReadScope}

	eventFormat := client.updateFormat().GetIncludeTransactions().GetEventFormat()
	require.NotNil(t, eventFormat)
	assert.Nil(t, eventFormat.GetFiltersForAnyParty(), "a narrowed stream must not use filters_for_any_party")
	require.Len(t, eventFormat.GetFiltersByParty(), 1)
	assert.Contains(t, eventFormat.GetFiltersByParty(), governanceParty)
	assert.True(t, eventFormat.GetVerbose())
}

func TestUpdateFormatWildcardForAnyPartyReadScope(t *testing.T) {
	client := &grpcClient{readScope: anyPartyReadScope}

	eventFormat := client.updateFormat().GetIncludeTransactions().GetEventFormat()
	require.NotNil(t, eventFormat)
	assert.NotNil(t, eventFormat.GetFiltersForAnyParty())
	assert.Empty(t, eventFormat.GetFiltersByParty())
}

func TestUpdateFormatPanicsOnZeroReadScope(t *testing.T) {
	client := &grpcClient{}

	assert.Panics(t, func() { client.updateFormat() })
}

func TestNewCantonGrpcClientRefusesZeroReadScope(t *testing.T) {
	client, err := NewCantonGrpcClient("127.0.0.1:0", ReadScope{}, nil, nil)

	require.ErrorContains(t, err, "not constructed")
	assert.Nil(t, client)
}
