package cantonclient

import (
	apiv2 "github.com/certusone/wormhole/node/pkg/cantonclient/proto/gen/com/daml/ledger/api/v2"
)

// DecodeCreatedCoreContractForTest decodes event with the production core contract decoder.
// Do not call it outside tests: it exists so that other packages test with decoded values.
func DecodeCreatedCoreContractForTest(readScope ReadScope, approved ApprovedCorePackages, event *apiv2.CreatedEvent, synchronizerIDText string) (CreatedCoreContract, error) {
	decoder, err := newCoreContractDecoder(readScope, approved)
	if err != nil {
		return CreatedCoreContract{}, err
	}
	return decoder.decodeCreatedCoreContract(event, synchronizerIDText)
}

// DecodeCoreContractUpdatesResponseForTest decodes one GetUpdates response with the production stream decoder.
// Do not call it outside tests: it exists so that other packages test with decoded values.
func DecodeCoreContractUpdatesResponseForTest(readScope ReadScope, approved ApprovedCorePackages, response *apiv2.GetUpdatesResponse, previousOffset int64) (advance CoreContractLedgerAdvance, movesTheOffset bool, err error) {
	decoder, err := newCoreContractDecoder(readScope, approved)
	if err != nil {
		return CoreContractLedgerAdvance{}, false, err
	}
	return decoder.decodeUpdatesResponse(response, previousOffset)
}

// CollectActiveCoreContractsForTest applies the ReadActiveCoreContracts limits to contracts in order: at the first
// ReplayNode above maxReplayNodes it keeps the singletons of contracts alone, as the singleton-only read does.
// Do not call it outside tests: it exists so that fake readers in other packages enforce the same limits.
func CollectActiveCoreContractsForTest(contracts []CreatedCoreContract, maxReplayNodes int) (ActiveCoreContracts, error) {
	collector, err := newActiveCoreContractCollector(maxReplayNodes)
	if err != nil {
		return ActiveCoreContracts{}, err
	}
	for _, contract := range contracts {
		replayNodeLimitExceeded, err := collector.keepActiveContract(contract)
		if err != nil {
			return ActiveCoreContracts{}, err
		}
		if replayNodeLimitExceeded {
			return collectSingletonContracts(contracts)
		}
	}
	return collector.collectedActiveCoreContracts(), nil
}

func collectSingletonContracts(contracts []CreatedCoreContract) (ActiveCoreContracts, error) {
	singletonCollector := newSingletonContractCollector()
	for _, contract := range contracts {
		if contract.TemplateID().Template() == CoreContractTemplateReplayNode {
			continue
		}
		if _, err := singletonCollector.keepActiveContract(contract); err != nil {
			return ActiveCoreContracts{}, err
		}
	}
	return singletonCollector.collectedActiveCoreContracts(), nil
}
