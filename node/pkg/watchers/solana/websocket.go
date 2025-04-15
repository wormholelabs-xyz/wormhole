package solana

/*
Notes:
https://solana.com/docs/rpc/websocket/logssubscribe

Issues:
- The websocket it timing out after a minute with "failed to get reader: failed to read frame header: EOF".

- They are sometimes reporting things twice, with different context slots:
2025-04-08T09:45:57.368-0500    INFO    root.fogo-confirmed_watch       TEST: log event received   {"data": "{\"jsonrpc\":\"2.0\",\"method\":\"logsNotification\",\"params\":{\"result\":{\"context\":{\"slot\":20722852},\"value\":{\"signature\":\"5T9ZheeiygUmYBUPueZSJJKVW9NwPVGDdpM5gmDsfcFDgxpy4N89DwbKY5o55cG81nrZEFaV7xuLjsfFUJZMvT1T\",\"err\":null,\"logs\":[\"Program 11111111111111111111111111111111 invoke [1]\",\"Program 11111111111111111111111111111111 success\",\"Program BhnQyKoQQgpuRTRo6D8Emz93PvXCYfVgHhnrR4T3qhw4 invoke [1]\",\"Program log: Sequence: 10\",\"Program 11111111111111111111111111111111 invoke [2]\",\"Program 11111111111111111111111111111111 success\",\"Program 11111111111111111111111111111111 invoke [2]\",\"Program 11111111111111111111111111111111 success\",\"Program 11111111111111111111111111111111 invoke [2]\",\"Program 11111111111111111111111111111111 success\",\"Program BhnQyKoQQgpuRTRo6D8Emz93PvXCYfVgHhnrR4T3qhw4 consumed 18781 of 399850 compute units\",\"Program BhnQyKoQQgpuRTRo6D8Emz93PvXCYfVgHhnrR4T3qhw4 success\"]}},\"subscription\":46}}"}
2025-04-08T09:46:04.898-0500    INFO    root.fogo-confirmed_watch       TEST: log event received   {"data": "{\"jsonrpc\":\"2.0\",\"method\":\"logsNotification\",\"params\":{\"result\":{\"context\":{\"slot\":20723040},\"value\":{\"signature\":\"5T9ZheeiygUmYBUPueZSJJKVW9NwPVGDdpM5gmDsfcFDgxpy4N89DwbKY5o55cG81nrZEFaV7xuLjsfFUJZMvT1T\",\"err\":null,\"logs\":[\"Program 11111111111111111111111111111111 invoke [1]\",\"Program 11111111111111111111111111111111 success\",\"Program BhnQyKoQQgpuRTRo6D8Emz93PvXCYfVgHhnrR4T3qhw4 invoke [1]\",\"Program log: Sequence: 10\",\"Program 11111111111111111111111111111111 invoke [2]\",\"Program 11111111111111111111111111111111 success\",\"Program 11111111111111111111111111111111 invoke [2]\",\"Program 11111111111111111111111111111111 success\",\"Program 11111111111111111111111111111111 invoke [2]\",\"Program 11111111111111111111111111111111 success\",\"Program BhnQyKoQQgpuRTRo6D8Emz93PvXCYfVgHhnrR4T3qhw4 consumed 18781 of 399850 compute units\",\"Program BhnQyKoQQgpuRTRo6D8Emz93PvXCYfVgHhnrR4T3qhw4 success\"]}},\"subscription\":46}}"}

The log events come in long before the account update (and if the tx fails, we'll see the log events but not the account update):
2025-04-10T12:53:14.814-0500    INFO    root.fogo-confirmed_watch       TEST: log event received   {"data": "{\"jsonrpc\":\"2.0\",\"method\":\"logsNotification\",\"params\":{\"result\":{\"context\":{\"slot\":25029192},\"value\":{\"signature\":\"PcME25C2GcTDLS9YZSnfcmoJ6iHULR8XdY67qa8dfSGdNV7pNV4da92weNfeb6cwZFsw4VTaEVYeRg8P78tU51x\",\"err\":null,\"logs\":[\"Program 11111111111111111111111111111111 invoke [1]\",\"Program 11111111111111111111111111111111 success\",\"Program BhnQyKoQQgpuRTRo6D8Emz93PvXCYfVgHhnrR4T3qhw4 invoke [1]\",\"Program log: Sequence: 23\",\"Program 11111111111111111111111111111111 invoke [2]\",\"Program 11111111111111111111111111111111 success\",\"Program 11111111111111111111111111111111 invoke [2]\",\"Program 11111111111111111111111111111111 success\",\"Program 11111111111111111111111111111111 invoke [2]\",\"Program 11111111111111111111111111111111 success\",\"Program BhnQyKoQQgpuRTRo6D8Emz93PvXCYfVgHhnrR4T3qhw4 consumed 18781 of 399850 compute units\",\"Program BhnQyKoQQgpuRTRo6D8Emz93PvXCYfVgHhnrR4T3qhw4 success\"]}},\"subscription\":219}}"}
2025-04-10T12:53:16.054-0500    INFO    root.fogo-finalized_watch       TEST: log event received   {"data": "{\"jsonrpc\":\"2.0\",\"method\":\"logsNotification\",\"params\":{\"result\":{\"context\":{\"slot\":25029192},\"value\":{\"signature\":\"PcME25C2GcTDLS9YZSnfcmoJ6iHULR8XdY67qa8dfSGdNV7pNV4da92weNfeb6cwZFsw4VTaEVYeRg8P78tU51x\",\"err\":null,\"logs\":[\"Program 11111111111111111111111111111111 invoke [1]\",\"Program 11111111111111111111111111111111 success\",\"Program BhnQyKoQQgpuRTRo6D8Emz93PvXCYfVgHhnrR4T3qhw4 invoke [1]\",\"Program log: Sequence: 23\",\"Program 11111111111111111111111111111111 invoke [2]\",\"Program 11111111111111111111111111111111 success\",\"Program 11111111111111111111111111111111 invoke [2]\",\"Program 11111111111111111111111111111111 success\",\"Program 11111111111111111111111111111111 invoke [2]\",\"Program 11111111111111111111111111111111 success\",\"Program BhnQyKoQQgpuRTRo6D8Emz93PvXCYfVgHhnrR4T3qhw4 consumed 18781 of 399850 compute units\",\"Program BhnQyKoQQgpuRTRo6D8Emz93PvXCYfVgHhnrR4T3qhw4 success\"]}},\"subscription\":220}}"}
2025-04-10T12:53:28.732-0500    INFO    root.fogo-confirmed_watch       TEST: account event received    {"msg": "{\"jsonrpc\":\"2.0\",\"method\":\"programNotification\",\"params\":{\"result\":{\"context\":{\"slot\":25029540},\"value\":{\"pubkey\":\"fZxfHeZRMLU6paNA2QjqygNSu53Euvds3jaeD1Kakkg\",\"account\":{\"lamports\":1057920,\"data\":[\"AAAAAGChDQAAAAAAgFEBAGQAAAAAAAAA\",\"base64\"],\"owner\":\"BhnQyKoQQgpuRTRo6D8Emz93PvXCYfVgHhnrR4T3qhw4\",\"executable\":false,\"rentEpoch\":18446744073709551615,\"space\":24}}},\"subscription\":221}}"}
2025-04-10T12:53:28.732-0500    INFO    root.fogo-confirmed_watch       TEST: account event received    {"msg": "{\"jsonrpc\":\"2.0\",\"method\":\"programNotification\",\"params\":{\"result\":{\"context\":{\"slot\":25029540},\"value\":{\"pubkey\":\"4eALVAiNyaAS2p2NWjDPafEVmHzL6eMa9Zu4u9kyp5MH\",\"account\":{\"lamports\":1628640,\"data\":[\"bXNnAAEAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAACYBfhnAAAAABcAAAAAAAAAMwCDcYt+yJYXtwQGheAb3MoDIUAimA2q6RNA4MP4QMAF7wsAAABoZWxsbyB3b3JsZA==\",\"base64\"],\"owner\":\"BhnQyKoQQgpuRTRo6D8Emz93PvXCYfVgHhnrR4T3qhw4\",\"executable\":false,\"rentEpoch\":18446744073709551615,\"space\":106}}},\"subscription\":221}}"}

Whereas on Solana, they come in close together:

2025-04-10T13:12:38.212-0500    INFO    root.solana-finalized_watch     TEST: account event received    {"msg": "{\"jsonrpc\":\"2.0\",\"method\":\"programNotification\",\"params\":{\"result\":{\"context\":{\"slot\":332592469},\"value\":{\"pubkey\":\"E18u2P7AdwdctWSRPRZCyipQzqgSVQXafCseCxkamVo9\",\"account\":{\"lamports\":3062400,\"data\":[\"bXNnACAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAICvhnAAAAAFoDAAAAAAAAAQA/24IIcr901McaOfEk5CZ+6eSu1t5uEAoV8UysEa0eHNkAAACZRf8QC8FPkiqyzB/+Aqtxbn8AuQv2AU9qIYgKXT6phfWRzyUAAAAAAAAAAAAAAAC8UfdheKVoEf3+ldOJfmrCsR27YgCRR/w8SItqKp/SZ/MUCobz1TQCMaPa8UFoKuzVdAAQg1UD2DqGRt0xAdjnrCOn17KlpOUrNxUERy/oJtca2jBiBwBPmU5UVAgAAABOMGVP0EZ6tJ+oiCqfnqXtCM2yEz3Fq9swKHEKqzYb0uT2IrSLAAAAAAAAAAAAAAAA6D91kH+0xXVBT6b1z+jO8k3FhwwAFwAA\",\"base64\"],\"owner\":\"worm2ZoG2kUd4vFXhvjh93UUH596ayRfgQ2MgjNMTth\",\"executable\":false,\"rentEpoch\":18446744073709551615,\"space\":312}}},\"subscription\":93855}}"}
2025-04-10T13:12:38.212-0500    INFO    root.solana-finalized_watch     message observed        {"account": "5f6fQ6mnR9LVSVdS6HHNxzbFbDBj1LvYdbzapjbUnRzY", "timestamp": "2025-04-10T13:12:24.000-0500", "nonce": 0, "sequence": 858, "emitter_chain": "solana", "emitter_address": "3fdb820872bf74d4c71a39f124e4267ee9e4aed6de6e100a15f14cac11ad1e1c", "isReobservation": false, "payload": "mUX/EAvBT5Iqsswf/gKrcW5/ALkL9gFPaiGICl0+qYX1kc8lAAAAAAAAAAAAAAAAvFH3YXilaBH9/pXTiX5qwrEdu2IAkUf8PEiLaiqf0mfzFAqG89U0AjGj2vFBaCrs1XQAEINVA9g6hkbdMQHY56wjp9eypaTlKzcVBEcv6CbXGtowYgcAT5lOVFQIAAAATjBlT9BGerSfqIgqn56l7QjNshM9xavbMChxCqs2G9Lk9iK0iwAAAAAAAAAAAAAAAOg/dZB/tMV1QU+m9c/ozvJNxYcMABcAAA==", "consistency_level": 32}
                                                                                                                                                                                                                                                                                                                                                 2025-04-10T13:12:38.596-0500    INFO    root.solana-finalized_watch     TEST    {"data": "{\"jsonrpc\":\"2.0\",\"method\":\"logsNotification\",\"params\":{\"result\":{\"context\":{\"slot\":332592469},\"value\":{\"signature\":\"5h9jY6tYRt2ewDKqyjKEif9zYjSNSWwiCUEUnoQfgnQAaS6Nx177A8GmDo8Lf8Nd5QPjYeY1BiZA4sEZEBMbhuSh\",\"err\":null,\"logs\":[\"Program ComputeBudget111111111111111111111111111111 invoke [1]\",\"Program ComputeBudget111111111111111111111111111111 success\",\"Program ComputeBudget111111111111111111111111111111 invoke [1]\",\"Program ComputeBudget111111111111111111111111111111 success\",\"Program 9YMqQ6gsN3za6go4XddbX7okCuZG4mv94NcgGP5rpVcR invoke [1]\",\"Program log: instruction: CheckSlot\",\"Program 9YMqQ6gsN3za6go4XddbX7okCuZG4mv94NcgGP5rpVcR consumed 1971 of 1399700 compute units\",\"Program 9YMqQ6gsN3za6go4XddbX7okCuZG4mv94NcgGP5rpVcR success\",\"Program 11111111111111111111111111111111 invoke [1]\",\"Program 11111111111111111111111111111111 success\",\"Program TokenkegQfeZyiNwAJbNbGKPFXCWuBvf9Ss623VQ5DA invoke [1]\",\"Program log: Instruction: Approve\",\"Program TokenkegQfeZyiNwAJbNbGKPFXCWuBvf9Ss623VQ5DA consumed 2929 of 1397579 compute units\",\"Program TokenkegQfeZyiNwAJbNbGKPFXCWuBvf9Ss623VQ5DA success\",\"Program ntT5xGC7XEuR8Po9U3Umze12T9LBdaTCuEc9Cby6qPa invoke [1]\",\"Program log: Instruction: TransferBurn\",\"Program 11111111111111111111111111111111 invoke [2]\",\"Program 11111111111111111111111111111111 success\",\"Program TokenkegQfeZyiNwAJbNbGKPFXCWuBvf9Ss623VQ5DA invoke [2]\",\"Program log: Instruction: TransferChecked\",\"Program TokenkegQfeZyiNwAJbNbGKPFXCWuBvf9Ss623VQ5DA consumed 6263 of 1367814 compute units\",\"Program TokenkegQfeZyiNwAJbNbGKPFXCWuBvf9Ss623VQ5DA success\",\"Program TokenkegQfeZyiNwAJbNbGKPFXCWuBvf9Ss623VQ5DA invoke [2]\",\"Program log: Instruction: Burn\",\"Program TokenkegQfeZyiNwAJbNbGKPFXCWuBvf9Ss623VQ5DA consumed 4707 of 1359131 compute units\",\"Program TokenkegQfeZyiNwAJbNbGKPFXCWuBvf9Ss623VQ5DA success\",\"Program ntT5xGC7XEuR8Po9U3Umze12T9LBdaTCuEc9Cby6qPa consumed 45084 of 1394650 compute units\",\"Program ntT5xGC7XEuR8Po9U3Umze12T9LBdaTCuEc9Cby6qPa success\",\"Program ntT5xGC7XEuR8Po9U3Umze12T9LBdaTCuEc9Cby6qPa invoke [1]\",\"Program log: Instruction: ReleaseWormholeOutbound\",\"Program 11111111111111111111111111111111 invoke [2]\",\"Program 11111111111111111111111111111111 success\",\"Program worm2ZoG2kUd4vFXhvjh93UUH596ayRfgQ2MgjNMTth invoke [2]\",\"Program log: Sequence: 858\",\"Program 11111111111111111111111111111111 invoke [3]\",\"Program 11111111111111111111111111111111 success\",\"Program 11111111111111111111111111111111 invoke [3]\",\"Program 11111111111111111111111111111111 success\",\"Program 11111111111111111111111111111111 invoke [3]\",\"Program 11111111111111111111111111111111 success\",\"Program worm2ZoG2kUd4vFXhvjh93UUH596ayRfgQ2MgjNMTth consumed 27840 of 1330391 compute units\",\"Program worm2ZoG2kUd4vFXhvjh93UUH596ayRfgQ2MgjNMTth success\",\"Program ntT5xGC7XEuR8Po9U3Umze12T9LBdaTCuEc9Cby6qPa consumed 48219 of 1349566 compute units\",\"Program ntT5xGC7XEuR8Po9U3Umze12T9LBdaTCuEc9Cby6qPa success\"]}},\"subscription\":102101}}"}
2025-04-10T13:12:38.692-0500    INFO    root.solana-finalized_watch     transaction contained messages  {"numObservations": 1, "signature": "5h9jY6tYRt2ewDKqyjKEif9zYjSNSWwiCUEUnoQfgnQAaS6Nx177A8GmDo8Lf8Nd5QPjYeY1BiZA4sEZEBMbhuSh"}
2025-04-10T13:12:38.757-0500    INFO    root.solana-finalized_watch     message observed        {"account": "E18u2P7AdwdctWSRPRZCyipQzqgSVQXafCseCxkamVo9", "timestamp": "2025-04-10T13:12:24.000-0500", "nonce": 0, "sequence": 858, "emitter_chain": "solana", "emitter_address": "3fdb820872bf74d4c71a39f124e4267ee9e4aed6de6e100a15f14cac11ad1e1c", "isReobservation": false, "payload": "mUX/EAvBT5Iqsswf/gKrcW5/ALkL9gFPaiGICl0+qYX1kc8lAAAAAAAAAAAAAAAAvFH3YXilaBH9/pXTiX5qwrEdu2IAkUf8PEiLaiqf0mfzFAqG89U0AjGj2vFBaCrs1XQAEINVA9g6hkbdMQHY56wjp9eypaTlKzcVBEcv6CbXGtowYgcAT5lOVFQIAAAATjBlT9BGerSfqIgqn56l7QjNshM9xavbMChxCqs2G9Lk9iK0iwAAAAAAAAAAAAAAAOg/dZB/tMV1QU+m9c/ozvJNxYcMABcAAA==", "consistency_level": 32}

Notes of strange things seen on both Solana and Fogo:
- We see the message on both watchers and we end up ignoring it on one of them, based on the commitment.
2025-04-09T12:10:25.674-0500    INFO    root.fogo-confirmed_watch       transaction contained messages  {"numObservations": 1, "data": "{\"jsonrpc\":\"2.0\",\"method\":\"logsNotification\",\"params\":{\"result\":{\"context\":{\"slot\":22805266},\"value\":{\"signature\":\"4UYyzrimBMbhgaAMVp7rFL7xpHnit7iHyHmLxvZkLzuwhnfpHbgWih6ZkUE8r1aeP6cWgFWs9MUfeu6ECePJTfAC\",\"err\":null,\"logs\":[\"Program 11111111111111111111111111111111 invoke [1]\",\"Program 11111111111111111111111111111111 success\",\"Program BhnQyKoQQgpuRTRo6D8Emz93PvXCYfVgHhnrR4T3qhw4 invoke [1]\",\"Program log: Sequence: 19\",\"Program 11111111111111111111111111111111 invoke [2]\",\"Program 11111111111111111111111111111111 success\",\"Program 11111111111111111111111111111111 invoke [2]\",\"Program 11111111111111111111111111111111 success\",\"Program 11111111111111111111111111111111 invoke [2]\",\"Program 11111111111111111111111111111111 success\",\"Program BhnQyKoQQgpuRTRo6D8Emz93PvXCYfVgHhnrR4T3qhw4 consumed 18781 of 399850 compute units\",\"Program BhnQyKoQQgpuRTRo6D8Emz93PvXCYfVgHhnrR4T3qhw4 success\"]}},\"subscription\":69}}"}
2025-04-09T12:10:25.879-0500    INFO    root.fogo-confirmed_watch       transaction contained messages  {"numObservations": 1, "data": "{\"jsonrpc\":\"2.0\",\"method\":\"logsNotification\",\"params\":{\"result\":{\"context\":{\"slot\":22805267},\"value\":{\"signature\":\"4UYyzrimBMbhgaAMVp7rFL7xpHnit7iHyHmLxvZkLzuwhnfpHbgWih6ZkUE8r1aeP6cWgFWs9MUfeu6ECePJTfAC\",\"err\":null,\"logs\":[\"Program 11111111111111111111111111111111 invoke [1]\",\"Program 11111111111111111111111111111111 success\",\"Program BhnQyKoQQgpuRTRo6D8Emz93PvXCYfVgHhnrR4T3qhw4 invoke [1]\",\"Program log: Sequence: 19\",\"Program 11111111111111111111111111111111 invoke [2]\",\"Program 11111111111111111111111111111111 success\",\"Program 11111111111111111111111111111111 invoke [2]\",\"Program 11111111111111111111111111111111 success\",\"Program 11111111111111111111111111111111 invoke [2]\",\"Program 11111111111111111111111111111111 success\",\"Program BhnQyKoQQgpuRTRo6D8Emz93PvXCYfVgHhnrR4T3qhw4 consumed 18781 of 399850 compute units\",\"Program BhnQyKoQQgpuRTRo6D8Emz93PvXCYfVgHhnrR4T3qhw4 success\"]}},\"subscription\":69}}"}
2025-04-09T12:10:26.920-0500    ERROR   root.fogo-finalized_watch       transaction did not contain any messages        {"data": "{\"jsonrpc\":\"2.0\",\"method\":\"logsNotification\",\"params\":{\"result\":{\"context\":{\"slot\":22805266},\"value\":{\"signature\":\"4UYyzrimBMbhgaAMVp7rFL7xpHnit7iHyHmLxvZkLzuwhnfpHbgWih6ZkUE8r1aeP6cWgFWs9MUfeu6ECePJTfAC\",\"err\":null,\"logs\":[\"Program 11111111111111111111111111111111 invoke [1]\",\"Program 11111111111111111111111111111111 success\",\"Program BhnQyKoQQgpuRTRo6D8Emz93PvXCYfVgHhnrR4T3qhw4 invoke [1]\",\"Program log: Sequence: 19\",\"Program 11111111111111111111111111111111 invoke [2]\",\"Program 11111111111111111111111111111111 success\",\"Program 11111111111111111111111111111111 invoke [2]\",\"Program 11111111111111111111111111111111 success\",\"Program 11111111111111111111111111111111 invoke [2]\",\"Program 11111111111111111111111111111111 success\",\"Program BhnQyKoQQgpuRTRo6D8Emz93PvXCYfVgHhnrR4T3qhw4 consumed 18781 of 399850 compute units\",\"Program BhnQyKoQQgpuRTRo6D8Emz93PvXCYfVgHhnrR4T3qhw4 success\"]}},\"subscription\":70}}"}
2025-04-09T12:10:26.946-0500    ERROR   root.fogo-finalized_watch       transaction did not contain any messages        {"data": "{\"jsonrpc\":\"2.0\",\"method\":\"logsNotification\",\"params\":{\"result\":{\"context\":{\"slot\":22805267},\"value\":{\"signature\":\"4UYyzrimBMbhgaAMVp7rFL7xpHnit7iHyHmLxvZkLzuwhnfpHbgWih6ZkUE8r1aeP6cWgFWs9MUfeu6ECePJTfAC\",\"err\":null,\"logs\":[\"Program 11111111111111111111111111111111 invoke [1]\",\"Program 11111111111111111111111111111111 success\",\"Program BhnQyKoQQgpuRTRo6D8Emz93PvXCYfVgHhnrR4T3qhw4 invoke [1]\",\"Program log: Sequence: 19\",\"Program 11111111111111111111111111111111 invoke [2]\",\"Program 11111111111111111111111111111111 success\",\"Program 11111111111111111111111111111111 invoke [2]\",\"Program 11111111111111111111111111111111 success\",\"Program 11111111111111111111111111111111 invoke [2]\",\"Program 11111111111111111111111111111111 success\",\"Program BhnQyKoQQgpuRTRo6D8Emz93PvXCYfVgHhnrR4T3qhw4 consumed 18781 of 399850 compute units\",\"Program BhnQyKoQQgpuRTRo6D8Emz93PvXCYfVgHhnrR4T3qhw4 success\"]}},\"subscription\":70}}"}

*/

import (
	"context"
	"errors"
	"fmt"
	"time"

	"encoding/json"

	"github.com/certusone/wormhole/node/pkg/common"
	"github.com/certusone/wormhole/node/pkg/p2p"
	"github.com/gagliardetto/solana-go"
	"github.com/gagliardetto/solana-go/rpc"
	"github.com/google/uuid"
	"github.com/mr-tron/base58"
	"github.com/wormhole-foundation/wormhole/sdk/vaa"
	"go.uber.org/zap"
	"nhooyr.io/websocket"
)

type (
	LogsSubscriptionData struct {
		Jsonrpc string `json:"jsonrpc"`
		Method  string `json:"method"`
		Params  *struct {
			Result struct {
				Context struct {
					Slot int64 `json:"slot"`
				} `json:"context"`
				Value struct {
					Signature string                 `json:"signature"`
					Err       map[string]interface{} `json:"err"`
					Logs      []string               `json:"logs"`
				} `json:"value"`
			} `json:"result"`
			Subscription int `json:"subscription"`
		} `json:"params"`
	}
)

// setUpLogsWebSocket creates a websocket used to subscribe for logs from the core contract.
func (s *SolanaWatcher) setUpLogsWebSocket(ctx context.Context) error {
	if s.chainID == vaa.ChainIDPythNet {
		panic("unsupported chain id")
	}

	ws, err := s.subscribeForLogs(ctx)
	if err != nil {
		return err
	}

	common.RunWithScissors(ctx, s.errC, "SolanaLogsDataPump", func(ctx context.Context) error {
		s.logger.Info("TEST: entering SolanaLogsDataPump")
		defer ws.Close(websocket.StatusNormalClosure, "")

		for {
			select {
			case <-ctx.Done():
				s.logger.Info("TEST: exiting SolanaLogsDataPump")
				return nil
			default:
				if msg, err := s.readLogsWebSocket(ctx, ws); err != nil {
					if !errors.Is(err, context.Canceled) {
						s.logger.Error("failed to read logs websocket", zap.Error(err))
					}
					s.logger.Info("TEST: exiting SolanaLogsDataPump on error", zap.Error(err))
					return err
				} else if msg != nil {
					s.logsDataPump <- msg
				}
			}
		}
	})

	return nil
}

// subscribeForLogs subscribes for log events from the core contract.
func (s *SolanaWatcher) subscribeForLogs(ctx context.Context) (*websocket.Conn, error) {
	ws, _, err := websocket.Dial(ctx, s.wsUrl, nil)

	if err != nil {
		return nil, err
	}

	s.subId = uuid.New().String()

	var p = fmt.Sprintf(`{ "jsonrpc": "2.0", "id": "%s", "method": "logsSubscribe", "params": [ { "mentions": ["%s"] }, { "commitment": "%s" } ] }`, s.subId, s.contract.String(), string(s.commitment))

	s.logger.Info(fmt.Sprintf("%s watcher subscribing to logs via websocket", s.chainID.String()), zap.String("url", s.wsUrl), zap.String("request", p))

	if err := ws.Write(ctx, websocket.MessageText, []byte(p)); err != nil {
		s.logger.Error(fmt.Sprintf("write: %s", err.Error()))
		return nil, err
	}
	return ws, nil
}

// readLogsWebSocket Reads the logs websocket for an event. It doesn't use a timeout context because we don't know how long it will be before we get anything.
func (s *SolanaWatcher) readLogsWebSocket(ctx context.Context, ws *websocket.Conn) ([]byte, error) {
	rCtx, cancel := context.WithCancel(ctx)
	defer cancel()
	_, msg, err := ws.Read(rCtx)
	return msg, err
}

// processLogsSubscriptionData handles data received from the `logssubscribe` websocket subscription.
func (s *SolanaWatcher) processLogsSubscriptionData(_ context.Context, logger *zap.Logger, data []byte) error {
	// Check for an error being reported.
	var errEvt EventSubscriptionError
	err := json.Unmarshal(data, &errEvt)
	if err != nil {
		logger.Error("failed to unmarshal logs subscription error report", zap.Error(err))
		p2p.DefaultRegistry.AddErrorCount(s.chainID, 1)
		return err
	}

	if errEvt.Error.Message != nil {
		return errors.New(*errEvt.Error.Message)
	}

	// Unmarshal the event.
	var evt LogsSubscriptionData
	err = json.Unmarshal(data, &evt)
	if err != nil {
		logger.Error("failed to unmarshal logs subscription event", zap.Error(err), zap.String("data", string(data)))
		p2p.DefaultRegistry.AddErrorCount(s.chainID, 1)
		return err
	}

	if evt.Params == nil {
		return nil
	}

	// If it doesn't look like it contains a core PostMessage, drop it.
	if !isPossibleWormholeMessage(s.whLogPrefix, evt.Params.Result.Value.Logs) {
		return nil
	}

	s.logger.Info("TEST: log event received", zap.String("data", string(data)))

	sigBytes, err := base58.Decode(evt.Params.Result.Value.Signature)
	if err != nil {
		logger.Error("failed to decode signature for subscription event", zap.Error(err), zap.String("data", string(data)))
		return fmt.Errorf("failed to decode signature for subscription event: %v", err)
	}
	signature := solana.SignatureFromBytes(sigBytes)

	go s.processLogsTransactionWithRetry(signature, data)
	return nil
}

// processLogsTransactionWithRetry is called for log subscriptions. It attempts to observe the transaction, retrying if the transaction is not found yet.
// Once the transaction is successfully read, it passes it off to the standard transaction processing code.
func (s *SolanaWatcher) processLogsTransactionWithRetry(signature solana.Signature, data []byte) {
	for count := range maxRetries {
		if count != 0 {
			time.Sleep(retryDelay)
		}

		rCtx, cancel := context.WithTimeout(s.ctx, rpcTimeout)
		version := uint64(0)
		result, err := s.rpcClient.GetTransaction(
			rCtx,
			signature,
			&rpc.GetTransactionOpts{
				MaxSupportedTransactionVersion: &version,
				Commitment:                     s.commitment,
				Encoding:                       solana.EncodingBase64,
			},
		)
		cancel()
		if err != nil {
			if errors.Is(err, rpc.ErrNotFound) {
				continue
			}

			if errors.Is(err, context.Canceled) {
				s.logger.Warn("context canceled when getting transaction for subscription event", zap.Stringer("signature", signature), zap.Error(err), zap.String("data", string(data)))
				continue
			}

			s.logger.Error("failed to get transaction for subscription event", zap.Stringer("signature", signature), zap.Error(err), zap.String("data", string(data)))
			return
		}

		tx, err := result.Transaction.GetTransaction()
		if err != nil {
			s.logger.Error("failed to extract transaction for subscription event", zap.Error(err), zap.String("data", string(data)))
			return
		}

		numObservations := s.processTransaction(s.ctx, s.rpcClient, tx, result.Meta, result.Slot, false)
		if numObservations != 0 {
			if _, exists := s.observations[signature]; exists {
				s.logger.Error("duplicate transaction", zap.Stringer("signature", signature))
			} else {
				s.logger.Info("transaction contained messages", zap.Uint32("numObservations", numObservations), zap.Stringer("signature", signature))
				s.observations[signature] = struct{}{}
			}
		}
		return
	}

	s.logger.Error("failed to observe any messages in expected transaction", zap.Stringer("signature", signature))
}
