package txverifier

// TODO:
// - more robust mocking of RPC return values so that we can test multiple cases
// - add tests checking amount values from ValidateReceipt

import (
	"context"
	"errors"
	"math/big"
	"testing"

	"github.com/ethereum/go-ethereum/common"
	"github.com/ethereum/go-ethereum/core/types"
	"github.com/stretchr/testify/assert"
	"github.com/stretchr/testify/require"
	"github.com/wormhole-foundation/wormhole/sdk/vaa"

	ethereum "github.com/ethereum/go-ethereum"

	"github.com/certusone/wormhole/node/pkg/watchers/evm/connectors/ethabi"
	ipfslog "github.com/ipfs/go-log/v2"
)

// Important addresses for testing. Arbitrary, but Ethereum mainnet values used here.
var (
	coreBridgeAddr  = common.HexToAddress("0x98f3c9e6E3fAce36bAAd05FE09d375Ef1464288B")
	tokenBridgeAddr = common.HexToAddress("0x3ee18B2214AFF97000D974cf647E7C347E8fa585")

	// WETH
	nativeAddrGeth   = common.HexToAddress("0xc02aaa39b223fe8d0a0e5c4f27ead9083c756cc2")
	nativeAddrVAA, _ = vaa.BytesToAddress(nativeAddrGeth.Bytes())

	// USDC
	usdcAddrGeth   = common.HexToAddress("0xA0b86991c6218b36c1d19D4a2e9Eb0cE3606eB48")
	usdcAddrVAA, _ = vaa.BytesToAddress(usdcAddrGeth.Bytes())

	// EOA account representing a transaction sender.
	eoaAddrGeth   = common.HexToAddress("0xbeefcafe")
	eoaAddrVAA, _ = vaa.BytesToAddress([]byte{0xbe, 0xef, 0xca, 0xfe})
)

type mockConnections struct {
	transferVerifier *TransferVerifier[*mockClient, *mockConnector]
	ctx              *context.Context
	ctxCancel        context.CancelFunc
}

// Stub struct, only exist to implement the interfaces
type mockClient struct{}

// TODO add a helper method to actually populate the results of the mocked method
// TODO this should maybe be mocked differently. CallContract is used for both 'get decimals' and 'unwrap'.
// Depending on how much mocking we want to do, this might need edits. On the other hand, we don't really need to
// test geth's functions and this functionality is better handled by integration testing anyway
func (m *mockClient) CallContract(ctx context.Context, msg ethereum.CallMsg, blockNumber *big.Int) ([]byte, error) {
	// this is used by the calling code only to get decimal values
	// always return 8
	return common.LeftPadBytes([]byte{0x08}, 32), nil
}

type mockConnector struct{}

// TODO add a helper method to actually populate the results of the mocked method
// TODO add different results here so we can test different values
func (c *mockConnector) ParseLogMessagePublished(log types.Log) (*ethabi.AbiLogMessagePublished, error) {
	// add mock data
	return &ethabi.AbiLogMessagePublished{
		Sender:   tokenBridgeAddr,
		Sequence: 0,
		Nonce:    0,
		Payload:  transferTokensPayload(big.NewInt(1)),
		Raw:      log,
	}, nil
}

func (c *mockConnector) TransactionReceipt(ctx context.Context, txHash common.Hash) (*types.Receipt, error) {
	return nil, nil
}

// Create the connections and loggers expected by the functions we are testing
func setup() *mockConnections {
	logger := ipfslog.Logger("wormhole-transfer-verifier-tests").Desugar()
	ipfslog.SetAllLoggers(ipfslog.LevelDebug)
	transferVerifier := &TransferVerifier[*mockClient, *mockConnector]{
		Addresses: &TVAddresses{
			CoreBridgeAddr:    coreBridgeAddr,
			TokenBridgeAddr:   tokenBridgeAddr,
			WrappedNativeAddr: nativeAddrGeth,
		},
		chainIds:     &chainIds{evmChainId: 1, wormholeChainId: vaa.ChainIDEthereum},
		evmConnector: &mockConnector{},
		client:       &mockClient{},
		logger:       *logger,
	}
	ctx, ctxCancel := context.WithCancel(context.Background())

	return &mockConnections{
		transferVerifier,
		&ctx,
		ctxCancel,
	}
}

// Define some transfer logs to make it easier to write tests for parsing receipts.
// Typical receipt logs that can be included in various receipt test cases
var (
	// A valid transfer log for an ERC20 transfer event.
	transferLog = &types.Log{
		Address: usdcAddrGeth,
		Topics: []common.Hash{
			// Transfer(address,address,uint256)
			common.HexToHash(EVENTHASH_ERC20_TRANSFER),
			// from
			eoaAddrGeth.Hash(),
			// to
			tokenBridgeAddr.Hash(),
		},
		// amount
		Data: common.LeftPadBytes([]byte{0x01}, 32),
	}

	// A valid transfer log for a log message published event.
	validLogMessagedPublishedLog = &types.Log{
		Address: coreBridgeAddr,
		Topics: []common.Hash{
			// LogMessagePublished(address indexed sender, uint64 sequence, uint32 nonce, bytes payload, uint8 consistencyLevel);
			common.HexToHash(EVENTHASH_WORMHOLE_LOG_MESSAGE_PUBLISHED),
			// sender
			tokenBridgeAddr.Hash(),
		},
		Data: receiptData(big.NewInt(255)),
	}
)

var (
	validTransferReceipt = &types.Receipt{
		Status: types.ReceiptStatusSuccessful,
		Logs: []*types.Log{
			transferLog,
			validLogMessagedPublishedLog,
		},
	}
	// Invalid: no erc20 transfer, so amount out > amount in
	// invalidTransferReceipt = &types.Receipt{
	// 	Status: types.ReceiptStatusSuccessful,
	// 	Logs: []*types.Log{
	// 		logMessagedPublishedLog,
	// 	},
	// }
	// TODO: Invalid: erc20 transfer amount is less than payload amount, so amount out > amount in
	// invalidTransferReceipt = &types.Receipt{
	// 	Status:            types.ReceiptStatusSuccessful,
	// 	Logs: []*types.Log{logMessagedPublishedLog},
	// }
)

func TestParseReceiptHappyPath(t *testing.T) {
	mocks := setup()
	defer mocks.ctxCancel()

	tests := map[string]struct {
		receipt  *types.Receipt
		expected *TransferReceipt
	}{
		"valid transfer receipt, single LogMessagePublished": {
			validTransferReceipt,
			&TransferReceipt{
				Deposits: &[]*NativeDeposit{},
				Transfers: &[]*ERC20Transfer{
					{
						From:         eoaAddrGeth,
						To:           tokenBridgeAddr,
						TokenAddress: usdcAddrGeth,
						TokenChain:   vaa.ChainIDEthereum,
						Amount:       big.NewInt(1),
					},
				},
				MessagePublications: &[]*LogMessagePublished{
					{
						EventEmitter: coreBridgeAddr,
						MsgSender:    tokenBridgeAddr,
						TransferDetails: &TransferDetails{
							PayloadType:   TransferTokens,
							TokenChain:    2, // Wormhole ethereum chain ID
							TargetAddress: eoaAddrVAA,
							// Amount and OriginAddress are not populated by ParseReceipt
							Amount:        big.NewInt(1),
							OriginAddress: usdcAddrVAA,
						},
					},
				},
			},
		},
	}
	for name, test := range tests {
		t.Run(name, func(t *testing.T) {

			transferReceipt, err := mocks.transferVerifier.parseReceipt(test.receipt)
			require.NoError(t, err)

			// Note: the data for this test uses only a single transfer. However, if multiple transfers
			// are used, iteration over these slices will be non-deterministic which might result in a flaky
			// test.
			expectedTransfers := *test.expected.Transfers
			assert.Equal(t, len(expectedTransfers), len(*transferReceipt.Transfers))
			for _, ret := range *transferReceipt.Transfers {
				assert.Equal(t, expectedTransfers[0].To, ret.To)
				assert.Equal(t, expectedTransfers[0].From, ret.From)
				assert.Equal(t, expectedTransfers[0].TokenAddress, ret.TokenAddress)
				assert.Zero(t, ret.Amount.Cmp(expectedTransfers[0].Amount))
			}

			expectedMessages := *test.expected.MessagePublications
			assert.Equal(t, len(expectedMessages), len(*transferReceipt.MessagePublications))
			for _, ret := range *transferReceipt.MessagePublications {
				// TODO: switch argument order to (expected, actual)
				assert.Equal(t, ret.MsgSender, expectedMessages[0].MsgSender)
				assert.Equal(t, ret.EventEmitter, expectedMessages[0].EventEmitter)
				assert.Equal(t, ret.TransferDetails, expectedMessages[0].TransferDetails)

				t.Logf("Expected Amount: %s", expectedMessages[0].TransferDetails.Amount.String())
				t.Logf("Actual Amount: %s", ret.TransferDetails.Amount.String())
				assert.Zero(t, expectedMessages[0].TransferDetails.Amount.Cmp(ret.TransferDetails.Amount))
			}
		})
	}
}

func TestParseReceiptErrors(t *testing.T) {
	mocks := setup()
	defer mocks.ctxCancel()

	// Create a log containing an invalid deposit log
	badDepositLog := *transferLog
	badDepositLog.Topics = []common.Hash{
		common.HexToHash(EVENTHASH_WETH_DEPOSIT),
		// Omit essential topics
	}

	// Create a log containing an invalid transfer log
	badTransferLog := *transferLog
	badTransferLog.Topics = []common.Hash{
		common.HexToHash(EVENTHASH_ERC20_TRANSFER),
		// Omit essential topics
	}

	// Create a log containing a LogMessagePublished event without any payload
	emptyPayloadLogMessagePublishedLog := *validLogMessagedPublishedLog
	emptyPayloadLogMessagePublishedLog.Data = []byte{}

	// TODO: Create a receipt with the wrong payload type (not a token transfer).
	// wrongPayloadTypeLogMessagePublishedLog := types.Log{
	// 	Address: coreBridgeAddr,
	// 	Topics: []common.Hash{
	// 		// LogMessagePublished(address indexed sender, uint64 sequence, uint32 nonce, bytes payload, uint8 consistencyLevel);
	// 		common.HexToHash(EVENTHASH_WORMHOLE_LOG_MESSAGE_PUBLISHED),
	// 		// sender
	// 		tokenBridgeAddr.Hash(),
	// 	},
	// 	Data: receiptData(big.NewInt(1).SetBytes([]byte{0xaa})),
	// }
	// // The LogMessagePublished payload type occurs in the 6th EVM word slot, and is left-padded with zeroes.
	// // Note that the value is 0-indexed
	// payloadTypeOffset := EVM_WORD_LENGTH * 5
	// wrongPayloadTypeLogMessagePublishedLog.Data[payloadTypeOffset] = 0x02

	tests := map[string]struct {
		receipt *types.Receipt
	}{
		"wrong receipt status": {
			receipt: &types.Receipt{
				Status: types.ReceiptStatusFailed,
				Logs: []*types.Log{
					validLogMessagedPublishedLog,
				},
			},
		},
		"no logs": {
			receipt: &types.Receipt{
				Status: types.ReceiptStatusSuccessful,
				Logs:   []*types.Log{},
			},
		},
		"invalid deposit log in receipt": {
			receipt: &types.Receipt{
				Status: types.ReceiptStatusSuccessful,
				Logs: []*types.Log{
					&badDepositLog,
				},
			},
		},
		"invalid transfer log in receipt": {
			receipt: &types.Receipt{
				Status: types.ReceiptStatusSuccessful,
				Logs: []*types.Log{
					&badTransferLog,
				},
			},
		},
		"LogMessagePublished with empty payload": {
			receipt: &types.Receipt{
				Status: types.ReceiptStatusSuccessful,
				Logs: []*types.Log{
					&emptyPayloadLogMessagePublishedLog,
				},
			},
		},
		// TODO: Need to create a different mock for ParseLogMessagePublished in order to test this
		// "LogMessagePublished with wrong payload type": {
		// 	receipt: &types.Receipt{
		// 		Status: types.ReceiptStatusSuccessful,
		// 		Logs: []*types.Log{
		// 			&wrongPayloadTypeLogMessagePublishedLog,
		// 		},
		// 	},
		// },
	}
	for name, test := range tests {
		t.Run(name, func(t *testing.T) {

			receipt, err := mocks.transferVerifier.parseReceipt(test.receipt)
			require.Error(t, err)
			assert.Nil(t, receipt)
		})
	}
}

func TestParseERC20TransferEvent(t *testing.T) {
	type parsedValues struct {
		from   common.Address
		to     common.Address
		amount *big.Int
	}
	erc20TransferHash := common.HexToHash(EVENTHASH_ERC20_TRANSFER)
	t.Parallel() // marks TLog as capable of running in parallel with other tests
	tests := map[string]struct {
		topics   []common.Hash
		data     []byte
		expected *parsedValues
		err      error
	}{
		"well-formed": {
			topics: []common.Hash{
				erc20TransferHash,
				eoaAddrGeth.Hash(),
				tokenBridgeAddr.Hash(),
			},
			data: common.LeftPadBytes([]byte{0x01}, 32),
			expected: &parsedValues{
				from:   eoaAddrGeth,
				to:     tokenBridgeAddr,
				amount: new(big.Int).SetBytes([]byte{0x01}),
			},
			err: nil,
		},
		"data too short": {
			topics: []common.Hash{
				erc20TransferHash,
				eoaAddrGeth.Hash(),
				tokenBridgeAddr.Hash(),
			},
			// should be 32 bytes exactly
			data:     []byte{0x01},
			expected: &parsedValues{}, // everything nil for its type
			err:      ErrEventWrongDataSize,
		},
		"wrong number of topics": {
			// only 1 topic: should be 3
			topics: []common.Hash{
				erc20TransferHash,
			},
			data:     common.LeftPadBytes([]byte{0x01}, 32),
			expected: &parsedValues{}, // everything nil for its type
			err:      ErrTransferIsNotERC20,
		},
	}

	for name, test := range tests {
		t.Run(name, func(t *testing.T) {
			t.Parallel() // marks each test case as capable of running in parallel with each other

			from, to, amount, err := parseERC20TransferEvent(test.topics, test.data)
			require.Equal(t, err, test.err)
			assert.Equal(t, test.expected.from, from)
			assert.Equal(t, test.expected.to, to)
			assert.Zero(t, amount.Cmp(test.expected.amount))
		})
	}
}

func TestParseWNativeDepositEvent(t *testing.T) {
	{
		type parsedValues struct {
			destination common.Address
			amount      *big.Int
			err         error
		}
		t.Parallel()

		wethDepositHash := common.HexToHash(EVENTHASH_WETH_DEPOSIT)
		tests := map[string]struct {
			topics   []common.Hash
			data     []byte
			expected *parsedValues
		}{
			"well-formed": {
				topics: []common.Hash{
					wethDepositHash,
					tokenBridgeAddr.Hash(),
				},
				data: common.LeftPadBytes([]byte{0x01}, 32),
				expected: &parsedValues{
					destination: tokenBridgeAddr,
					amount:      new(big.Int).SetBytes([]byte{0x01}),
					err:         nil,
				},
			},
			"data too short": {
				topics: []common.Hash{
					wethDepositHash,
					tokenBridgeAddr.Hash(),
				},
				// should be 32 bytes exactly
				data: []byte{0x01},
				expected: &parsedValues{
					destination: common.Address{},
					amount:      nil,
					err:         ErrEventWrongDataSize,
				},
			},
			"wrong number of topics": {
				// only 1 topic: should be 2
				topics: []common.Hash{
					wethDepositHash,
				},
				data: common.LeftPadBytes([]byte{0x01}, 32),
				expected: &parsedValues{
					destination: common.Address{},
					amount:      nil,
					err:         ErrDepositWrongNumberOfTopics,
				},
			},
		}

		for name, test := range tests {
			t.Run(name, func(t *testing.T) {
				t.Parallel()

				destination, amount, err := parseWNativeDepositEvent(test.topics, test.data)
				require.Equal(t, test.expected.destination, destination)
				require.Equal(t, test.expected.amount, amount)
				require.Equal(t, test.expected.err, err)
				require.Zero(t, amount.Cmp(test.expected.amount))
			})
		}
	}

}

func TestValidateReceipt(t *testing.T) {
	mocks := setup()

	tests := map[string]struct {
		transferReceipt *TransferReceipt
		// number of receipts successfully processed
		expected    int
		shouldError bool
	}{
		// TODO test cases:
		// - multiple transfers adding up to the right amount
		// - multiple depoists adding up to the right amount
		// - multiple LogMessagePublished events
		"valid transfer: amounts match, deposit": {
			transferReceipt: &TransferReceipt{
				Deposits: &[]*NativeDeposit{
					{
						TokenAddress: nativeAddrGeth,
						TokenChain:   vaa.ChainIDEthereum,
						Receiver:     tokenBridgeAddr,
						Amount:       big.NewInt(123),
					},
				},
				Transfers: &[]*ERC20Transfer{},
				MessagePublications: &[]*LogMessagePublished{
					{
						EventEmitter: coreBridgeAddr,
						MsgSender:    tokenBridgeAddr,
						TransferDetails: &TransferDetails{
							PayloadType:   TransferTokens,
							OriginAddress: nativeAddrVAA,
							TargetAddress: eoaAddrVAA,
							TokenChain:    2,
							Amount:        big.NewInt(123),
						},
					},
				},
			},
			expected:    1,
			shouldError: false,
		},
		"valid transfer: amounts match, transfer": {
			transferReceipt: &TransferReceipt{
				Deposits: &[]*NativeDeposit{},
				Transfers: &[]*ERC20Transfer{
					{
						TokenAddress: usdcAddrGeth,
						TokenChain:   vaa.ChainIDEthereum,
						From:         eoaAddrGeth,
						To:           tokenBridgeAddr,
						Amount:       big.NewInt(456),
						OriginAddr:   usdcAddrVAA,
					},
				},
				MessagePublications: &[]*LogMessagePublished{
					{
						EventEmitter: coreBridgeAddr,
						MsgSender:    tokenBridgeAddr,
						TransferDetails: &TransferDetails{
							PayloadType:   TransferTokens,
							OriginAddress: usdcAddrVAA,
							TokenChain:    2,
							TargetAddress: eoaAddrVAA,
							Amount:        big.NewInt(456),
						},
					},
				},
			},
			expected:    1,
			shouldError: false,
		},
		"valid transfer: amount in is greater than amount out, deposit": {
			transferReceipt: &TransferReceipt{
				Deposits: &[]*NativeDeposit{
					{
						TokenAddress: nativeAddrGeth,
						TokenChain:   vaa.ChainIDEthereum,
						Receiver:     tokenBridgeAddr,
						Amount:       big.NewInt(999),
					},
				},
				Transfers: &[]*ERC20Transfer{},
				MessagePublications: &[]*LogMessagePublished{
					{
						EventEmitter: coreBridgeAddr,
						MsgSender:    tokenBridgeAddr,
						TransferDetails: &TransferDetails{
							PayloadType:   TransferTokens,
							TokenChain:    2,
							OriginAddress: nativeAddrVAA,
							TargetAddress: eoaAddrVAA,
							Amount:        big.NewInt(321),
						},
					},
				},
			},
			expected:    1,
			shouldError: false,
		},
		"valid transfer: amount in is greater than amount out, transfer": {
			transferReceipt: &TransferReceipt{
				Deposits: &[]*NativeDeposit{},
				Transfers: &[]*ERC20Transfer{
					{
						TokenAddress: usdcAddrGeth,
						TokenChain:   vaa.ChainIDEthereum,
						From:         eoaAddrGeth,
						To:           tokenBridgeAddr,
						Amount:       big.NewInt(999),
						OriginAddr:   usdcAddrVAA,
					},
				},
				MessagePublications: &[]*LogMessagePublished{
					{
						EventEmitter: coreBridgeAddr,
						MsgSender:    tokenBridgeAddr,
						TransferDetails: &TransferDetails{
							PayloadType:   TransferTokens,
							OriginAddress: usdcAddrVAA,
							TargetAddress: eoaAddrVAA,
							TokenChain:    2,
							Amount:        big.NewInt(321),
						},
					},
				},
			},
			expected:    1,
			shouldError: false,
		},
		"invalid transfer: amount in too low, deposit": {
			transferReceipt: &TransferReceipt{
				Deposits: &[]*NativeDeposit{
					{
						TokenAddress: nativeAddrGeth,
						TokenChain:   NATIVE_CHAIN_ID,
						Receiver:     tokenBridgeAddr,
						Amount:       big.NewInt(10),
					},
				},
				Transfers: &[]*ERC20Transfer{},
				MessagePublications: &[]*LogMessagePublished{
					{
						EventEmitter: coreBridgeAddr,
						MsgSender:    tokenBridgeAddr,
						TransferDetails: &TransferDetails{
							PayloadType:   TransferTokens,
							OriginAddress: nativeAddrVAA,
							TargetAddress: eoaAddrVAA,
							TokenChain:    vaa.ChainIDEthereum,
							Amount:        big.NewInt(11),
						},
					},
				},
			},
			expected:    1,
			shouldError: true,
		},
		"invalid transfer: amount in too low, transfer": {
			transferReceipt: &TransferReceipt{
				Deposits: &[]*NativeDeposit{},
				Transfers: &[]*ERC20Transfer{
					{
						TokenAddress: usdcAddrGeth,
						TokenChain:   NATIVE_CHAIN_ID,
						From:         eoaAddrGeth,
						To:           tokenBridgeAddr,
						Amount:       big.NewInt(1),
						OriginAddr:   usdcAddrVAA,
					},
				},
				MessagePublications: &[]*LogMessagePublished{
					{
						EventEmitter: coreBridgeAddr,
						MsgSender:    tokenBridgeAddr,
						TransferDetails: &TransferDetails{
							PayloadType:   TransferTokens,
							OriginAddress: usdcAddrVAA,
							TargetAddress: eoaAddrVAA,
							TokenChain:    2,
							Amount:        big.NewInt(2),
						},
					},
				},
			},
			expected:    1,
			shouldError: true,
		},
		"invalid transfer: transfer out after transferring a different token": {
			transferReceipt: &TransferReceipt{
				Deposits: &[]*NativeDeposit{},
				Transfers: &[]*ERC20Transfer{
					{
						TokenAddress: usdcAddrGeth,
						TokenChain:   vaa.ChainIDEthereum,
						From:         eoaAddrGeth,
						To:           tokenBridgeAddr,
						Amount:       big.NewInt(2),
						OriginAddr:   usdcAddrVAA,
					},
				},
				MessagePublications: &[]*LogMessagePublished{
					{
						EventEmitter: coreBridgeAddr,
						MsgSender:    tokenBridgeAddr,
						TransferDetails: &TransferDetails{
							PayloadType:   TransferTokens,
							OriginAddress: nativeAddrVAA,
							TargetAddress: eoaAddrVAA,
							TokenChain:    2,
							Amount:        big.NewInt(2),
						},
					},
				},
			},
			expected:    1,
			shouldError: true,
		},
	}

	for name, test := range tests {
		t.Run(name, func(t *testing.T) {

			summary, err := mocks.transferVerifier.validateReceipt(test.transferReceipt)

			assert.Equal(t, test.expected, summary.logsProcessed, "number of processed receipts did not match")

			if err != nil {
				assert.True(t, test.shouldError, "test should have returned an error")
				var invErr *InvariantError
				ok := errors.As(err, &invErr)
				assert.True(t, ok, "wrong error type. expected InvariantError, got: `%w`", err)
			} else {
				assert.False(t, test.shouldError, "test should not have returned an error but got: `%w`", err)
			}
		})
	}
}

// TestTransferReceiptValidate verifies the happy path and expected errors for the TransferReceipt's Validate() method.
func TestTransferReceiptValidate(t *testing.T) {

	// Test happy path: a TransferReceipt is well-formed if it has at least one MessagePublication.
	transferReceipt := TransferReceipt{
		Deposits:  &[]*NativeDeposit{},
		Transfers: &[]*ERC20Transfer{},
		MessagePublications: &[]*LogMessagePublished{

			{
				EventEmitter:    [20]byte{},
				MsgSender:       [20]byte{},
				TransferDetails: &TransferDetails{},
			},
		},
	}

	err := transferReceipt.Validate()
	require.NoError(t, err, "Validate must not return an error when it has a non-zero Message Publication slice")

	// Test error cases.
	// NOTE: The test cases below distinguish between nil and the empty struct values for a TransferReceipt.
	tests := map[string]struct {
		transferReceipt *TransferReceipt
		errMsg          string
	}{
		"nil Deposits": {
			&TransferReceipt{
				Deposits:            nil,
				Transfers:           &[]*ERC20Transfer{},
				MessagePublications: &[]*LogMessagePublished{},
			},
			"parsed receipt's Deposits field is nil",
		},
		"nil Transfers": {
			&TransferReceipt{
				Deposits:            &[]*NativeDeposit{},
				Transfers:           nil,
				MessagePublications: &[]*LogMessagePublished{},
			},
			"parsed receipt's Transfers field is nil",
		},
		"nil MessagePublications": {
			&TransferReceipt{
				Deposits:            &[]*NativeDeposit{},
				Transfers:           &[]*ERC20Transfer{},
				MessagePublications: nil,
			},
			"parsed receipt's MessagePublications field is nil",
		},
		"empty MessagePublications": {
			&TransferReceipt{
				Deposits:            &[]*NativeDeposit{},
				Transfers:           &[]*ERC20Transfer{},
				MessagePublications: &[]*LogMessagePublished{},
			},
			ErrNoMsgsFromTokenBridge.Error(),
		},
	}

	for name, test := range tests {
		t.Run(name, func(t *testing.T) {
			require.NotNil(t, test.transferReceipt)
			err := test.transferReceipt.Validate()
			require.ErrorContains(t, err, test.errMsg)
		})
	}
}

func TestNoPanics(t *testing.T) {
	mocks := setup()
	defer mocks.ctxCancel()

	require.NotPanics(t, func() {
		_, err := mocks.transferVerifier.validateReceipt(nil)
		require.Error(t, err, "must return an error on nil input")
	}, "should handle nil without panicking")

	require.NotPanics(t, func() {
		err := mocks.transferVerifier.updateReceiptDetails(nil)
		require.Error(t, err, "UpdateReceiptDetails must return an error on nil input")
	}, "UpdateReceiptDetails should handle nil without panicking")

	// Regression check: ensure that a log with no indexed topics does not panic.
	receipt := *validTransferReceipt
	receipt.Logs[0].Topics = []common.Hash{}
	require.NotPanics(t, func() {
		parsed, err := mocks.transferVerifier.parseReceipt(&receipt)
		require.NotNil(t, parsed)
		require.NoError(t, err)
	}, "UpdateReceiptDetails must not panic when a log with no topics is present")
}

func receiptData(payloadAmount *big.Int) (data []byte) {
	// non-payload part of the receipt and ABI metadata fields
	seq := common.LeftPadBytes([]byte{0x11}, 32)
	nonce := common.LeftPadBytes([]byte{0x22}, 32)
	offset := common.LeftPadBytes([]byte{0x80}, 32)
	consistencyLevel := common.LeftPadBytes([]byte{0x01}, 32)
	payloadLength := common.LeftPadBytes([]byte{0x85}, 32) // 133 for transferTokens

	data = append(data, seq...)
	data = append(data, nonce...)
	data = append(data, offset...)
	data = append(data, consistencyLevel...)
	data = append(data, payloadLength...)
	data = append(data, transferTokensPayload(payloadAmount)...)

	return data
}

// Generate the Payload portion of a LogMessagePublished receipt for use in unit tests.
func transferTokensPayload(payloadAmount *big.Int) (data []byte) {
	// tokenTransfer() payload format:
	//     transfer.payloadID, uint8, size: 1
	//     amount, uint256, size: 32
	//     tokenAddress, bytes32: size 32
	//     tokenChain, uint16, size 2
	//     to, bytes32: size 32
	//     toChain, uint16, size: 2
	//     fee, uint256 size: size 32
	// 1 + 32 + 32 + 2 + 32 + 2 + 32 = 133
	// See also: https://docs.soliditylang.org/en/latest/abi-spec.html

	payloadType := []byte{0x01} // transferTokens, not padded
	amount := common.LeftPadBytes(payloadAmount.Bytes(), 32)
	tokenAddress := usdcAddrVAA.Bytes()
	tokenChain := common.LeftPadBytes([]byte{0x02}, 2) // Eth wormhole chain ID, uint16
	to := common.LeftPadBytes([]byte{0xbe, 0xef, 0xca, 0xfe}, 32)
	toChain := common.LeftPadBytes([]byte{0x01}, 2) // Eth wormhole chain ID, uint16
	fee := common.LeftPadBytes([]byte{0x00}, 32)
	data = append(data, payloadType...)
	data = append(data, amount...)
	data = append(data, tokenAddress...)
	data = append(data, tokenChain...)
	data = append(data, to...)
	data = append(data, toChain...)
	data = append(data, fee...)
	return data
}
