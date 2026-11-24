package cantonclient

import (
	"errors"
	"fmt"
	"strings"
	"unicode/utf8"

	apiv2 "github.com/certusone/wormhole/node/pkg/cantonclient/proto/gen/com/daml/ledger/api/v2"
)

// CoreContractTemplate is one core template whose contracts a non-stakeholder submitter discloses.
type CoreContractTemplate int

const (
	CoreContractTemplateCoreState CoreContractTemplate = iota + 1
	CoreContractTemplateEmitterRegistry
	CoreContractTemplateReplayRootRegistry
	CoreContractTemplateReplayNode
)

// Module names of canton/core/daml/Wormhole/Core/State.daml and Replay.daml.
const (
	coreStateModuleName  = "Wormhole.Core.State"
	coreReplayModuleName = "Wormhole.Core.Replay"
)

// disclosedCoreContractTemplates lists every CoreContractTemplate in filter order.
var disclosedCoreContractTemplates = [...]CoreContractTemplate{
	CoreContractTemplateCoreState,
	CoreContractTemplateEmitterRegistry,
	CoreContractTemplateReplayRootRegistry,
	CoreContractTemplateReplayNode,
}

// singletonCoreContractTemplates lists the disclosed core templates with one live instance per guardianGovernance.
var singletonCoreContractTemplates = [...]CoreContractTemplate{
	CoreContractTemplateCoreState,
	CoreContractTemplateEmitterRegistry,
	CoreContractTemplateReplayRootRegistry,
}

// Compile-time assertion: the singleton templates are every disclosed template except ReplayNode.
var _ = [1]struct{}{}[len(disclosedCoreContractTemplates)-len(singletonCoreContractTemplates)-1]

// ModuleName is the Daml module that declares the template.
func (template CoreContractTemplate) ModuleName() string {
	switch template {
	case CoreContractTemplateCoreState, CoreContractTemplateEmitterRegistry, CoreContractTemplateReplayRootRegistry:
		return coreStateModuleName
	case CoreContractTemplateReplayNode:
		return coreReplayModuleName
	default:
		panic(fmt.Sprintf("cantonclient: unknown core contract template %d", int(template)))
	}
}

// EntityName is the Daml template name.
func (template CoreContractTemplate) EntityName() string {
	switch template {
	case CoreContractTemplateCoreState:
		return "CoreState"
	case CoreContractTemplateEmitterRegistry:
		return "EmitterRegistry"
	case CoreContractTemplateReplayRootRegistry:
		return "ReplayRootRegistry"
	case CoreContractTemplateReplayNode:
		return "ReplayNode"
	default:
		panic(fmt.Sprintf("cantonclient: unknown core contract template %d", int(template)))
	}
}

// String is "Module:Entity".
func (template CoreContractTemplate) String() string {
	return template.ModuleName() + ":" + template.EntityName()
}

// parseCoreContractTemplate maps a module and entity to a CoreContractTemplate.
func parseCoreContractTemplate(moduleName string, entityName string) (CoreContractTemplate, error) {
	for _, template := range disclosedCoreContractTemplates {
		if template.ModuleName() == moduleName && template.EntityName() == entityName {
			return template, nil
		}
	}
	return 0, fmt.Errorf("template %s:%s is not a disclosed core template", moduleName, entityName)
}

// CoreTemplateID is a disclosed core template in an approved core package.
type CoreTemplateID struct {
	packageID PackageID
	template  CoreContractTemplate
}

func (templateID CoreTemplateID) PackageID() PackageID { return templateID.packageID }

func (templateID CoreTemplateID) Template() CoreContractTemplate { return templateID.template }

// String is "packageId:Module:Entity".
func (templateID CoreTemplateID) String() string {
	return templateID.packageID.String() + ":" + templateID.template.String()
}

// parseCoreTemplateID requires an approved package id and a disclosed core template.
func parseCoreTemplateID(identifier *apiv2.Identifier, approved ApprovedCorePackages) (CoreTemplateID, error) {
	if identifier == nil {
		return CoreTemplateID{}, errors.New("missing template id")
	}
	packageID, err := ParsePackageID(identifier.GetPackageId())
	if err != nil {
		return CoreTemplateID{}, fmt.Errorf("template id: %w", err)
	}
	if err := approved.CheckCorePackageIDApproved(packageID); err != nil {
		return CoreTemplateID{}, fmt.Errorf("template id: %w", err)
	}
	template, err := parseCoreContractTemplate(identifier.GetModuleName(), identifier.GetEntityName())
	if err != nil {
		return CoreTemplateID{}, err
	}
	return CoreTemplateID{packageID: packageID, template: template}, nil
}

// Daml-LF V1 contract id: version byte, 32-byte discriminator, suffix of at most 94 bytes.
const (
	contractIDV1VersionPrefix = "00"
	contractIDV2VersionPrefix = "01"
	maxContractIDHexLength    = 254
)

// ContractID is a Daml contract id: lowercase hex of whole bytes with a V1 or V2 version byte.
type ContractID struct {
	hexText string
}

// ParseContractID accepts exactly one contract id.
func ParseContractID(hexText string) (ContractID, error) {
	if len(hexText) > maxContractIDHexLength {
		return ContractID{}, fmt.Errorf("contract id length %d exceeds %d", len(hexText), maxContractIDHexLength)
	}
	if len(hexText)%2 != 0 {
		return ContractID{}, fmt.Errorf("contract id %q has an odd hex length %d", hexText, len(hexText))
	}
	if !strings.HasPrefix(hexText, contractIDV1VersionPrefix) && !strings.HasPrefix(hexText, contractIDV2VersionPrefix) {
		return ContractID{}, fmt.Errorf("contract id %q has no %s or %s version prefix", hexText, contractIDV1VersionPrefix, contractIDV2VersionPrefix)
	}
	if !isLowercaseHex(hexText) {
		return ContractID{}, fmt.Errorf("contract id %q is not lowercase hex", hexText)
	}
	return ContractID{hexText: hexText}, nil
}

func (contractID ContractID) String() string { return contractID.hexText }

// SynchronizerID is a Canton synchronizer id `<identifier>::<fingerprint>`, with the rules of PartyID.
type SynchronizerID struct {
	text string
}

// ParseSynchronizerID accepts exactly one synchronizer id.
func ParseSynchronizerID(text string) (SynchronizerID, error) {
	identifier, fingerprint, found := strings.Cut(text, partyIDDelimiter)
	if !found {
		return SynchronizerID{}, fmt.Errorf("synchronizer id %q has no '::'", text)
	}
	if err := checkPartyIDHint(identifier); err != nil {
		return SynchronizerID{}, fmt.Errorf("synchronizer id %q: %w", text, err)
	}
	if err := checkPartyIDFingerprint(fingerprint); err != nil {
		return SynchronizerID{}, fmt.Errorf("synchronizer id %q: %w", text, err)
	}
	return SynchronizerID{text: text}, nil
}

func (synchronizerID SynchronizerID) String() string { return synchronizerID.text }

// Wormhole.Core.Replay.replayNamespaceMinLength and replayNamespaceMaxLength, in characters.
const (
	replayNamespaceMinCharacters = 1
	replayNamespaceMaxCharacters = 128
)

// ReplayNamespace is the opaque sub-scope of a replay trie: 1 to 128 characters.
type ReplayNamespace struct {
	text string
}

// ParseReplayNamespace applies the ReplayNode ensure rule. Daml T.length counts code points.
func ParseReplayNamespace(text string) (ReplayNamespace, error) {
	if !utf8.ValidString(text) {
		return ReplayNamespace{}, errors.New("replay namespace is not valid UTF-8")
	}
	characterCount := utf8.RuneCountInString(text)
	if characterCount < replayNamespaceMinCharacters || characterCount > replayNamespaceMaxCharacters {
		return ReplayNamespace{}, fmt.Errorf("replay namespace length %d is outside [%d, %d]", characterCount, replayNamespaceMinCharacters, replayNamespaceMaxCharacters)
	}
	return ReplayNamespace{text: text}, nil
}

func (namespace ReplayNamespace) String() string { return namespace.text }

// Wormhole.Core.Bytes.DigestNibblePrefix: 0 to 63 nibbles; "" is the trie root.
const maxDigestNibblePrefixNibbles = 63

// DigestNibblePrefix is the digest prefix that a ReplayNode covers, in lowercase hex nibbles.
type DigestNibblePrefix struct {
	nibbles string
}

// ParseDigestNibblePrefix applies the DigestNibblePrefix constructor rule.
func ParseDigestNibblePrefix(nibbles string) (DigestNibblePrefix, error) {
	if len(nibbles) > maxDigestNibblePrefixNibbles {
		return DigestNibblePrefix{}, fmt.Errorf("digest nibble prefix length %d exceeds %d", len(nibbles), maxDigestNibblePrefixNibbles)
	}
	if !isLowercaseHex(nibbles) {
		return DigestNibblePrefix{}, fmt.Errorf("digest nibble prefix %q is not lowercase hex", nibbles)
	}
	return DigestNibblePrefix{nibbles: nibbles}, nil
}

func (prefix DigestNibblePrefix) String() string { return prefix.nibbles }

func (prefix DigestNibblePrefix) NibbleCount() int { return len(prefix.nibbles) }

// ReplayNodeKey identifies the trie node of one (consumer, namespace) trie that covers prefix.
type ReplayNodeKey struct {
	consumer  PartyID
	namespace ReplayNamespace
	prefix    DigestNibblePrefix
}

func (key ReplayNodeKey) Consumer() PartyID { return key.consumer }

func (key ReplayNodeKey) Namespace() ReplayNamespace { return key.namespace }

func (key ReplayNodeKey) Prefix() DigestNibblePrefix { return key.prefix }

// CreatedCoreContract is one decoded active core contract.
type CreatedCoreContract struct {
	contractID         ContractID
	templateID         CoreTemplateID
	synchronizerID     SynchronizerID
	guardianGovernance PartyID
	replayNodeKey      ReplayNodeKey
	createdEventBlob   []byte
}

func (contract CreatedCoreContract) ContractID() ContractID { return contract.contractID }

func (contract CreatedCoreContract) TemplateID() CoreTemplateID { return contract.templateID }

func (contract CreatedCoreContract) SynchronizerID() SynchronizerID { return contract.synchronizerID }

func (contract CreatedCoreContract) GuardianGovernance() PartyID { return contract.guardianGovernance }

// ReplayNodeKey is false for every template but ReplayNode.
func (contract CreatedCoreContract) ReplayNodeKey() (ReplayNodeKey, bool) {
	if contract.templateID.template != CoreContractTemplateReplayNode {
		return ReplayNodeKey{}, false
	}
	return contract.replayNodeKey, true
}

// CreatedEventBlob is the non-empty explicit-disclosure payload.
func (contract CreatedCoreContract) CreatedEventBlob() []byte {
	return append([]byte(nil), contract.createdEventBlob...)
}

func (contract CreatedCoreContract) CreatedEventBlobByteCount() int {
	return len(contract.createdEventBlob)
}

// ArchivedCoreContract is one archived core contract.
type ArchivedCoreContract struct {
	contractID ContractID
	templateID CoreTemplateID
}

func (contract ArchivedCoreContract) ContractID() ContractID { return contract.contractID }

// TemplateID names the package of the archiving choice, not of the creation. Match archives by contract id only.
func (contract ArchivedCoreContract) TemplateID() CoreTemplateID { return contract.templateID }

// coreContractDecoder holds the rules that every decoded core contract satisfies.
type coreContractDecoder struct {
	guardianGovernance PartyID
	approved           ApprovedCorePackages
}

// decodeCreatedCoreContract requires an approved package id, a disclosed core
// template, a non-empty blob and well-formed key fields.
//
// SECURITY: the read party sees a core contract only as a signatory, so its
// guardianGovernance field must equal the read party.
func (decoder coreContractDecoder) decodeCreatedCoreContract(event *apiv2.CreatedEvent, synchronizerIDText string) (CreatedCoreContract, error) {
	if event == nil {
		return CreatedCoreContract{}, errors.New("missing created event")
	}
	templateID, err := parseCoreTemplateID(event.GetTemplateId(), decoder.approved)
	if err != nil {
		return CreatedCoreContract{}, err
	}
	if event.GetPackageName() != CorePackageName {
		return CreatedCoreContract{}, fmt.Errorf("%s: package name %q, expected %q", templateID, event.GetPackageName(), CorePackageName)
	}
	contractID, err := ParseContractID(event.GetContractId())
	if err != nil {
		return CreatedCoreContract{}, fmt.Errorf("%s: %w", templateID, err)
	}
	synchronizerID, err := ParseSynchronizerID(synchronizerIDText)
	if err != nil {
		return CreatedCoreContract{}, fmt.Errorf("%s %s: %w", templateID, contractID, err)
	}
	createdEventBlob := event.GetCreatedEventBlob()
	if len(createdEventBlob) == 0 {
		return CreatedCoreContract{}, fmt.Errorf("%s %s: empty created event blob", templateID, contractID)
	}
	createArguments, err := recordFieldsByLabel(event.GetCreateArguments())
	if err != nil {
		return CreatedCoreContract{}, fmt.Errorf("%s %s: create arguments: %w", templateID, contractID, err)
	}
	guardianGovernance, err := partyIDField(createArguments, "guardianGovernance")
	if err != nil {
		return CreatedCoreContract{}, fmt.Errorf("%s %s: %w", templateID, contractID, err)
	}
	if guardianGovernance != decoder.guardianGovernance {
		return CreatedCoreContract{}, fmt.Errorf("%s %s: guardianGovernance %s is not the read party %s", templateID, contractID, guardianGovernance, decoder.guardianGovernance)
	}
	contract := CreatedCoreContract{
		contractID:         contractID,
		templateID:         templateID,
		synchronizerID:     synchronizerID,
		guardianGovernance: guardianGovernance,
		createdEventBlob:   createdEventBlob,
	}
	if templateID.template != CoreContractTemplateReplayNode {
		return contract, nil
	}
	contract.replayNodeKey, err = decodeReplayNodeKey(createArguments, decoder.approved)
	if err != nil {
		return CreatedCoreContract{}, fmt.Errorf("%s %s: %w", templateID, contractID, err)
	}
	return contract, nil
}

// decodeArchivedCoreContract requires an approved package id and a disclosed core template.
func (decoder coreContractDecoder) decodeArchivedCoreContract(event *apiv2.ArchivedEvent) (ArchivedCoreContract, error) {
	if event == nil {
		return ArchivedCoreContract{}, errors.New("missing archived event")
	}
	templateID, err := parseCoreTemplateID(event.GetTemplateId(), decoder.approved)
	if err != nil {
		return ArchivedCoreContract{}, err
	}
	if event.GetPackageName() != CorePackageName {
		return ArchivedCoreContract{}, fmt.Errorf("%s: package name %q, expected %q", templateID, event.GetPackageName(), CorePackageName)
	}
	contractID, err := ParseContractID(event.GetContractId())
	if err != nil {
		return ArchivedCoreContract{}, fmt.Errorf("%s: %w", templateID, err)
	}
	return ArchivedCoreContract{contractID: contractID, templateID: templateID}, nil
}

// Identity of Wormhole.Core.Bytes.DigestNibblePrefix: module, type and its single constructor.
var digestNibblePrefixVariant = singleConstructorVariant{
	moduleName:  "Wormhole.Core.Bytes",
	entityName:  "DigestNibblePrefix",
	constructor: "DigestNibblePrefix",
}

// decodeReplayNodeKey reads consumer, namespace and prefix of Wormhole.Core.Replay.ReplayNode.
// The prefix variant can name the template package or, after party replication,
// the representative package; both are approved core packages.
func decodeReplayNodeKey(createArguments map[string]*apiv2.Value, approved ApprovedCorePackages) (ReplayNodeKey, error) {
	consumer, err := partyIDField(createArguments, "consumer")
	if err != nil {
		return ReplayNodeKey{}, err
	}
	namespaceText, err := textField(createArguments, "namespace")
	if err != nil {
		return ReplayNodeKey{}, err
	}
	namespace, err := ParseReplayNamespace(namespaceText)
	if err != nil {
		return ReplayNodeKey{}, fmt.Errorf("field %q: %w", "namespace", err)
	}
	prefixNibbles, err := singleConstructorVariantTextField(createArguments, "prefix", digestNibblePrefixVariant, func(variantPackageID string) error {
		packageID, err := ParsePackageID(variantPackageID)
		if err != nil {
			return err
		}
		return approved.CheckCorePackageIDApproved(packageID)
	})
	if err != nil {
		return ReplayNodeKey{}, err
	}
	prefix, err := ParseDigestNibblePrefix(prefixNibbles)
	if err != nil {
		return ReplayNodeKey{}, fmt.Errorf("field %q: %w", "prefix", err)
	}
	return ReplayNodeKey{consumer: consumer, namespace: namespace, prefix: prefix}, nil
}

// recordFieldsByLabel requires a record with unique labels.
func recordFieldsByLabel(record *apiv2.Record) (map[string]*apiv2.Value, error) {
	if record == nil {
		return nil, errors.New("missing record")
	}
	fields := make(map[string]*apiv2.Value, len(record.GetFields()))
	for _, field := range record.GetFields() {
		if _, duplicate := fields[field.GetLabel()]; duplicate {
			return nil, fmt.Errorf("duplicate field %q", field.GetLabel())
		}
		fields[field.GetLabel()] = field.GetValue()
	}
	return fields, nil
}

// partyIDField reads a Daml Party field as a PartyID.
func partyIDField(fields map[string]*apiv2.Value, label string) (PartyID, error) {
	partyText, err := partyField(fields, label)
	if err != nil {
		return PartyID{}, err
	}
	party, err := ParsePartyID(partyText)
	if err != nil {
		return PartyID{}, fmt.Errorf("field %q: %w", label, err)
	}
	return party, nil
}

// textField reads a Daml Text field.
func textField(fields map[string]*apiv2.Value, label string) (string, error) {
	fieldValue, found := fields[label]
	if !found {
		return "", fmt.Errorf("missing field %q", label)
	}
	textValue, isText := fieldValue.GetSum().(*apiv2.Value_Text)
	if !isText {
		return "", fmt.Errorf("field %q is not text", label)
	}
	return textValue.Text, nil
}
