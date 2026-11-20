package guardiand

import (
	"encoding/hex"
	"io"
	"os"
	"testing"
	"time"

	"github.com/certusone/wormhole/node/pkg/adminrpc"
	nodev1 "github.com/certusone/wormhole/node/pkg/proto/node/v1"
	"github.com/stretchr/testify/require"
	"google.golang.org/protobuf/encoding/prototext"
)

func captureCoreBridgeTransferFeesTemplate(t *testing.T, targetChain string, amount string, recipient string) []byte {
	*coreBridgeTransferFeesChainId = targetChain
	*coreBridgeTransferFeesAmount = amount
	*coreBridgeTransferFeesRecipient = recipient

	originalStdout := os.Stdout
	reader, writer, err := os.Pipe()
	require.NoError(t, err)
	os.Stdout = writer
	runCoreBridgeTransferFeesTemplate(nil, nil)
	os.Stdout = originalStdout
	require.NoError(t, writer.Close())

	templateOutput, err := io.ReadAll(reader)
	require.NoError(t, err)
	require.NotEmpty(t, templateOutput)
	return templateOutput
}

func TestCoreBridgeTransferFeesTemplateToVaa(t *testing.T) {
	const recipientHex = "00000000000000000000000000000000000000000000000000000000deadbeef"
	const moduleAndAction = "00000000000000000000000000000000000000000000000000000000436f726504"
	const amountTenThousand = "0000000000000000000000000000000000000000000000000000000000002710"
	const zeroAmount = "0000000000000000000000000000000000000000000000000000000000000000"

	cases := []struct {
		name            string
		targetChain     string
		amount          string
		expectedPayload string
	}{
		{"chain 0", "0", "10000", moduleAndAction + "0000" + amountTenThousand + recipientHex},
		{"amount 0", "canton", "0", moduleAndAction + "004b" + zeroAmount + recipientHex},
		{"chain 0 and amount 0", "0", "0", moduleAndAction + "0000" + zeroAmount + recipientHex},
		{"named chain", "solana", "10000", moduleAndAction + "0001" + amountTenThousand + recipientHex},
	}
	for _, testCase := range cases {
		t.Run(testCase.name, func(t *testing.T) {
			templateOutput := captureCoreBridgeTransferFeesTemplate(t, testCase.targetChain, testCase.amount, "0x"+recipientHex)

			var request nodev1.InjectGovernanceVAARequest
			require.NoError(t, prototext.Unmarshal(templateOutput, &request))
			require.Len(t, request.Messages, 1)

			governanceVAA, err := adminrpc.GovMsgToVaa(request.Messages[0], request.CurrentSetIndex, time.Now())
			require.NoError(t, err)
			require.Equal(t, testCase.expectedPayload, hex.EncodeToString(governanceVAA.Payload))
		})
	}
}

func TestCoreBridgeTransferFeesTemplateToVaaRefusesZeroRecipient(t *testing.T) {
	templateOutput := captureCoreBridgeTransferFeesTemplate(t, "0", "0", "0x00")

	var request nodev1.InjectGovernanceVAARequest
	require.NoError(t, prototext.Unmarshal(templateOutput, &request))
	require.Len(t, request.Messages, 1)

	_, err := adminrpc.GovMsgToVaa(request.Messages[0], request.CurrentSetIndex, time.Now())
	require.ErrorContains(t, err, "recipient must be non-zero")
}
