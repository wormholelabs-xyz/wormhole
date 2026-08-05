package ceremony

import "context"

// PreparedTx is an unsigned topology transaction produced on one participant
// and signed by owners on other machines at other times. TxBase64 is an
// adapter-opaque serialization; HashHex is what owners actually sign. It is
// stored in the shared report store so signatures can be collected across
// sessions.
//
// Because the store travels between operators (in production, through a Git
// repository), a PreparedTx read back from it is UNTRUSTED input: before any
// owner signs HashHex, the workflow re-derives it from TxBase64 and checks the
// decoded transaction against the spec via Topology.Describe. HashHex on its
// own is never trusted.
type PreparedTx struct {
	TxBase64  string `json:"txBase64"`
	HashHex   string `json:"hashHex"`
	Namespace string `json:"namespace,omitempty"` // decentralized-namespace fingerprint, set by PrepareDecentralizedNamespace
	PartyID   string `json:"partyId,omitempty"`   // set by PreparePartyHosting
}

// OwnerSignature is one owner's signature over a PreparedTx hash. The raw
// signature travels as hex, alongside the signer's key fingerprint so the
// topology backend can attach it to the transaction under the right key.
type OwnerSignature struct {
	OwnerID     string `json:"ownerId"`
	Fingerprint string `json:"fingerprint"`
	SigHex      string `json:"sigHex"`
}

// Permission is the hosting permission a participant grants a party.
type Permission string

const (
	// Confirmation lets the hosting participant confirm transactions for the
	// party (used by guardianGovernance).
	Confirmation Permission = "Confirmation"
	// Observation is read-only hosting (used by guardianObserver).
	Observation Permission = "Observation"
)

// Host is one hosting entry of a PartyToParticipant mapping.
type Host struct {
	ParticipantUID string     `json:"participantUid"`
	Permission     Permission `json:"permission"`
}

// TxKind identifies what a prepared transaction does, so the workflow can
// check a store-loaded transaction against the spec before signing it.
type TxKind string

const (
	// DelegationTx is one owner's root NamespaceDelegation.
	DelegationTx TxKind = "delegation"
	// NamespaceTx is the DecentralizedNamespaceDefinition.
	NamespaceTx TxKind = "namespace"
	// HostingTx is a PartyToParticipant mapping.
	HostingTx TxKind = "hosting"
)

// TxView is a decoded, hash-verified description of a PreparedTx: the
// security-relevant fields the workflow confirms against the spec before an
// owner signs. Adapters populate only the fields relevant to Kind.
//
// Namespace ownership is expressed by key FINGERPRINT, not raw key, because
// that is what Canton's DecentralizedNamespaceDefinition records — a decoded
// namespace transaction references owner namespaces, never the DER keys. The
// workflow maps its spec keys to fingerprints via Topology.Fingerprint to
// compare, so the check works identically against the fake and real Canton.
type TxView struct {
	Kind                   TxKind
	OwnerFingerprints      []string // DelegationTx: the single delegated key's fingerprint; NamespaceTx: all owner fingerprints
	Threshold              int      // NamespaceTx: the namespace threshold
	PartyName              string   // HostingTx: the party's name
	Hosts                  []Host   // HostingTx: the hosting entries
	SigningKeyFingerprints []string // HostingTx: the party's signing-key fingerprints (empty for a keyless party)
	SigningThreshold       int      // HostingTx: the party's signing threshold
}

// HostingSpec describes a party to allocate under the decentralized
// namespace: its name, where it is hosted, and (for an external party) the
// threshold signing keys. SigningKeysDER nil means the party holds no keys of
// its own (a purely observing party).
type HostingSpec struct {
	PartyName             string   `json:"partyName"`
	Namespace             string   `json:"namespace"`
	Hosts                 []Host   `json:"hosts"`
	ConfirmationThreshold int      `json:"confirmationThreshold"`
	SigningKeysDER        [][]byte `json:"signingKeysDer,omitempty"`
	SigningThreshold      int      `json:"signingThreshold,omitempty"`
}

// Topology is each actor's window onto its own Canton participant. Prepare*
// methods build unsigned transactions, Describe decodes one for pre-sign
// verification, Consent produces this participant's consent-to-host, and
// Submit loads a fully signed transaction onto the synchronizer.
// Implementations: the in-memory fake (tests), and eventually a console /
// Admin API gRPC client.
//
// Contract relied on by the workflow:
//   - Prepare* are deterministic: the same inputs produce a byte-identical
//     TxBase64 and HashHex. (Canton topology transactions are content
//     addressed, so this holds; the workflow re-prepares the delegation on
//     resume and depends on it.)
//   - Describe re-derives the hash from TxBase64 and fails if it does not equal
//     HashHex, so a tampered (body, hash) pair is rejected before signing.
//   - Submit is idempotent: submitting the same transaction twice is a no-op,
//     so a resume after a crash between Submit and Store.Put is safe.
type Topology interface {
	// ParticipantID returns the unique id of the participant this actor
	// operates, used as the hosting target in PartyToParticipant mappings.
	ParticipantID(ctx context.Context) (string, error)
	// Fingerprint returns the namespace fingerprint Canton derives from an
	// Ed25519 public key (DER), so the workflow can map its spec keys to the
	// fingerprints a decoded namespace transaction references.
	Fingerprint(ctx context.Context, pubDER []byte) (string, error)
	// PrepareRootDelegation builds the self-signed root NamespaceDelegation
	// for one owner key (the namespace's root of trust).
	PrepareRootDelegation(ctx context.Context, ownerPubDER []byte) (PreparedTx, error)
	// PrepareDecentralizedNamespace builds the DecentralizedNamespaceDefinition
	// over all owner namespaces with the given threshold.
	PrepareDecentralizedNamespace(ctx context.Context, ownerPubsDER [][]byte, threshold int) (PreparedTx, error)
	// PreparePartyHosting builds the PartyToParticipant mapping for a party
	// under the decentralized namespace.
	PreparePartyHosting(ctx context.Context, spec HostingSpec) (PreparedTx, error)
	// Describe decodes a prepared transaction and verifies its hash, returning
	// the fields the workflow checks against the spec before signing. It fails
	// if HashHex is not the hash of TxBase64.
	Describe(ctx context.Context, tx PreparedTx) (TxView, error)
	// Consent produces this participant's consent-to-host over the prepared
	// PartyToParticipant transaction, as an adapter-opaque blob.
	Consent(ctx context.Context, tx PreparedTx) (string, error)
	// Submit loads the transaction with the collected owner signatures and
	// hosting consents. Submitting the same transaction twice is a no-op.
	Submit(ctx context.Context, tx PreparedTx, ownerSigs []OwnerSignature, consents []string) error
}

// Signer signs a topology-transaction hash on behalf of one owner. This is
// the custody boundary: implementations range from an in-process Ed25519 key
// (tests, local dev) to an external command wrapping an HSM/KMS signer.
type Signer interface {
	Sign(hashHex string) (sigHex string, err error)
}

// Store persists ceremony progress as an append-only map of operation key to
// result. Put of an identical value is idempotent; Put of a different value
// for an existing key is a conflict and must fail — history is never
// rewritten. Equality is by canonical JSON, so formatting differences between
// implementations never cause a false conflict.
type Store interface {
	Put(key string, value any) error
	// Get unmarshals the stored value into `into` and reports presence.
	Get(key string, into any) (bool, error)
}
