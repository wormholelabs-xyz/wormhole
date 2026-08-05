package ceremony

import (
	"context"
	"encoding/json"
	"fmt"
)

// Onboarding is the multi-actor state machine that creates the guardian
// decentralized namespace and both guardian parties. Each guardian (and the
// coordinator, who is one of them) constructs it with its own actor id,
// Topology, and Signer, plus the shared Store, and calls Advance whenever it
// is its turn. Operations already recorded in the store are skipped, so
// Advance is idempotent and the ceremony survives any interleaving of actors,
// process restarts, and re-runs.
//
// Every step that produces an owner signature first re-derives the
// transaction hash from its body and checks the decoded transaction against
// the spec (Topology.Describe): the store is untrusted shared state, so an
// owner never signs a hash it has not independently verified.
//
// Step plan (op key -> responsible actor):
//
//	identify/<owner>            each owner    record own participant uid
//	delegate/<owner>            each owner    self-signed root NamespaceDelegation
//	dns/prepare                 coordinator   DecentralizedNamespaceDefinition (unsigned)
//	dns/sign/<owner>            each owner    signature over the DNS hash (verified vs spec)
//	dns/submit                  coordinator   submit with ALL owner signatures (creation rule)
//	host/governance/prepare     coordinator   PartyToParticipant: keys@threshold, Confirmation everywhere
//	host/governance/sign/<o>    each owner    owner authorization signature (verified vs spec)
//	host/governance/consent/<o> each owner    own participant's consent-to-host
//	host/governance/submit      coordinator   submit at >= threshold owner sigs + every consent
//	host/observer/...           same shape    keyless party, Observation everywhere
//	artifacts                   coordinator   final record (namespace, party ids, participants)
//
// Onboarding requires EVERY owner to take part: namespace creation needs all
// owners' signatures, and hosting needs every hosting participant's consent.
// The k-of-n threshold governs the parties' ongoing authorization (and later
// ceremonies), not attendance at the one-time bootstrap; a guardian that
// misses onboarding is added afterwards by a separate add-guardian ceremony.
type Onboarding struct {
	spec   Spec
	actor  string
	topo   Topology
	signer Signer
	store  Store
}

// NewOnboarding wires one actor's view of the ceremony. The actor must be one
// of the spec's owners.
func NewOnboarding(spec Spec, actor string, topo Topology, signer Signer, store Store) (*Onboarding, error) {
	if err := spec.validate(); err != nil {
		return nil, err
	}
	if spec.Kind != KindOnboarding {
		return nil, fmt.Errorf("onboarding: spec kind is %q", spec.Kind)
	}
	if _, ok := spec.Owner(actor); !ok {
		return nil, fmt.Errorf("onboarding: actor %q is not an owner in the spec", actor)
	}
	if topo == nil || signer == nil || store == nil {
		return nil, fmt.Errorf("onboarding: topology, signer and store are all required")
	}
	return &Onboarding{spec: spec, actor: actor, topo: topo, signer: signer, store: store}, nil
}

// Status reports what one Advance call did and what is still outstanding.
type Status struct {
	Complete bool
	Ran      []string // ops executed by this call
	Waiting  []string // ops not yet done (someone else's turn, or blocked on a gate)
}

// Artifacts is the ceremony's final record, written by the coordinator once
// both parties are live. Participants lists the hosting participant uids; the
// per-party permissions are on the ledger (Confirmation for governance,
// Observation for observer), not repeated here.
type Artifacts struct {
	Namespace         string   `json:"namespace"`
	GovernancePartyID string   `json:"governancePartyId"`
	ObserverPartyID   string   `json:"observerPartyId"`
	Threshold         int      `json:"threshold"`
	Participants      []string `json:"participants"`
}

type step struct {
	key   string
	owner string
	ready func(ctx context.Context) (bool, error)
	run   func(ctx context.Context) (any, error)
}

// Advance executes every step that is this actor's responsibility and ready,
// records the results, and reports overall progress. It never blocks waiting
// for other actors: their pending steps surface in Status.Waiting.
func (o *Onboarding) Advance(ctx context.Context) (Status, error) {
	status := Status{}
	for _, st := range o.plan() {
		done, err := o.done(st.key)
		if err != nil {
			return status, err
		}
		if done {
			continue
		}
		if st.owner != o.actor {
			status.Waiting = append(status.Waiting, st.key)
			continue
		}
		ready, err := st.ready(ctx)
		if err != nil {
			return status, err
		}
		if !ready {
			status.Waiting = append(status.Waiting, st.key)
			continue
		}
		value, err := st.run(ctx)
		if err != nil {
			return status, fmt.Errorf("op %s: %w", st.key, err)
		}
		if err := o.store.Put(st.key, value); err != nil {
			return status, fmt.Errorf("op %s: recording result: %w", st.key, err)
		}
		status.Ran = append(status.Ran, st.key)
	}
	status.Complete = len(status.Waiting) == 0
	return status, nil
}

func (o *Onboarding) plan() []step {
	always := func(context.Context) (bool, error) { return true, nil }
	var steps []step

	// Every owner announces its participant and roots its namespace.
	for _, owner := range o.spec.Owners {
		steps = append(steps, step{
			key:   "identify/" + owner.ID,
			owner: owner.ID,
			ready: always,
			run: func(ctx context.Context) (any, error) {
				return o.topo.ParticipantID(ctx)
			},
		})
		pub := owner.PublicKeyDER
		steps = append(steps, step{
			key:   "delegate/" + owner.ID,
			owner: owner.ID,
			ready: always,
			run: func(ctx context.Context) (any, error) {
				tx, err := o.topo.PrepareRootDelegation(ctx, pub)
				if err != nil {
					return nil, err
				}
				// Verify before signing: this must be a root delegation for
				// exactly this owner's own key.
				if err := o.verify(ctx, tx, func(ctx context.Context, v TxView) error {
					if v.Kind != DelegationTx {
						return fmt.Errorf("expected a delegation, got %q", v.Kind)
					}
					fp, err := o.topo.Fingerprint(ctx, pub)
					if err != nil {
						return err
					}
					if len(v.OwnerFingerprints) != 1 || v.OwnerFingerprints[0] != fp {
						return fmt.Errorf("delegation is not for this owner's key")
					}
					return nil
				}); err != nil {
					return nil, err
				}
				sig, err := o.signer.Sign(tx.HashHex)
				if err != nil {
					return nil, err
				}
				fp, err := o.topo.Fingerprint(ctx, pub)
				if err != nil {
					return nil, err
				}
				own := []OwnerSignature{{OwnerID: o.actor, Fingerprint: fp, SigHex: sig}}
				if err := o.topo.Submit(ctx, tx, own, nil); err != nil {
					return nil, err
				}
				return tx, nil
			},
		})
	}

	// Decentralized namespace: prepared by the coordinator, signed by every
	// owner (creating a brand-new namespace requires all declared owners; the
	// threshold governs everything after), submitted by the coordinator.
	steps = append(steps, step{
		key:   "dns/prepare",
		owner: o.spec.Coordinator,
		ready: func(ctx context.Context) (bool, error) {
			return o.allDone(o.perOwnerKeys("identify"), o.perOwnerKeys("delegate"))
		},
		run: func(ctx context.Context) (any, error) {
			return o.topo.PrepareDecentralizedNamespace(ctx, o.spec.OwnerKeys(), o.spec.Threshold)
		},
	})
	steps = append(steps, o.signSteps("dns", func(ctx context.Context, v TxView) error {
		if v.Kind != NamespaceTx {
			return fmt.Errorf("expected a namespace definition, got %q", v.Kind)
		}
		want, err := o.fingerprints(ctx, o.spec.OwnerKeys())
		if err != nil {
			return err
		}
		if !stringSetEqual(v.OwnerFingerprints, want) {
			return fmt.Errorf("namespace owner keys do not match the spec")
		}
		if v.Threshold != o.spec.Threshold {
			return fmt.Errorf("namespace threshold %d does not match spec %d", v.Threshold, o.spec.Threshold)
		}
		return nil
	})...)
	steps = append(steps, step{
		key:   "dns/submit",
		owner: o.spec.Coordinator,
		ready: func(ctx context.Context) (bool, error) {
			sigs, err := o.signatures("dns")
			if err != nil {
				return false, err
			}
			return len(sigs) == len(o.spec.Owners), nil
		},
		run: func(ctx context.Context) (any, error) {
			return o.submit(ctx, "dns", nil)
		},
	})

	// Both parties, same shape: governance carries the threshold signing keys
	// and Confirmation hosting at the guardian threshold; observer is keyless
	// and Observation.
	steps = append(steps, o.hostingSteps("governance", o.spec.GovernanceParty, Confirmation, o.spec.OwnerKeys())...)
	steps = append(steps, o.hostingSteps("observer", o.spec.ObserverParty, Observation, nil)...)

	steps = append(steps, step{
		key:   "artifacts",
		owner: o.spec.Coordinator,
		ready: func(ctx context.Context) (bool, error) {
			return o.allDone([]string{"host/governance/submit", "host/observer/submit"})
		},
		run: func(ctx context.Context) (any, error) {
			return o.buildArtifacts()
		},
	})
	return steps
}

// hostingSteps produces the prepare/sign/consent/submit sequence for one
// party. signingKeys nil allocates a keyless (purely observing) party. The
// confirmation threshold across hosting participants is min(guardian
// threshold, host count): the guardian threshold on a real ≥k-participant
// topology, and necessarily 1 when a single participant hosts (Canton rejects
// a confirmation threshold above the host count).
func (o *Onboarding) hostingSteps(name string, partyName string, permission Permission, signingKeys [][]byte) []step {
	prefix := "host/" + name
	var steps []step
	steps = append(steps, step{
		key:   prefix + "/prepare",
		owner: o.spec.Coordinator,
		ready: func(ctx context.Context) (bool, error) {
			return o.allDone([]string{"dns/submit"})
		},
		run: func(ctx context.Context) (any, error) {
			dns, err := o.prepared("dns/prepare")
			if err != nil {
				return nil, err
			}
			hosts, err := o.hosts(permission)
			if err != nil {
				return nil, err
			}
			confirmationThreshold := 1
			if signingKeys != nil {
				confirmationThreshold = o.spec.Threshold
				if confirmationThreshold > len(hosts) {
					confirmationThreshold = len(hosts)
				}
			}
			spec := HostingSpec{
				PartyName:             partyName,
				Namespace:             dns.Namespace,
				Hosts:                 hosts,
				ConfirmationThreshold: confirmationThreshold,
			}
			if signingKeys != nil {
				spec.SigningKeysDER = signingKeys
				spec.SigningThreshold = o.spec.Threshold
			}
			return o.topo.PreparePartyHosting(ctx, spec)
		},
	})
	steps = append(steps, o.signSteps(prefix, func(ctx context.Context, v TxView) error {
		if v.Kind != HostingTx {
			return fmt.Errorf("expected a hosting mapping, got %q", v.Kind)
		}
		if v.PartyName != partyName {
			return fmt.Errorf("hosting party %q does not match expected %q", v.PartyName, partyName)
		}
		if !allHostsPermission(v.Hosts, permission) {
			return fmt.Errorf("hosting permission is not uniformly %s", permission)
		}
		if signingKeys == nil {
			if len(v.SigningKeyFingerprints) != 0 {
				return fmt.Errorf("observer party must be keyless, found %d signing keys", len(v.SigningKeyFingerprints))
			}
		} else {
			want, err := o.fingerprints(ctx, signingKeys)
			if err != nil {
				return err
			}
			if !stringSetEqual(v.SigningKeyFingerprints, want) {
				return fmt.Errorf("party signing keys do not match the spec")
			}
			// SigningThreshold is only checked when the backend reports it
			// (>0); some backends do not expose it on a decoded mapping. The
			// threshold is set at prepare time from the spec regardless.
			if v.SigningThreshold != 0 && v.SigningThreshold != o.spec.Threshold {
				return fmt.Errorf("party signing threshold %d does not match spec %d", v.SigningThreshold, o.spec.Threshold)
			}
		}
		return nil
	})...)
	for _, owner := range o.spec.Owners {
		steps = append(steps, step{
			key:   prefix + "/consent/" + owner.ID,
			owner: owner.ID,
			ready: func(ctx context.Context) (bool, error) {
				return o.allDone([]string{prefix + "/prepare"})
			},
			run: func(ctx context.Context) (any, error) {
				tx, err := o.prepared(prefix + "/prepare")
				if err != nil {
					return nil, err
				}
				return o.topo.Consent(ctx, tx)
			},
		})
	}
	steps = append(steps, step{
		key:   prefix + "/submit",
		owner: o.spec.Coordinator,
		ready: func(ctx context.Context) (bool, error) {
			sigs, err := o.signatures(prefix)
			if err != nil {
				return false, err
			}
			if len(sigs) < o.spec.Threshold {
				return false, nil
			}
			consents, err := o.consents(prefix)
			if err != nil {
				return false, err
			}
			return len(consents) == len(o.spec.Owners), nil
		},
		run: func(ctx context.Context) (any, error) {
			consents, err := o.consents(prefix)
			if err != nil {
				return nil, err
			}
			return o.submit(ctx, prefix, consents)
		},
	})
	return steps
}

// signSteps produces one authorization-signature step per owner over the
// PreparedTx recorded at <prefix>/prepare. Each step re-derives and checks the
// transaction against `expect` before signing.
func (o *Onboarding) signSteps(prefix string, expect func(context.Context, TxView) error) []step {
	var steps []step
	for _, owner := range o.spec.Owners {
		steps = append(steps, step{
			key:   prefix + "/sign/" + owner.ID,
			owner: owner.ID,
			ready: func(ctx context.Context) (bool, error) {
				return o.allDone([]string{prefix + "/prepare"})
			},
			run: func(ctx context.Context) (any, error) {
				tx, err := o.prepared(prefix + "/prepare")
				if err != nil {
					return nil, err
				}
				if err := o.verify(ctx, tx, expect); err != nil {
					return nil, err
				}
				sig, err := o.signer.Sign(tx.HashHex)
				if err != nil {
					return nil, err
				}
				self, _ := o.spec.Owner(o.actor)
				fp, err := o.topo.Fingerprint(ctx, self.PublicKeyDER)
				if err != nil {
					return nil, err
				}
				return OwnerSignature{OwnerID: o.actor, Fingerprint: fp, SigHex: sig}, nil
			},
		})
	}
	return steps
}

// verify decodes tx (which re-derives and checks its hash) and applies the
// caller's spec check. Signing proceeds only if both pass.
func (o *Onboarding) verify(ctx context.Context, tx PreparedTx, expect func(context.Context, TxView) error) error {
	view, err := o.topo.Describe(ctx, tx)
	if err != nil {
		return fmt.Errorf("refusing to sign: %w", err)
	}
	if err := expect(ctx, view); err != nil {
		return fmt.Errorf("refusing to sign: %w", err)
	}
	return nil
}

func (o *Onboarding) submit(ctx context.Context, prefix string, consents []string) (any, error) {
	tx, err := o.prepared(prefix + "/prepare")
	if err != nil {
		return nil, err
	}
	sigs, err := o.signatures(prefix)
	if err != nil {
		return nil, err
	}
	if err := o.topo.Submit(ctx, tx, sigs, consents); err != nil {
		return nil, err
	}
	return true, nil
}

func (o *Onboarding) buildArtifacts() (Artifacts, error) {
	dns, err := o.prepared("dns/prepare")
	if err != nil {
		return Artifacts{}, err
	}
	gov, err := o.prepared("host/governance/prepare")
	if err != nil {
		return Artifacts{}, err
	}
	obs, err := o.prepared("host/observer/prepare")
	if err != nil {
		return Artifacts{}, err
	}
	hosts, err := o.hosts(Confirmation)
	if err != nil {
		return Artifacts{}, err
	}
	participants := make([]string, len(hosts))
	for i, h := range hosts {
		participants[i] = h.ParticipantUID
	}
	return Artifacts{
		Namespace:         dns.Namespace,
		GovernancePartyID: gov.PartyID,
		ObserverPartyID:   obs.PartyID,
		Threshold:         o.spec.Threshold,
		Participants:      participants,
	}, nil
}

// --- store accessors ---

func (o *Onboarding) done(key string) (bool, error) {
	var raw json.RawMessage
	return o.store.Get(key, &raw)
}

func (o *Onboarding) allDone(keyGroups ...[]string) (bool, error) {
	for _, group := range keyGroups {
		for _, key := range group {
			done, err := o.done(key)
			if err != nil || !done {
				return false, err
			}
		}
	}
	return true, nil
}

func (o *Onboarding) perOwnerKeys(prefix string) []string {
	keys := make([]string, len(o.spec.Owners))
	for i, owner := range o.spec.Owners {
		keys[i] = prefix + "/" + owner.ID
	}
	return keys
}

func (o *Onboarding) prepared(key string) (PreparedTx, error) {
	var tx PreparedTx
	ok, err := o.store.Get(key, &tx)
	if err != nil {
		return PreparedTx{}, err
	}
	if !ok {
		return PreparedTx{}, fmt.Errorf("prepared transaction %q not yet recorded", key)
	}
	return tx, nil
}

func (o *Onboarding) signatures(prefix string) ([]OwnerSignature, error) {
	var sigs []OwnerSignature
	for _, owner := range o.spec.Owners {
		var sig OwnerSignature
		ok, err := o.store.Get(prefix+"/sign/"+owner.ID, &sig)
		if err != nil {
			return nil, err
		}
		if ok {
			sigs = append(sigs, sig)
		}
	}
	return sigs, nil
}

func (o *Onboarding) consents(prefix string) ([]string, error) {
	var consents []string
	for _, owner := range o.spec.Owners {
		var blob string
		ok, err := o.store.Get(prefix+"/consent/"+owner.ID, &blob)
		if err != nil {
			return nil, err
		}
		if ok {
			consents = append(consents, blob)
		}
	}
	return consents, nil
}

// hosts assembles the hosting entries from every owner's recorded participant
// uid, at the given permission, deduplicated by uid: a PartyToParticipant
// mapping lists each participant once, even when several owners are backed by
// the same participant (as in a single-node test).
func (o *Onboarding) hosts(permission Permission) ([]Host, error) {
	hosts := make([]Host, 0, len(o.spec.Owners))
	seen := map[string]bool{}
	for _, owner := range o.spec.Owners {
		var uid string
		ok, err := o.store.Get("identify/"+owner.ID, &uid)
		if err != nil {
			return nil, err
		}
		if !ok {
			return nil, fmt.Errorf("participant uid for owner %q not yet recorded", owner.ID)
		}
		if seen[uid] {
			continue
		}
		seen[uid] = true
		hosts = append(hosts, Host{ParticipantUID: uid, Permission: permission})
	}
	return hosts, nil
}

// fingerprints maps each DER key to its Canton namespace fingerprint via the
// topology backend, so spec keys can be compared against a decoded
// transaction's fingerprint references.
func (o *Onboarding) fingerprints(ctx context.Context, keys [][]byte) ([]string, error) {
	fps := make([]string, len(keys))
	for i, k := range keys {
		fp, err := o.topo.Fingerprint(ctx, k)
		if err != nil {
			return nil, err
		}
		fps[i] = fp
	}
	return fps, nil
}

// stringSetEqual reports set equality of two string slices (no duplicates
// tolerated beyond matching).
func stringSetEqual(a, b []string) bool {
	if len(a) != len(b) {
		return false
	}
	used := make([]bool, len(b))
	for _, x := range a {
		found := false
		for i, y := range b {
			if !used[i] && x == y {
				used[i] = true
				found = true
				break
			}
		}
		if !found {
			return false
		}
	}
	return true
}

func allHostsPermission(hosts []Host, permission Permission) bool {
	if len(hosts) == 0 {
		return false
	}
	for _, h := range hosts {
		if h.Permission != permission {
			return false
		}
	}
	return true
}
