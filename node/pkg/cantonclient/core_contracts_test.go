package cantonclient

import (
	"os"
	"strconv"
	"strings"
	"testing"

	"github.com/stretchr/testify/assert"
	"github.com/stretchr/testify/require"

	apiv2 "github.com/certusone/wormhole/node/pkg/cantonclient/proto/gen/com/daml/ledger/api/v2"
)

const (
	testConsumerPartyID     = "consumer::" + testLocalNetFingerprint
	testSynchronizerID      = "global-synchronizer::" + testGovernanceFingerprint
	testCoreContractIDHex   = "00" + "aa11aa11aa11aa11aa11aa11aa11aa11aa11aa11aa11aa11aa11aa11aa11aa11" + "ca12" + "bb22bb22bb22bb22bb22bb22bb22bb22bb22bb22bb22bb22bb22bb22bb22bb22"
	testOtherContractIDHex  = "00" + "cc33cc33cc33cc33cc33cc33cc33cc33cc33cc33cc33cc33cc33cc33cc33cc33" + "ca12" + "dd44dd44dd44dd44dd44dd44dd44dd44dd44dd44dd44dd44dd44dd44dd44dd44"
	testUnapprovedPackageID = lookalikePackageID
)

var testCreatedEventBlob = []byte{0x0a, 0x0b, 0x0c}

func testApprovedCorePackages(t *testing.T) ApprovedCorePackages {
	t.Helper()
	approved, err := ParseApprovedCorePackages([]byte(`[{"packageId":"` + pinnedPackageID + `","version":"0.12.3"}]`))
	require.NoError(t, err)
	return approved
}

func testCoreContractDecoder(t *testing.T) coreContractDecoder {
	t.Helper()
	decoder, err := newCoreContractDecoder(governanceReadScope, testApprovedCorePackages(t))
	require.NoError(t, err)
	return decoder
}

func digestNibblePrefixVal(packageID string, nibbles string) *apiv2.Value {
	return variantVal(&apiv2.Identifier{PackageId: packageID, ModuleName: digestNibblePrefixVariant.moduleName, EntityName: digestNibblePrefixVariant.entityName},
		digestNibblePrefixVariant.constructor, textVal(nibbles))
}

// baselineCreateArguments is a valid create-argument set of template in pinnedPackageID.
func baselineCreateArguments(template CoreContractTemplate) map[string]*apiv2.Value {
	switch template {
	case CoreContractTemplateCoreState:
		return map[string]*apiv2.Value{"guardianGovernance": partyVal(governanceParty), "chainId": int64Val(75), "feeRecipient": partyVal(testConsumerPartyID)}
	case CoreContractTemplateEmitterRegistry:
		return map[string]*apiv2.Value{"guardianGovernance": partyVal(governanceParty), "onboardingFeeAdmin": partyVal(testConsumerPartyID), "registrationFee": int64Val(0), "nextId": int64Val(1)}
	case CoreContractTemplateReplayRootRegistry:
		return map[string]*apiv2.Value{"guardianGovernance": partyVal(governanceParty), "onboardingFeeAdmin": partyVal(testConsumerPartyID), "claimFee": int64Val(0)}
	case CoreContractTemplateReplayNode:
		return map[string]*apiv2.Value{
			"consumer":           partyVal(testConsumerPartyID),
			"guardianGovernance": partyVal(governanceParty),
			"namespace":          textVal("ntt-manager"),
			"prefix":             digestNibblePrefixVal(pinnedPackageID, "a7"),
			"splitThreshold":     int64Val(128),
		}
	default:
		panic("unknown template")
	}
}

func recordOf(fields map[string]*apiv2.Value) *apiv2.Record {
	record := &apiv2.Record{}
	for label, fieldValue := range fields {
		record.Fields = append(record.Fields, &apiv2.RecordField{Label: label, Value: fieldValue})
	}
	return record
}

func coreTemplateIdentifier(packageID string, template CoreContractTemplate) *apiv2.Identifier {
	return &apiv2.Identifier{PackageId: packageID, ModuleName: template.ModuleName(), EntityName: template.EntityName()}
}

// createdCoreEvent builds a valid created event of template with argument overrides; a nil override removes the field.
func createdCoreEvent(template CoreContractTemplate, offset int64, contractIDHex string, overrides map[string]*apiv2.Value) *apiv2.CreatedEvent {
	fields := baselineCreateArguments(template)
	for label, fieldValue := range overrides {
		if fieldValue == nil {
			delete(fields, label)
			continue
		}
		fields[label] = fieldValue
	}
	return &apiv2.CreatedEvent{
		Offset:           offset,
		ContractId:       contractIDHex,
		TemplateId:       coreTemplateIdentifier(pinnedPackageID, template),
		PackageName:      CorePackageName,
		CreateArguments:  recordOf(fields),
		CreatedEventBlob: testCreatedEventBlob,
	}
}

func archivedCoreEvent(template CoreContractTemplate, offset int64, contractIDHex string) *apiv2.ArchivedEvent {
	return &apiv2.ArchivedEvent{
		Offset:      offset,
		ContractId:  contractIDHex,
		TemplateId:  coreTemplateIdentifier(pinnedPackageID, template),
		PackageName: CorePackageName,
	}
}

func TestDecodeCreatedCoreContractValidTemplates(t *testing.T) {
	decoder := testCoreContractDecoder(t)
	for _, template := range disclosedCoreContractTemplates {
		t.Run(template.String(), func(t *testing.T) {
			contract, err := decoder.decodeCreatedCoreContract(createdCoreEvent(template, 7, testCoreContractIDHex, nil), testSynchronizerID)
			require.NoError(t, err)
			assert.Equal(t, testCoreContractIDHex, contract.ContractID().String())
			assert.Equal(t, pinnedPackageID, contract.TemplateID().PackageID().String())
			assert.Equal(t, template, contract.TemplateID().Template())
			assert.Equal(t, testSynchronizerID, contract.SynchronizerID().String())
			assert.Equal(t, governanceParty, contract.GuardianGovernance().String())
			assert.Equal(t, testCreatedEventBlob, contract.CreatedEventBlob())
			assert.Equal(t, len(testCreatedEventBlob), contract.CreatedEventBlobByteCount())
			replayNodeKey, isReplayNode := contract.ReplayNodeKey()
			if template != CoreContractTemplateReplayNode {
				assert.False(t, isReplayNode)
				assert.Equal(t, ReplayNodeKey{}, replayNodeKey)
				return
			}
			require.True(t, isReplayNode)
			assert.Equal(t, testConsumerPartyID, replayNodeKey.Consumer().String())
			assert.Equal(t, "ntt-manager", replayNodeKey.Namespace().String())
			assert.Equal(t, "a7", replayNodeKey.Prefix().String())
			assert.Equal(t, 2, replayNodeKey.Prefix().NibbleCount())
		})
	}
}

func TestDecodeCreatedReplayNodeKeyEdges(t *testing.T) {
	decoder := testCoreContractDecoder(t)
	cases := []struct {
		name              string
		overrides         map[string]*apiv2.Value
		wantNamespace     string
		wantPrefixNibbles string
	}{
		{name: "root prefix", overrides: map[string]*apiv2.Value{"prefix": digestNibblePrefixVal(pinnedPackageID, "")}, wantNamespace: "ntt-manager", wantPrefixNibbles: ""},
		{name: "prefix of 63 nibbles", overrides: map[string]*apiv2.Value{"prefix": digestNibblePrefixVal(pinnedPackageID, strings.Repeat("f", 63))}, wantNamespace: "ntt-manager", wantPrefixNibbles: strings.Repeat("f", 63)},
		{name: "namespace of 1 character", overrides: map[string]*apiv2.Value{"namespace": textVal("n")}, wantNamespace: "n", wantPrefixNibbles: "a7"},
		{name: "namespace of 128 characters", overrides: map[string]*apiv2.Value{"namespace": textVal(strings.Repeat("n", 128))}, wantNamespace: strings.Repeat("n", 128), wantPrefixNibbles: "a7"},
		{name: "namespace of 128 multi-byte characters", overrides: map[string]*apiv2.Value{"namespace": textVal(strings.Repeat("é", 128))}, wantNamespace: strings.Repeat("é", 128), wantPrefixNibbles: "a7"},
	}
	for _, tc := range cases {
		t.Run(tc.name, func(t *testing.T) {
			contract, err := decoder.decodeCreatedCoreContract(createdCoreEvent(CoreContractTemplateReplayNode, 7, testCoreContractIDHex, tc.overrides), testSynchronizerID)
			require.NoError(t, err)
			replayNodeKey, isReplayNode := contract.ReplayNodeKey()
			require.True(t, isReplayNode)
			assert.Equal(t, tc.wantNamespace, replayNodeKey.Namespace().String())
			assert.Equal(t, tc.wantPrefixNibbles, replayNodeKey.Prefix().String())
		})
	}
}

func TestDecodeCreatedCoreContractRejects(t *testing.T) {
	decoder := testCoreContractDecoder(t)
	cases := []struct {
		name               string
		template           CoreContractTemplate
		overrides          map[string]*apiv2.Value
		mutateEvent        func(event *apiv2.CreatedEvent)
		synchronizerIDText string
		wantErr            string
	}{
		{name: "unapproved package id", template: CoreContractTemplateCoreState, mutateEvent: func(event *apiv2.CreatedEvent) {
			event.TemplateId = coreTemplateIdentifier(testUnapprovedPackageID, CoreContractTemplateCoreState)
		}, wantErr: "not in the approved list"},
		{name: "malformed package id", template: CoreContractTemplateCoreState, mutateEvent: func(event *apiv2.CreatedEvent) {
			event.TemplateId = coreTemplateIdentifier("#wormhole-core", CoreContractTemplateCoreState)
		}, wantErr: "lowercase hex characters"},
		{name: "missing template id", template: CoreContractTemplateCoreState, mutateEvent: func(event *apiv2.CreatedEvent) { event.TemplateId = nil }, wantErr: "missing template id"},
		{name: "unknown template", template: CoreContractTemplateCoreState, mutateEvent: func(event *apiv2.CreatedEvent) {
			event.TemplateId = &apiv2.Identifier{PackageId: pinnedPackageID, ModuleName: coreStateModuleName, EntityName: "Emitter"}
		}, wantErr: "not a disclosed core template"},
		{name: "template in another module", template: CoreContractTemplateReplayNode, mutateEvent: func(event *apiv2.CreatedEvent) {
			event.TemplateId = &apiv2.Identifier{PackageId: pinnedPackageID, ModuleName: coreStateModuleName, EntityName: "ReplayNode"}
		}, wantErr: "not a disclosed core template"},
		{name: "package name of a lookalike", template: CoreContractTemplateCoreState, mutateEvent: func(event *apiv2.CreatedEvent) { event.PackageName = "wormhole-core-lookalike" }, wantErr: "package name"},
		{name: "malformed contract id", template: CoreContractTemplateCoreState, mutateEvent: func(event *apiv2.CreatedEvent) { event.ContractId = "zz" }, wantErr: "version prefix"},
		{name: "empty contract id", template: CoreContractTemplateCoreState, mutateEvent: func(event *apiv2.CreatedEvent) { event.ContractId = "" }, wantErr: "version prefix"},
		{name: "odd contract id", template: CoreContractTemplateCoreState, mutateEvent: func(event *apiv2.CreatedEvent) { event.ContractId = "001" }, wantErr: "odd hex length"},
		{name: "uppercase contract id", template: CoreContractTemplateCoreState, mutateEvent: func(event *apiv2.CreatedEvent) { event.ContractId = "00AB" }, wantErr: "not lowercase hex"},
		{name: "oversized contract id", template: CoreContractTemplateCoreState, mutateEvent: func(event *apiv2.CreatedEvent) { event.ContractId = "00" + strings.Repeat("ab", 127) }, wantErr: "exceeds 254"},
		{name: "malformed synchronizer id", template: CoreContractTemplateCoreState, synchronizerIDText: "global-synchronizer", wantErr: "synchronizer id"},
		{name: "empty created event blob", template: CoreContractTemplateCoreState, mutateEvent: func(event *apiv2.CreatedEvent) { event.CreatedEventBlob = nil }, wantErr: "empty created event blob"},
		{name: "missing create arguments", template: CoreContractTemplateCoreState, mutateEvent: func(event *apiv2.CreatedEvent) { event.CreateArguments = nil }, wantErr: "missing record"},
		{name: "duplicate field", template: CoreContractTemplateCoreState, mutateEvent: func(event *apiv2.CreatedEvent) {
			event.CreateArguments.Fields = append(event.CreateArguments.Fields, &apiv2.RecordField{Label: "guardianGovernance", Value: partyVal(governanceParty)})
		}, wantErr: "duplicate field"},
		{name: "missing guardianGovernance", template: CoreContractTemplateEmitterRegistry, overrides: map[string]*apiv2.Value{"guardianGovernance": nil}, wantErr: `missing field "guardianGovernance"`},
		{name: "guardianGovernance of wrong type", template: CoreContractTemplateReplayRootRegistry, overrides: map[string]*apiv2.Value{"guardianGovernance": textVal(governanceParty)}, wantErr: "is not a party"},
		{name: "malformed guardianGovernance party", template: CoreContractTemplateCoreState, overrides: map[string]*apiv2.Value{"guardianGovernance": partyVal("guardianGovernance::1220abcd")}, wantErr: "fingerprint length"},
		{name: "guardianGovernance other than the read party", template: CoreContractTemplateCoreState, overrides: map[string]*apiv2.Value{"guardianGovernance": partyVal(testConsumerPartyID)}, wantErr: "is not the read party"},
		{name: "missing consumer", template: CoreContractTemplateReplayNode, overrides: map[string]*apiv2.Value{"consumer": nil}, wantErr: `missing field "consumer"`},
		{name: "malformed consumer party", template: CoreContractTemplateReplayNode, overrides: map[string]*apiv2.Value{"consumer": partyVal("consumer")}, wantErr: "has no '::'"},
		{name: "consumer of wrong type", template: CoreContractTemplateReplayNode, overrides: map[string]*apiv2.Value{"consumer": int64Val(1)}, wantErr: "is not a party"},
		{name: "missing namespace", template: CoreContractTemplateReplayNode, overrides: map[string]*apiv2.Value{"namespace": nil}, wantErr: `missing field "namespace"`},
		{name: "namespace of wrong type", template: CoreContractTemplateReplayNode, overrides: map[string]*apiv2.Value{"namespace": int64Val(1)}, wantErr: "is not text"},
		{name: "empty namespace", template: CoreContractTemplateReplayNode, overrides: map[string]*apiv2.Value{"namespace": textVal("")}, wantErr: "outside [1, 128]"},
		{name: "oversized namespace", template: CoreContractTemplateReplayNode, overrides: map[string]*apiv2.Value{"namespace": textVal(strings.Repeat("n", 129))}, wantErr: "length 129 is outside [1, 128]"},
		{name: "missing prefix", template: CoreContractTemplateReplayNode, overrides: map[string]*apiv2.Value{"prefix": nil}, wantErr: `missing field "prefix"`},
		{name: "prefix as plain text", template: CoreContractTemplateReplayNode, overrides: map[string]*apiv2.Value{"prefix": textVal("a7")}, wantErr: "is not a variant"},
		{name: "prefix of 64 nibbles", template: CoreContractTemplateReplayNode, overrides: map[string]*apiv2.Value{"prefix": digestNibblePrefixVal(pinnedPackageID, strings.Repeat("a", 64))}, wantErr: "length 64 exceeds 63"},
		{name: "uppercase prefix", template: CoreContractTemplateReplayNode, overrides: map[string]*apiv2.Value{"prefix": digestNibblePrefixVal(pinnedPackageID, "A7")}, wantErr: "not lowercase hex"},
		{name: "non-hex prefix", template: CoreContractTemplateReplayNode, overrides: map[string]*apiv2.Value{"prefix": digestNibblePrefixVal(pinnedPackageID, "g")}, wantErr: "not lowercase hex"},
		{name: "prefix variant of another package", template: CoreContractTemplateReplayNode, overrides: map[string]*apiv2.Value{"prefix": digestNibblePrefixVal(testUnapprovedPackageID, "a7")}, wantErr: "expected the executing package"},
		{name: "prefix with wrong constructor", template: CoreContractTemplateReplayNode, overrides: map[string]*apiv2.Value{
			"prefix": variantVal(&apiv2.Identifier{PackageId: pinnedPackageID, ModuleName: digestNibblePrefixVariant.moduleName, EntityName: digestNibblePrefixVariant.entityName}, "DigestNibbleSuffix", textVal("a7")),
		}, wantErr: "has constructor"},
	}
	for _, tc := range cases {
		t.Run(tc.name, func(t *testing.T) {
			event := createdCoreEvent(tc.template, 7, testCoreContractIDHex, tc.overrides)
			if tc.mutateEvent != nil {
				tc.mutateEvent(event)
			}
			synchronizerIDText := tc.synchronizerIDText
			if synchronizerIDText == "" {
				synchronizerIDText = testSynchronizerID
			}
			_, err := decoder.decodeCreatedCoreContract(event, synchronizerIDText)
			require.ErrorContains(t, err, tc.wantErr)
		})
	}
}

func TestDecodeArchivedCoreContract(t *testing.T) {
	decoder := testCoreContractDecoder(t)
	cases := []struct {
		name        string
		mutateEvent func(event *apiv2.ArchivedEvent)
		wantErr     string
	}{
		{name: "valid"},
		{name: "unapproved package id", mutateEvent: func(event *apiv2.ArchivedEvent) {
			event.TemplateId = coreTemplateIdentifier(testUnapprovedPackageID, CoreContractTemplateReplayNode)
		}, wantErr: "not in the approved list"},
		{name: "unknown template", mutateEvent: func(event *apiv2.ArchivedEvent) {
			event.TemplateId = &apiv2.Identifier{PackageId: pinnedPackageID, ModuleName: coreStateModuleName, EntityName: "Emitter"}
		}, wantErr: "not a disclosed core template"},
		{name: "package name of a lookalike", mutateEvent: func(event *apiv2.ArchivedEvent) { event.PackageName = "wormhole-core-lookalike" }, wantErr: "package name"},
		{name: "malformed contract id", mutateEvent: func(event *apiv2.ArchivedEvent) { event.ContractId = "" }, wantErr: "version prefix"},
	}
	for _, tc := range cases {
		t.Run(tc.name, func(t *testing.T) {
			event := archivedCoreEvent(CoreContractTemplateReplayNode, 9, testCoreContractIDHex)
			if tc.mutateEvent != nil {
				tc.mutateEvent(event)
			}
			archived, err := decoder.decodeArchivedCoreContract(event)
			if tc.wantErr != "" {
				require.ErrorContains(t, err, tc.wantErr)
				return
			}
			require.NoError(t, err)
			assert.Equal(t, testCoreContractIDHex, archived.ContractID().String())
			assert.Equal(t, CoreContractTemplateReplayNode, archived.TemplateID().Template())
			assert.Equal(t, pinnedPackageID, archived.TemplateID().PackageID().String())
		})
	}
}

func TestNewCoreContractDecoderRejects(t *testing.T) {
	cases := []struct {
		name      string
		readScope ReadScope
		approved  ApprovedCorePackages
		wantErr   string
	}{
		{name: "any-party read scope", readScope: anyPartyReadScope, approved: testApprovedCorePackages(t), wantErr: "require a guardianGovernance read scope"},
		{name: "zero read scope", readScope: ReadScope{}, approved: testApprovedCorePackages(t), wantErr: "read scope not constructed"},
		{name: "no approved packages", readScope: governanceReadScope, approved: ApprovedCorePackages{}, wantErr: "require approved core packages"},
	}
	for _, tc := range cases {
		t.Run(tc.name, func(t *testing.T) {
			_, err := newCoreContractDecoder(tc.readScope, tc.approved)
			require.ErrorContains(t, err, tc.wantErr)
		})
	}
}

const damlCoreDirectory = "../../../canton/core/daml/Wormhole/Core/"

func readCoreDamlFile(t *testing.T, fileName string) string {
	t.Helper()
	damlSource, err := os.ReadFile(damlCoreDirectory + fileName)
	require.NoError(t, err)
	return string(damlSource)
}

// The core contract templates and replay key bounds match canton/core/daml.
func TestCoreContractDeclarationsMatchDaml(t *testing.T) {
	templateFiles := map[CoreContractTemplate]string{
		CoreContractTemplateCoreState:          "State.daml",
		CoreContractTemplateEmitterRegistry:    "State.daml",
		CoreContractTemplateReplayRootRegistry: "State.daml",
		CoreContractTemplateReplayNode:         "Replay.daml",
	}
	require.Len(t, templateFiles, len(disclosedCoreContractTemplates))
	for _, template := range disclosedCoreContractTemplates {
		t.Run(template.String(), func(t *testing.T) {
			damlSource := readCoreDamlFile(t, templateFiles[template])
			assert.Contains(t, damlSource, "\nmodule "+template.ModuleName()+" where\n")
			assert.Contains(t, damlSource, "\ntemplate "+template.EntityName()+"\n")
		})
	}
	replaySource := readCoreDamlFile(t, "Replay.daml")
	assert.Contains(t, replaySource, "replayNamespaceMinLength = "+strconv.Itoa(replayNamespaceMinCharacters)+"\n")
	assert.Contains(t, replaySource, "replayNamespaceMaxLength = "+strconv.Itoa(replayNamespaceMaxCharacters)+"\n")
	bytesSource := readCoreDamlFile(t, "Bytes.daml")
	assert.Contains(t, bytesSource, "data DigestNibblePrefix = DigestNibblePrefix Text")
	assert.Contains(t, bytesSource, `parseNibbleCountInRange "DigestNibblePrefix" 0 `+strconv.Itoa(maxDigestNibblePrefixNibbles)+"\n")
	assert.Contains(t, bytesSource, "\nmodule "+digestNibblePrefixVariant.moduleName+"\n")
}
