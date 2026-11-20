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

type coreBridgeTransferFeesTemplateOutput struct {
	stdout []byte
	stderr []byte
}

func setCoreBridgeTransferFeesFlags(t *testing.T, targetChain string, amount string, recipient string) {
	originalTargetChain := *coreBridgeTransferFeesChainId
	originalAmount := *coreBridgeTransferFeesAmount
	originalRecipient := *coreBridgeTransferFeesRecipient
	t.Cleanup(func() {
		*coreBridgeTransferFeesChainId = originalTargetChain
		*coreBridgeTransferFeesAmount = originalAmount
		*coreBridgeTransferFeesRecipient = originalRecipient
	})
	*coreBridgeTransferFeesChainId = targetChain
	*coreBridgeTransferFeesAmount = amount
	*coreBridgeTransferFeesRecipient = recipient
}

func captureCoreBridgeTransferFeesTemplate(t *testing.T, targetChain string, amount string, recipient string) coreBridgeTransferFeesTemplateOutput {
	setCoreBridgeTransferFeesFlags(t, targetChain, amount, recipient)

	originalStdout, originalStderr := os.Stdout, os.Stderr
	stdoutReader, stdoutWriter, err := os.Pipe()
	require.NoError(t, err)
	stderrReader, stderrWriter, err := os.Pipe()
	require.NoError(t, err)
	os.Stdout, os.Stderr = stdoutWriter, stderrWriter
	t.Cleanup(func() { os.Stdout, os.Stderr = originalStdout, originalStderr })
	runCoreBridgeTransferFeesTemplate(nil, nil)
	os.Stdout, os.Stderr = originalStdout, originalStderr
	require.NoError(t, stdoutWriter.Close())
	require.NoError(t, stderrWriter.Close())

	stdout, err := io.ReadAll(stdoutReader)
	require.NoError(t, err)
	require.NotEmpty(t, stdout)
	stderr, err := io.ReadAll(stderrReader)
	require.NoError(t, err)
	return coreBridgeTransferFeesTemplateOutput{stdout: stdout, stderr: stderr}
}

func TestCoreBridgeTransferFeesTemplateChainZeroIsExplicitAndWarns(t *testing.T) {
	const recipient = "0x00000000000000000000000000000000000000000000000000000000deadbeef"
	cases := []struct {
		name                     string
		targetChain              string
		expectedChainLinePattern string
		expectChainWarning       bool
	}{
		{"chain 0", "0", `chain_id:\s+0\n`, true},
		{"solana", "solana", `chain_id:\s+1\n`, false},
	}
	for _, testCase := range cases {
		t.Run(testCase.name, func(t *testing.T) {
			output := captureCoreBridgeTransferFeesTemplate(t, testCase.targetChain, "1", recipient)
			require.Regexp(t, testCase.expectedChainLinePattern, string(output.stdout))
			stderrText := string(output.stderr)
			if !testCase.expectChainWarning {
				require.Empty(t, stderrText)
				return
			}
			for _, expectedWarningPart := range []string{"every chain", "base unit", "20 bytes", "Terra2", "Injective", "Sei"} {
				require.Contains(t, stderrText, expectedWarningPart)
			}
		})
	}
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
			templateOutput := captureCoreBridgeTransferFeesTemplate(t, testCase.targetChain, testCase.amount, "0x"+recipientHex).stdout

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
	templateOutput := captureCoreBridgeTransferFeesTemplate(t, "0", "0", "0x00").stdout

	var request nodev1.InjectGovernanceVAARequest
	require.NoError(t, prototext.Unmarshal(templateOutput, &request))
	require.Len(t, request.Messages, 1)

	_, err := adminrpc.GovMsgToVaa(request.Messages[0], request.CurrentSetIndex, time.Now())
	require.ErrorContains(t, err, "recipient must be non-zero")
}
