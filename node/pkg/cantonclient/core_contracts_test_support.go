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
func DecodeCoreContractUpdatesResponseForTest(readScope ReadScope, approved ApprovedCorePackages, response *apiv2.GetUpdatesResponse, previousOffset int64) (advance CoreContractLedgerAdvance, advances bool, err error) {
	decoder, err := newCoreContractDecoder(readScope, approved)
	if err != nil {
		return CoreContractLedgerAdvance{}, false, err
	}
	return decoder.decodeUpdatesResponse(response, previousOffset)
}
