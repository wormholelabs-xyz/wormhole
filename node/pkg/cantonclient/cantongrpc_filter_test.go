package cantonclient

import (
	"testing"

	"github.com/stretchr/testify/assert"
	"github.com/stretchr/testify/require"
)

func TestUpdateFormatNarrowsToReadAsParty(t *testing.T) {
	const governanceParty = "guardianGovernance::1220abcd"
	client := &grpcClient{readAsParty: governanceParty}

	eventFormat := client.updateFormat().GetIncludeTransactions().GetEventFormat()
	require.NotNil(t, eventFormat)
	assert.Nil(t, eventFormat.GetFiltersForAnyParty(), "a narrowed stream must not use filters_for_any_party")
	require.Len(t, eventFormat.GetFiltersByParty(), 1)
	assert.Contains(t, eventFormat.GetFiltersByParty(), governanceParty)
	assert.True(t, eventFormat.GetVerbose())
}

func TestUpdateFormatWildcardWithoutReadAsParty(t *testing.T) {
	client := &grpcClient{}

	eventFormat := client.updateFormat().GetIncludeTransactions().GetEventFormat()
	require.NotNil(t, eventFormat)
	assert.NotNil(t, eventFormat.GetFiltersForAnyParty())
	assert.Empty(t, eventFormat.GetFiltersByParty())
}
