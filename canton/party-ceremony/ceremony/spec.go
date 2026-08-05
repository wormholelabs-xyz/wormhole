// Package ceremony contains the guardian party-ceremony domain: the workflow
// spec, the multi-actor onboarding state machine, and the ports it depends on.
//
// The design goal is that every collaborator is injected behind a small
// interface (Topology, Signer, Store) so the whole workflow is testable with
// fakes that carry real business logic, and so the Canton backend can evolve
// (console today, Admin API gRPC later) without touching the domain.
package ceremony

import (
	"bytes"
	"fmt"
)

// Owner is one guardian's identity in a ceremony: a stable actor id and the
// Ed25519 public key (X.509 SubjectPublicKeyInfo DER) whose namespace it owns.
// Private keys never appear anywhere in this package; signing goes through the
// Signer port.
type Owner struct {
	ID           string `json:"id"`
	PublicKeyDER []byte `json:"publicKeyDer"`
}

// Spec is the immutable input of one ceremony, agreed before anyone acts and
// persisted once (workflow.json). Everything mutable lives in the report
// store, never here.
type Spec struct {
	WorkflowID      string  `json:"workflowId"`
	Kind            string  `json:"kind"`
	Threshold       int     `json:"threshold"`
	Coordinator     string  `json:"coordinator"`
	GovernanceParty string  `json:"governanceParty"`
	ObserverParty   string  `json:"observerParty"`
	Owners          []Owner `json:"owners"`
}

// KindOnboarding creates the decentralized namespace and both guardian
// parties from scratch. It is the only workflow kind implemented so far.
const KindOnboarding = "onboarding"

// NewOnboardingSpec validates and assembles the immutable input of an
// onboarding ceremony. The party names are fixed to the production names;
// they are fields (not constants) only so the spec file is self-describing.
func NewOnboardingSpec(workflowID string, coordinator string, threshold int, owners []Owner) (Spec, error) {
	s := Spec{
		WorkflowID:      workflowID,
		Kind:            KindOnboarding,
		Threshold:       threshold,
		Coordinator:     coordinator,
		GovernanceParty: "guardianGovernance",
		ObserverParty:   "guardianObserver",
		Owners:          owners,
	}
	if err := s.validate(); err != nil {
		return Spec{}, err
	}
	return s, nil
}

func (s Spec) validate() error {
	if s.WorkflowID == "" {
		return fmt.Errorf("spec: workflow id must not be empty")
	}
	if len(s.Owners) == 0 {
		return fmt.Errorf("spec: at least one owner is required")
	}
	if s.Threshold < 1 || s.Threshold > len(s.Owners) {
		return fmt.Errorf("spec: threshold %d out of range 1..%d", s.Threshold, len(s.Owners))
	}
	seenID := map[string]bool{}
	for i, o := range s.Owners {
		if o.ID == "" {
			return fmt.Errorf("spec: owner %d has an empty id", i)
		}
		if seenID[o.ID] {
			return fmt.Errorf("spec: duplicate owner id %q", o.ID)
		}
		seenID[o.ID] = true
		if len(o.PublicKeyDER) == 0 {
			return fmt.Errorf("spec: owner %q has no public key", o.ID)
		}
		for _, prev := range s.Owners[:i] {
			if bytes.Equal(prev.PublicKeyDER, o.PublicKeyDER) {
				return fmt.Errorf("spec: owners %q and %q share a public key", prev.ID, o.ID)
			}
		}
	}
	if !seenID[s.Coordinator] {
		return fmt.Errorf("spec: coordinator %q is not an owner", s.Coordinator)
	}
	return nil
}

// Owner returns the owner with the given id.
func (s Spec) Owner(id string) (Owner, bool) {
	for _, o := range s.Owners {
		if o.ID == id {
			return o, true
		}
	}
	return Owner{}, false
}

// OwnerKeys returns every owner's public key in spec order.
func (s Spec) OwnerKeys() [][]byte {
	keys := make([][]byte, len(s.Owners))
	for i, o := range s.Owners {
		keys[i] = o.PublicKeyDER
	}
	return keys
}
