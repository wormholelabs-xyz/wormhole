package solana

import (
	"encoding/json"
	"testing"

	"github.com/stretchr/testify/assert"
	"github.com/stretchr/testify/require"
)

func TestParseLogsSubscribeData(t *testing.T) {
	eventJson := `
	{
		"jsonrpc": "2.0",
		"method": "logsNotification",
		"params": {
			"result": {
				"context": { "slot": 331947185 },
				"value": {
					"signature": "3qF4PdV7G1aTHddS6VqPsWa6w4eQtvudwDEeYcgh6znQUBy76xo8uWEFAtBd39qc6jk3wR71MKXN8afRF41mGYXB",
					"err": null,
					"logs": [
						"Program ComputeBudget111111111111111111111111111111 invoke [1]",
						"Program ComputeBudget111111111111111111111111111111 success",
						"Program ComputeBudget111111111111111111111111111111 invoke [1]",
						"Program ComputeBudget111111111111111111111111111111 success",
						"Program BLZRi6frs4X4DNLw56V4EXai1b6QVESN1BhHBTYM9VcY invoke [1]",
						"Program log: Instruction: PostUnlock",
						"Program 11111111111111111111111111111111 invoke [2]",
						"Program 11111111111111111111111111111111 success",
						"Program worm2ZoG2kUd4vFXhvjh93UUH596ayRfgQ2MgjNMTth invoke [2]",
						"Program log: Sequence: 149587",
						"Program 11111111111111111111111111111111 invoke [3]",
						"Program 11111111111111111111111111111111 success",
						"Program 11111111111111111111111111111111 invoke [3]",
						"Program 11111111111111111111111111111111 success",
						"Program 11111111111111111111111111111111 invoke [3]",
						"Program 11111111111111111111111111111111 success",
						"Program worm2ZoG2kUd4vFXhvjh93UUH596ayRfgQ2MgjNMTth consumed 27143 of 33713 compute units",
						"Program worm2ZoG2kUd4vFXhvjh93UUH596ayRfgQ2MgjNMTth success",
						"Program log: unlock message seq: 149587",
						"Program BLZRi6frs4X4DNLw56V4EXai1b6QVESN1BhHBTYM9VcY consumed 88681 of 94020 compute units",
						"Program BLZRi6frs4X4DNLw56V4EXai1b6QVESN1BhHBTYM9VcY success"
					]
				}
			},
			"subscription": 81211
		}
	}
	`

	var evt LogsSubscriptionData
	err := json.Unmarshal([]byte(eventJson), &evt)
	require.NoError(t, err)

	assert.Equal(t, int64(331947185), evt.Params.Result.Context.Slot)
	assert.Equal(t, "3qF4PdV7G1aTHddS6VqPsWa6w4eQtvudwDEeYcgh6znQUBy76xo8uWEFAtBd39qc6jk3wR71MKXN8afRF41mGYXB", evt.Params.Result.Value.Signature)
	require.Equal(t, 21, len(evt.Params.Result.Value.Logs))
	require.Equal(t, "Program worm2ZoG2kUd4vFXhvjh93UUH596ayRfgQ2MgjNMTth invoke [2]", evt.Params.Result.Value.Logs[8])
	require.Equal(t, "Program log: Sequence: 149587", evt.Params.Result.Value.Logs[9])
}
