package canton

import (
	"strings"
	"testing"

	"github.com/stretchr/testify/assert"
	"github.com/stretchr/testify/require"

	"github.com/certusone/wormhole/node/pkg/cantonclient"
)

// Read-as party fixtures in the Canton party id form.
const (
	readAsGovernanceParty = "guardianGovernance::1220a4d7463bd34b2ba3704401b48ab41d8f88cdcbe512fc1ef071aad97fef106161"
	foreignRegistrarParty = "rogueGovernance::1220c27103c953779ec7a88ef5e7c556ad079a78421c836975e4b1064cf28cf0544f"
)

func guardianGovernanceReadScope(t *testing.T) cantonclient.ReadScope {
	t.Helper()
	return readScopeOfParty(t, readAsGovernanceParty)
}

func readScopeOfParty(t *testing.T, party string) cantonclient.ReadScope {
	t.Helper()
	readScope, err := cantonclient.ParseReadScope(party, false)
	require.NoError(t, err)
	return readScope
}

func anyPartyReadScope(t *testing.T) cantonclient.ReadScope {
	t.Helper()
	readScope, err := cantonclient.ReadAnyPartyUnsafeDevMode(true)
	require.NoError(t, err)
	return readScope
}

func TestCheckRegistrarIsReadAsParty(t *testing.T) {
	cases := []struct {
		name      string
		registrar string
		readScope cantonclient.ReadScope
		wantErr   string
	}{
		{name: "registrar is the read-as party", registrar: readAsGovernanceParty, readScope: guardianGovernanceReadScope(t)},
		{name: "registrar is another party", registrar: foreignRegistrarParty, readScope: guardianGovernanceReadScope(t), wantErr: "is not the read-as party"},
		{name: "registrar differs only in case", registrar: strings.ToUpper(readAsGovernanceParty), readScope: guardianGovernanceReadScope(t), wantErr: "is not the read-as party"},
		{name: "registrar is empty", registrar: "", readScope: guardianGovernanceReadScope(t), wantErr: "is not the read-as party"},
		{name: "any-party scope admits another registrar", registrar: foreignRegistrarParty, readScope: anyPartyReadScope(t)},
		{name: "zero read scope", registrar: readAsGovernanceParty, readScope: cantonclient.ReadScope{}, wantErr: "not constructed"},
	}
	for _, tc := range cases {
		t.Run(tc.name, func(t *testing.T) {
			err := CheckRegistrarIsReadAsParty(tc.registrar, tc.readScope)

			if tc.wantErr != "" {
				require.ErrorContains(t, err, tc.wantErr)
				return
			}
			assert.NoError(t, err)
		})
	}
}
