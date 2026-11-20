package canton

import (
	"fmt"

	"github.com/certusone/wormhole/node/pkg/cantonclient"
)

// CheckRegistrarIsReadAsParty admits a publish whose Emitter registrar is the
// read-as party. The any-party scope admits every registrar.
// SECURITY: the read-party signatory check admits gg as the owner of an Emitter
// with another registrar. Exact party id equality.
func CheckRegistrarIsReadAsParty(registrar string, readScope cantonclient.ReadScope) error {
	if err := readScope.CheckWasConstructed(); err != nil {
		return err
	}
	if readScope.IsAnyPartyUnsafeDevMode() {
		return nil
	}
	readAsParty, isGuardianGovernance := readScope.GuardianGovernanceParty()
	if !isGuardianGovernance {
		panic("canton: constructed read scope is neither any party nor guardianGovernance")
	}
	if registrar != readAsParty.String() {
		return fmt.Errorf("canton: registrar %q is not the read-as party %q", registrar, readAsParty)
	}
	return nil
}
