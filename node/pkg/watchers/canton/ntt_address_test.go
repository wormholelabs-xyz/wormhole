package canton

import (
	"testing"

	"github.com/stretchr/testify/assert"
)

// TestNttTransceiverEmitterAddressMatchesVector pins the NTT transceiver derivation to the shared
// emitter vector (Test.TestCore:testAddressVector): the transceiver Emitter has registrar = operator and owner = gg.
func TestNttTransceiverEmitterAddressMatchesVector(t *testing.T) {
	const (
		vectorOperator           = sampleRegistrar
		vectorGuardianGovernance = sampleOwner
		vectorEmitterID          = sampleEmitterID
	)

	tests := []struct {
		name               string
		operator           string
		guardianGovernance string
		emitterID          uint64
		wantVectorAddress  bool
	}{
		{"shared vector", vectorOperator, vectorGuardianGovernance, vectorEmitterID, true},
		{"operator and gg swapped", vectorGuardianGovernance, vectorOperator, vectorEmitterID, false},
		{"other emitter id", vectorOperator, vectorGuardianGovernance, vectorEmitterID + 1, false},
		{"other operator", "other-operator::1220deadbeef", vectorGuardianGovernance, vectorEmitterID, false},
		{"other gg", vectorOperator, "other-gg::1220cafebabe", vectorEmitterID, false},
	}

	for _, tc := range tests {
		t.Run(tc.name, func(t *testing.T) {
			got := NttTransceiverEmitterAddress(tc.operator, tc.guardianGovernance, tc.emitterID)
			if tc.wantVectorAddress {
				assert.Equal(t, wantAddr(t), got)
				return
			}
			assert.NotEqual(t, wantAddr(t), got)
		})
	}
}
