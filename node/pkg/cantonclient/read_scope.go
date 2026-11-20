package cantonclient

import (
	"errors"
	"fmt"
	"strings"
)

const (
	partyIDDelimiter = "::"
	// Canton UniqueIdentifier identifier: String185.
	maxPartyIDHintLen = 185
	// Multihash of a SHA-256 digest: 0x12 (sha2-256), 0x20 (32 bytes).
	partyIDFingerprintPrefix = "1220"
	partyIDFingerprintLen    = len(partyIDFingerprintPrefix) + 64
)

// PartyID is a Canton party id `<hint>::<fingerprint>`. The fingerprint is a
// namespace fingerprint in the form Canton generates: 1220 + 64 lowercase hex.
// The hint is 1 to 185 characters of [A-Za-z0-9_-].
type PartyID struct {
	text string
}

// ParsePartyID accepts exactly one Canton party id.
func ParsePartyID(text string) (PartyID, error) {
	delimiterIndex := strings.Index(text, partyIDDelimiter)
	if delimiterIndex < 0 {
		return PartyID{}, fmt.Errorf("cantonclient: party id %q has no '::'", text)
	}
	hint := text[:delimiterIndex]
	fingerprint := text[delimiterIndex+len(partyIDDelimiter):]
	if err := checkPartyIDHint(hint); err != nil {
		return PartyID{}, fmt.Errorf("cantonclient: party id %q: %w", text, err)
	}
	if err := checkPartyIDFingerprint(fingerprint); err != nil {
		return PartyID{}, fmt.Errorf("cantonclient: party id %q: %w", text, err)
	}
	partyID := PartyID{text: text}
	if partyID.String() != hint+partyIDDelimiter+fingerprint {
		panic("cantonclient: party id does not round-trip")
	}
	return partyID, nil
}

func checkPartyIDHint(hint string) error {
	if len(hint) < 1 || len(hint) > maxPartyIDHintLen {
		return fmt.Errorf("hint length %d is outside [1, %d]", len(hint), maxPartyIDHintLen)
	}
	for _, character := range hint {
		isAllowed := (character >= 'a' && character <= 'z') ||
			(character >= 'A' && character <= 'Z') ||
			(character >= '0' && character <= '9') ||
			character == '_' || character == '-'
		if !isAllowed {
			return fmt.Errorf("hint character %q is not in [A-Za-z0-9_-]", character)
		}
	}
	return nil
}

func checkPartyIDFingerprint(fingerprint string) error {
	if len(fingerprint) != partyIDFingerprintLen {
		return fmt.Errorf("fingerprint length %d is not %d", len(fingerprint), partyIDFingerprintLen)
	}
	if !strings.HasPrefix(fingerprint, partyIDFingerprintPrefix) {
		return fmt.Errorf("fingerprint prefix is not %s (SHA-256 multihash)", partyIDFingerprintPrefix)
	}
	for _, character := range fingerprint {
		isLowercaseHex := (character >= '0' && character <= '9') || (character >= 'a' && character <= 'f')
		if !isLowercaseHex {
			return fmt.Errorf("fingerprint character %q is not lowercase hex", character)
		}
	}
	return nil
}

// String is the party id as the Ledger API carries it.
func (p PartyID) String() string {
	return p.text
}

type readScopeKind uint8

const (
	readScopeKindNotConstructed readScopeKind = iota
	readScopeKindGuardianGovernance
	readScopeKindAnyPartyUnsafeDevMode
)

// ReadScope is the party set the watcher reads: the guardianGovernance party,
// or every party on the participant (unsafe dev mode only).
type ReadScope struct {
	kind                    readScopeKind
	guardianGovernanceParty PartyID
}

var errReadScopeNotConstructed = errors.New("cantonclient: read scope not constructed")

// ReadAsGuardianGovernance narrows reads to party.
func ReadAsGuardianGovernance(party PartyID) (ReadScope, error) {
	if party == (PartyID{}) {
		return ReadScope{}, errors.New("cantonclient: read scope with a zero PartyID")
	}
	return ReadScope{kind: readScopeKindGuardianGovernance, guardianGovernanceParty: party}, nil
}

// ReadAnyPartyUnsafeDevMode reads every party hosted on the participant.
func ReadAnyPartyUnsafeDevMode(unsafeDevMode bool) (ReadScope, error) {
	if !unsafeDevMode {
		return ReadScope{}, errors.New("cantonclient: reading as any party requires unsafe dev mode; set the guardianGovernance party")
	}
	return ReadScope{kind: readScopeKindAnyPartyUnsafeDevMode}, nil
}

// ParseReadScope maps the read-as party flag to a ReadScope. Empty selects any
// party, which only unsafe dev mode admits.
func ParseReadScope(readAsParty string, unsafeDevMode bool) (ReadScope, error) {
	if readAsParty == "" {
		return ReadAnyPartyUnsafeDevMode(unsafeDevMode)
	}
	party, err := ParsePartyID(readAsParty)
	if err != nil {
		return ReadScope{}, err
	}
	return ReadAsGuardianGovernance(party)
}

// CheckWasConstructed rejects the zero ReadScope.
func (s ReadScope) CheckWasConstructed() error {
	switch s.kind {
	case readScopeKindGuardianGovernance:
		if s.guardianGovernanceParty == (PartyID{}) {
			return errReadScopeNotConstructed
		}
		return nil
	case readScopeKindAnyPartyUnsafeDevMode:
		if s.guardianGovernanceParty != (PartyID{}) {
			return errReadScopeNotConstructed
		}
		return nil
	case readScopeKindNotConstructed:
		return errReadScopeNotConstructed
	default:
		return errReadScopeNotConstructed
	}
}

// GuardianGovernanceParty is the read-as party of a guardianGovernance scope.
func (s ReadScope) GuardianGovernanceParty() (PartyID, bool) {
	if s.kind != readScopeKindGuardianGovernance {
		return PartyID{}, false
	}
	return s.guardianGovernanceParty, true
}

// IsAnyPartyUnsafeDevMode reports the any-party scope.
func (s ReadScope) IsAnyPartyUnsafeDevMode() bool {
	return s.kind == readScopeKindAnyPartyUnsafeDevMode
}

// String is for logs.
func (s ReadScope) String() string {
	switch s.kind {
	case readScopeKindGuardianGovernance:
		return s.guardianGovernanceParty.String()
	case readScopeKindAnyPartyUnsafeDevMode:
		return "any party (unsafe dev mode)"
	case readScopeKindNotConstructed:
		return "not constructed"
	default:
		return "not constructed"
	}
}
