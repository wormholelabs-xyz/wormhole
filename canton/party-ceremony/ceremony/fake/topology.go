// Package fake provides in-memory implementations of the ceremony ports that
// carry real business logic rather than stubbed answers: the Ledger enforces
// Canton's topology authorization rules (rooted namespaces, key-bound and
// immutable decentralized namespaces, all-owners namespace creation, threshold
// party authorization, per-host consent) and verifies real Ed25519 signatures.
// Submit treats its decoded input as untrusted and re-derives every security
// property, so tests exercise the same rejections a real synchronizer would.
package fake

import (
	"context"
	"crypto/ed25519"
	"crypto/sha256"
	"crypto/x509"
	"encoding/base64"
	"encoding/hex"
	"encoding/json"
	"fmt"
	"sort"
	"strings"
	"sync"

	"github.com/wormhole-foundation/wormhole/canton/party-ceremony/ceremony"
)

// Ledger is the shared topology state all fake participants submit to — the
// stand-in for the synchronizer's topology store.
type Ledger struct {
	mu           sync.Mutex
	participants map[string]bool
	// delegated namespaces: owner-key fingerprint -> public key DER
	namespaces map[string][]byte
	// decentralized namespaces: fingerprint -> definition
	dns map[string]DNSRecord
	// parties: party id -> record
	parties map[string]PartyRecord
	// consents issued by participants, keyed "uid|txHash" — a consent counts
	// only if the hosting participant itself recorded it here.
	consents map[string]bool
	// submitted transaction hashes, for idempotent re-submits
	submitted map[string]bool
}

// DNSRecord is a committed DecentralizedNamespaceDefinition.
type DNSRecord struct {
	Namespace string
	OwnerFPs  []string
	Threshold int
}

// PartyRecord is a committed PartyToParticipant mapping.
type PartyRecord struct {
	ID                    string
	Namespace             string
	Hosts                 map[string]ceremony.Permission
	ConfirmationThreshold int
	SigningKeysDER        [][]byte
	SigningThreshold      int
}

// NewLedger creates an empty shared topology state.
func NewLedger() *Ledger {
	return &Ledger{
		participants: map[string]bool{},
		namespaces:   map[string][]byte{},
		dns:          map[string]DNSRecord{},
		parties:      map[string]PartyRecord{},
		consents:     map[string]bool{},
		submitted:    map[string]bool{},
	}
}

// DNS returns a committed decentralized namespace, if present.
func (l *Ledger) DNS(namespace string) (DNSRecord, bool) {
	l.mu.Lock()
	defer l.mu.Unlock()
	rec, ok := l.dns[namespace]
	return rec, ok
}

// Party returns a committed party, if present.
func (l *Ledger) Party(id string) (PartyRecord, bool) {
	l.mu.Lock()
	defer l.mu.Unlock()
	rec, ok := l.parties[id]
	return rec, ok
}

// Snapshot serializes the whole ledger state; Restore loads it. Together they
// let the CLI persist the fake across separate process invocations.
func (l *Ledger) Snapshot() ([]byte, error) {
	l.mu.Lock()
	defer l.mu.Unlock()
	return json.Marshal(ledgerState{
		Participants: l.participants,
		Namespaces:   l.namespaces,
		DNS:          l.dns,
		Parties:      l.parties,
		Consents:     l.consents,
		Submitted:    l.submitted,
	})
}

// Restore replaces the ledger state with a snapshot.
func (l *Ledger) Restore(data []byte) error {
	var s ledgerState
	if err := json.Unmarshal(data, &s); err != nil {
		return fmt.Errorf("fake ledger: restoring snapshot: %w", err)
	}
	l.mu.Lock()
	defer l.mu.Unlock()
	l.participants = orEmpty(s.Participants)
	l.namespaces = orEmptyB(s.Namespaces)
	l.dns = orEmptyD(s.DNS)
	l.parties = orEmptyP(s.Parties)
	l.consents = orEmpty(s.Consents)
	l.submitted = orEmpty(s.Submitted)
	return nil
}

type ledgerState struct {
	Participants map[string]bool        `json:"participants"`
	Namespaces   map[string][]byte      `json:"namespaces"`
	DNS          map[string]DNSRecord   `json:"dns"`
	Parties      map[string]PartyRecord `json:"parties"`
	Consents     map[string]bool        `json:"consents"`
	Submitted    map[string]bool        `json:"submitted"`
}

func orEmpty(m map[string]bool) map[string]bool {
	if m == nil {
		return map[string]bool{}
	}
	return m
}
func orEmptyB(m map[string][]byte) map[string][]byte {
	if m == nil {
		return map[string][]byte{}
	}
	return m
}
func orEmptyD(m map[string]DNSRecord) map[string]DNSRecord {
	if m == nil {
		return map[string]DNSRecord{}
	}
	return m
}
func orEmptyP(m map[string]PartyRecord) map[string]PartyRecord {
	if m == nil {
		return map[string]PartyRecord{}
	}
	return m
}

// Participant is one actor's fake ceremony.Topology, bound to a uid on the
// shared ledger.
type Participant struct {
	ledger *Ledger
	uid    string
}

// NewParticipant registers a participant uid on the ledger and returns its
// topology port.
func NewParticipant(ledger *Ledger, uid string) *Participant {
	ledger.mu.Lock()
	ledger.participants[uid] = true
	ledger.mu.Unlock()
	return &Participant{ledger: ledger, uid: uid}
}

// ParticipantID implements ceremony.Topology.
func (p *Participant) ParticipantID(context.Context) (string, error) {
	return p.uid, nil
}

// Fingerprint implements ceremony.Topology: the fake's key fingerprint is the
// sha256-hex of the DER key, consistent with everything else in this package.
func (p *Participant) Fingerprint(_ context.Context, pubDER []byte) (string, error) {
	if len(pubDER) == 0 {
		return "", fmt.Errorf("fake topology: empty key")
	}
	return Fingerprint(pubDER), nil
}

// --- transaction payloads (the fake's wire format) ---

type txPayload struct {
	Type                  string          `json:"type"`
	OwnerPubs             [][]byte        `json:"ownerPubs,omitempty"`
	Threshold             int             `json:"threshold,omitempty"`
	Namespace             string          `json:"namespace,omitempty"`
	PartyName             string          `json:"partyName,omitempty"`
	PartyID               string          `json:"partyId,omitempty"`
	Hosts                 []ceremony.Host `json:"hosts,omitempty"`
	ConfirmationThreshold int             `json:"confirmationThreshold,omitempty"`
	SigningKeys           [][]byte        `json:"signingKeys,omitempty"`
	SigningThreshold      int             `json:"signingThreshold,omitempty"`
}

func encodeTx(p txPayload) (ceremony.PreparedTx, error) {
	raw, err := json.Marshal(p)
	if err != nil {
		return ceremony.PreparedTx{}, err
	}
	digest := sha256.Sum256(raw)
	return ceremony.PreparedTx{
		TxBase64:  base64.StdEncoding.EncodeToString(raw),
		HashHex:   hex.EncodeToString(digest[:]),
		Namespace: p.Namespace,
		PartyID:   p.PartyID,
	}, nil
}

// decodeTx re-derives the hash from the transaction body and rejects any
// (body, hash) pair that does not match — the tamper check the workflow relies
// on before signing.
func decodeTx(tx ceremony.PreparedTx) (txPayload, error) {
	raw, err := base64.StdEncoding.DecodeString(tx.TxBase64)
	if err != nil {
		return txPayload{}, fmt.Errorf("fake topology: undecodable transaction: %w", err)
	}
	digest := sha256.Sum256(raw)
	if hex.EncodeToString(digest[:]) != tx.HashHex {
		return txPayload{}, fmt.Errorf("fake topology: transaction hash does not match its body")
	}
	var p txPayload
	if err := json.Unmarshal(raw, &p); err != nil {
		return txPayload{}, fmt.Errorf("fake topology: unparseable transaction: %w", err)
	}
	return p, nil
}

// Fingerprint is the fake's key fingerprint: hex sha256 of the DER key.
func Fingerprint(pubDER []byte) string {
	d := sha256.Sum256(pubDER)
	return hex.EncodeToString(d[:])
}

// namespaceOf computes the decentralized-namespace fingerprint from the
// owners' key fingerprints (order-independent, like Canton's computeNamespace).
func namespaceOf(ownerPubs [][]byte) string {
	fps := make([]string, len(ownerPubs))
	for i, pub := range ownerPubs {
		fps[i] = Fingerprint(pub)
	}
	sort.Strings(fps)
	d := sha256.Sum256([]byte(strings.Join(fps, "|")))
	return hex.EncodeToString(d[:])
}

// PrepareRootDelegation implements ceremony.Topology.
func (p *Participant) PrepareRootDelegation(_ context.Context, ownerPubDER []byte) (ceremony.PreparedTx, error) {
	if len(ownerPubDER) == 0 {
		return ceremony.PreparedTx{}, fmt.Errorf("fake topology: empty owner key")
	}
	return encodeTx(txPayload{Type: "delegation", OwnerPubs: [][]byte{ownerPubDER}})
}

// PrepareDecentralizedNamespace implements ceremony.Topology.
func (p *Participant) PrepareDecentralizedNamespace(_ context.Context, ownerPubsDER [][]byte, threshold int) (ceremony.PreparedTx, error) {
	if threshold < 1 || threshold > len(ownerPubsDER) {
		return ceremony.PreparedTx{}, fmt.Errorf("fake topology: threshold %d out of range", threshold)
	}
	return encodeTx(txPayload{
		Type:      "dns",
		OwnerPubs: ownerPubsDER,
		Threshold: threshold,
		Namespace: namespaceOf(ownerPubsDER),
	})
}

// PreparePartyHosting implements ceremony.Topology.
func (p *Participant) PreparePartyHosting(_ context.Context, spec ceremony.HostingSpec) (ceremony.PreparedTx, error) {
	if spec.PartyName == "" || spec.Namespace == "" || len(spec.Hosts) == 0 {
		return ceremony.PreparedTx{}, fmt.Errorf("fake topology: incomplete hosting spec")
	}
	return encodeTx(txPayload{
		Type:                  "hosting",
		Namespace:             spec.Namespace,
		PartyName:             spec.PartyName,
		PartyID:               spec.PartyName + "::" + spec.Namespace,
		Hosts:                 spec.Hosts,
		ConfirmationThreshold: spec.ConfirmationThreshold,
		SigningKeys:           spec.SigningKeysDER,
		SigningThreshold:      spec.SigningThreshold,
	})
}

// Describe implements ceremony.Topology: decode (with hash re-derivation) into
// the security-relevant view the workflow checks before signing.
func (p *Participant) Describe(_ context.Context, tx ceremony.PreparedTx) (ceremony.TxView, error) {
	payload, err := decodeTx(tx)
	if err != nil {
		return ceremony.TxView{}, err
	}
	fps := func(keys [][]byte) []string {
		out := make([]string, len(keys))
		for i, k := range keys {
			out[i] = Fingerprint(k)
		}
		return out
	}
	switch payload.Type {
	case "delegation":
		return ceremony.TxView{Kind: ceremony.DelegationTx, OwnerFingerprints: fps(payload.OwnerPubs)}, nil
	case "dns":
		return ceremony.TxView{Kind: ceremony.NamespaceTx, OwnerFingerprints: fps(payload.OwnerPubs), Threshold: payload.Threshold}, nil
	case "hosting":
		return ceremony.TxView{
			Kind:                   ceremony.HostingTx,
			PartyName:              payload.PartyName,
			Hosts:                  payload.Hosts,
			SigningKeyFingerprints: fps(payload.SigningKeys),
			SigningThreshold:       payload.SigningThreshold,
		}, nil
	default:
		return ceremony.TxView{}, fmt.Errorf("fake topology: unknown transaction type %q", payload.Type)
	}
}

// Consent implements ceremony.Topology: this participant records and returns
// its consent-to-host. Recording it on the ledger is what makes it
// unforgeable — Submit accepts a consent only if the hosting participant
// itself issued it here.
func (p *Participant) Consent(_ context.Context, tx ceremony.PreparedTx) (string, error) {
	payload, err := decodeTx(tx)
	if err != nil {
		return "", err
	}
	if payload.Type != "hosting" {
		return "", fmt.Errorf("fake topology: consent over a %q transaction", payload.Type)
	}
	p.ledger.mu.Lock()
	p.ledger.consents[p.uid+"|"+tx.HashHex] = true
	p.ledger.mu.Unlock()
	return "consent|" + p.uid + "|" + tx.HashHex, nil
}

// Submit implements ceremony.Topology, enforcing the authorization rules a
// real synchronizer would. It trusts nothing in the decoded payload: it
// re-derives the namespace from the owner keys, rejects redefinition,
// validates the threshold, counts DISTINCT verified owner keys, and accepts a
// consent only if the hosting participant recorded it via Consent.
func (p *Participant) Submit(_ context.Context, tx ceremony.PreparedTx, ownerSigs []ceremony.OwnerSignature, consents []string) error {
	payload, err := decodeTx(tx)
	if err != nil {
		return err
	}
	hashBytes, err := hex.DecodeString(tx.HashHex)
	if err != nil {
		return fmt.Errorf("fake topology: bad hash hex: %w", err)
	}

	p.ledger.mu.Lock()
	defer p.ledger.mu.Unlock()
	if p.ledger.submitted[tx.HashHex] {
		return nil // idempotent re-submit
	}

	verified := func(pub []byte) bool {
		key, err := x509.ParsePKIXPublicKey(pub)
		if err != nil {
			return false
		}
		edKey, ok := key.(ed25519.PublicKey)
		if !ok {
			return false
		}
		for _, sig := range ownerSigs {
			raw, err := hex.DecodeString(sig.SigHex)
			if err != nil {
				continue
			}
			if ed25519.Verify(edKey, hashBytes, raw) {
				return true
			}
		}
		return false
	}
	// countVerified counts DISTINCT owner keys with a valid signature, so
	// duplicate entries can never inflate the count past the real key set.
	countVerified := func(pubs [][]byte) int {
		seen := map[string]bool{}
		for _, pub := range pubs {
			fp := Fingerprint(pub)
			if seen[fp] {
				continue
			}
			if verified(pub) {
				seen[fp] = true
			}
		}
		return len(seen)
	}

	switch payload.Type {
	case "delegation":
		if len(payload.OwnerPubs) != 1 {
			return fmt.Errorf("fake topology: delegation must carry exactly one key")
		}
		pub := payload.OwnerPubs[0]
		if !verified(pub) {
			return fmt.Errorf("fake topology: root delegation not signed by its own key")
		}
		p.ledger.namespaces[Fingerprint(pub)] = pub

	case "dns":
		if len(payload.OwnerPubs) == 0 {
			return fmt.Errorf("fake topology: namespace has no owners")
		}
		if payload.Threshold < 1 || payload.Threshold > len(payload.OwnerPubs) {
			return fmt.Errorf("fake topology: namespace threshold %d out of range 1..%d", payload.Threshold, len(payload.OwnerPubs))
		}
		fps := make([]string, len(payload.OwnerPubs))
		distinct := map[string]bool{}
		for i, pub := range payload.OwnerPubs {
			fp := Fingerprint(pub)
			if distinct[fp] {
				return fmt.Errorf("fake topology: duplicate owner key in namespace")
			}
			distinct[fp] = true
			fps[i] = fp
			if _, ok := p.ledger.namespaces[fp]; !ok {
				return fmt.Errorf("fake topology: owner namespace %s has no root delegation", fp[:8])
			}
		}
		// The namespace id MUST be the hash of the owner set — it cannot be
		// chosen (prevents squatting on another set's namespace).
		if payload.Namespace != namespaceOf(payload.OwnerPubs) {
			return fmt.Errorf("fake topology: namespace id is not derived from its owner keys")
		}
		if existing, ok := p.ledger.dns[payload.Namespace]; ok {
			if !sameFingerprints(existing.OwnerFPs, fps) {
				return fmt.Errorf("fake topology: namespace %s already defined with a different owner set", payload.Namespace[:8])
			}
			// Same owner set already defined; nothing to change.
		}
		if n := countVerified(payload.OwnerPubs); n < len(payload.OwnerPubs) {
			return fmt.Errorf("fake topology: namespace creation needs all %d owner signatures, got %d valid", len(payload.OwnerPubs), n)
		}
		p.ledger.dns[payload.Namespace] = DNSRecord{Namespace: payload.Namespace, OwnerFPs: fps, Threshold: payload.Threshold}

	case "hosting":
		dns, ok := p.ledger.dns[payload.Namespace]
		if !ok {
			return fmt.Errorf("fake topology: unknown decentralized namespace %s", payload.Namespace)
		}
		if len(payload.Hosts) == 0 {
			return fmt.Errorf("fake topology: hosting has no participants")
		}
		ownerPubs := make([][]byte, 0, len(dns.OwnerFPs))
		for _, fp := range dns.OwnerFPs {
			ownerPubs = append(ownerPubs, p.ledger.namespaces[fp])
		}
		if n := countVerified(ownerPubs); n < dns.Threshold {
			return fmt.Errorf("fake topology: party authorization needs %d owner signatures, got %d valid", dns.Threshold, n)
		}
		// A consent counts only if it was passed in AND the named participant
		// itself recorded it via Consent — a fabricated blob is not in the
		// ledger's issued set, so it cannot host a non-consenting participant.
		consentBy := map[string]bool{}
		for _, blob := range consents {
			parts := strings.SplitN(blob, "|", 3)
			if len(parts) != 3 || parts[0] != "consent" || parts[2] != tx.HashHex {
				return fmt.Errorf("fake topology: malformed consent %q", blob)
			}
			uid := parts[1]
			if !p.ledger.consents[uid+"|"+tx.HashHex] {
				return fmt.Errorf("fake topology: consent for %q was never issued by that participant", uid)
			}
			consentBy[uid] = true
		}
		hosts := map[string]ceremony.Permission{}
		for _, h := range payload.Hosts {
			if !p.ledger.participants[h.ParticipantUID] {
				return fmt.Errorf("fake topology: hosting an unknown participant %q", h.ParticipantUID)
			}
			if !consentBy[h.ParticipantUID] {
				return fmt.Errorf("fake topology: hosting participant %q did not consent", h.ParticipantUID)
			}
			hosts[h.ParticipantUID] = h.Permission
		}
		p.ledger.parties[payload.PartyID] = PartyRecord{
			ID:                    payload.PartyID,
			Namespace:             payload.Namespace,
			Hosts:                 hosts,
			ConfirmationThreshold: payload.ConfirmationThreshold,
			SigningKeysDER:        payload.SigningKeys,
			SigningThreshold:      payload.SigningThreshold,
		}

	default:
		return fmt.Errorf("fake topology: unknown transaction type %q", payload.Type)
	}

	p.ledger.submitted[tx.HashHex] = true
	return nil
}

// sameFingerprints reports set equality of two fingerprint lists.
func sameFingerprints(a, b []string) bool {
	if len(a) != len(b) {
		return false
	}
	as := append([]string(nil), a...)
	bs := append([]string(nil), b...)
	sort.Strings(as)
	sort.Strings(bs)
	for i := range as {
		if as[i] != bs[i] {
			return false
		}
	}
	return true
}
