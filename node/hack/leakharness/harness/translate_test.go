package harness

import (
	"testing"

	"github.com/stretchr/testify/require"

	"github.com/certusone/wormhole/node/hack/leakharness/fakerpc/common"
)

// TestTranslateAction pins the YAML-FaultAction-to-internal-Fault
// mapping. If anyone refactors translateAction and inverts two cases
// (a real human-error class), this fires.
func TestTranslateAction(t *testing.T) {
	cases := []struct {
		in   FaultAction
		want common.Fault
	}{
		{FaultClose, common.FaultCloseAllConnections},
		{FaultDisconnect, common.FaultCloseAllConnections},
		{FaultMalformed, common.FaultMalformed},
		{FaultSlow, common.FaultSlow},
		{FaultFreeze, common.FaultStuck},
		{FaultHeal, common.FaultNone},
		{FaultAction("unknown"), common.FaultNone},
		{FaultAction(""), common.FaultNone},
	}
	for _, c := range cases {
		t.Run(string(c.in), func(t *testing.T) {
			require.Equal(t, c.want, translateAction(c.in))
		})
	}
}
