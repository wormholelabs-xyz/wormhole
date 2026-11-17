package accountant

import (
	"context"
	"encoding/hex"
	"testing"
	"time"

	"github.com/certusone/wormhole/node/pkg/common"
	"github.com/stretchr/testify/assert"
	"github.com/stretchr/testify/require"
	"github.com/wormhole-foundation/wormhole/sdk/vaa"
	"go.uber.org/zap"
)

// TEST-ONLY Canton transceiver emitter addresses. Never copy into ntt_config.go.
// testOnlyCantonTransceiverEmitterHex is the shared cross-language emitter vector
// (operator "vector-operator::1220deadbeef", gg "vector-owner::1220cafebabe", emitterId 7).
const (
	testOnlyCantonTransceiverEmitterHex = "45ae8886d1c165b071d3b1202fe81ebedc39f61a6a91a1d13140eb64857f3f2b"
	testOnlyCantonLogOnlyEmitterHex     = "00000000000000000000000000000000000000000000000000000000c0ffee01"
	testOnlyCantonUnlistedEmitterHex    = "00000000000000000000000000000000000000000000000000000000c0ffee02"
)

func testOnlyCantonEmitterConfig() emitterConfig {
	return emitterConfig{
		{chainId: vaa.ChainIDCanton, addr: testOnlyCantonTransceiverEmitterHex},
		{chainId: vaa.ChainIDCanton, addr: testOnlyCantonLogOnlyEmitterHex, logOnly: true},
	}
}

func mustCantonTestAddress(t *testing.T, addrHex string) vaa.Address {
	t.Helper()
	addr, err := vaa.StringToAddress(addrHex)
	require.NoError(t, err)
	return addr
}

func TestNttCantonEmitterRoutesToNttAccountant(t *testing.T) {
	nttPayload, err := hex.DecodeString(goodPayload)
	require.NoError(t, err)
	nonNttPayload := []byte("not an ntt transfer payload")

	directEmitters, arEmitters, err := nttParseEmitters(testOnlyCantonEmitterConfig(), nil)
	require.NoError(t, err)

	tests := []struct {
		name         string
		emitterChain vaa.ChainID
		emitterHex   string
		payload      []byte
		wantCovered  bool
		wantNTT      bool
		wantEnforce  bool
	}{
		{"listed Canton transceiver is enforced NTT", vaa.ChainIDCanton, testOnlyCantonTransceiverEmitterHex, nttPayload, true, true, true},
		{"listed log-only Canton transceiver is unenforced NTT", vaa.ChainIDCanton, testOnlyCantonLogOnlyEmitterHex, nttPayload, true, true, false},
		{"unlisted Canton emitter is not covered", vaa.ChainIDCanton, testOnlyCantonUnlistedEmitterHex, nttPayload, false, false, false},
		{"listed address on another chain is not covered", vaa.ChainIDEthereum, testOnlyCantonTransceiverEmitterHex, nttPayload, false, false, false},
		{"listed Canton transceiver with non-NTT payload is not covered", vaa.ChainIDCanton, testOnlyCantonTransceiverEmitterHex, nonNttPayload, false, false, false},
	}

	for _, tc := range tests {
		t.Run(tc.name, func(t *testing.T) {
			ctx, cancel := context.WithCancel(context.Background())
			defer cancel()
			acct := newAccountantForTest(t, zap.NewNop(), ctx, true, nil, make(chan *common.MessagePublication, 1), nil)
			acct.nttDirectEmitters = directEmitters
			acct.nttArEmitters = arEmitters

			msg := &common.MessagePublication{
				TxID:             hashToTxID("0x06f541f5ecfc43407c31587aa6ac3a689e8960f36dc23c332db5510dfc6a4063"),
				Timestamp:        time.Unix(int64(1654543099), 0),
				Nonce:            0,
				Sequence:         1,
				EmitterChain:     tc.emitterChain,
				EmitterAddress:   mustCantonTestAddress(t, tc.emitterHex),
				ConsistencyLevel: 0,
				Payload:          tc.payload,
			}

			covered, isNTT, enforce := acct.isMessageCoveredByAccountant(msg)
			assert.Equal(t, tc.wantCovered, covered, "covered")
			assert.Equal(t, tc.wantNTT, isNTT, "isNTT")
			assert.Equal(t, tc.wantEnforce, enforce, "enforce")

			canPublish, err := acct.SubmitObservation(msg)
			require.NoError(t, err)
			assert.Equal(t, !tc.wantEnforce, canPublish, "canPublish")

			pending, isPending := acct.pendingTransfers[msg.MessageIDString()]
			require.Equal(t, tc.wantCovered, isPending, "pending")
			if isPending {
				assert.True(t, pending.isNTT, "routed to the NTT accountant")
				assert.Equal(t, tc.wantEnforce, pending.enforceFlag)
			}
		})
	}
}

func TestNttParseEmittersChainValidation(t *testing.T) {
	tests := []struct {
		name      string
		emitters  emitterConfig
		wantError bool
	}{
		{"Canton chain id 75 is accepted", emitterConfig{{chainId: vaa.ChainIDCanton, addr: testOnlyCantonTransceiverEmitterHex}}, false},
		{"numeric chain id 75 is accepted", emitterConfig{{chainId: vaa.ChainID(75), addr: testOnlyCantonTransceiverEmitterHex}}, false},
		{"unset chain id is rejected", emitterConfig{{chainId: vaa.ChainIDUnset, addr: testOnlyCantonTransceiverEmitterHex}}, true},
		{"unknown chain id is rejected", emitterConfig{{chainId: vaa.ChainID(65535), addr: testOnlyCantonTransceiverEmitterHex}}, true},
		{"short Canton address is rejected", emitterConfig{{chainId: vaa.ChainIDCanton, addr: "c0ffee01"}}, true},
		{"non-hex Canton address is rejected", emitterConfig{{chainId: vaa.ChainIDCanton, addr: "zz" + testOnlyCantonTransceiverEmitterHex[2:]}}, true},
		{"duplicate Canton emitter is rejected", emitterConfig{
			{chainId: vaa.ChainIDCanton, addr: testOnlyCantonTransceiverEmitterHex},
			{chainId: vaa.ChainIDCanton, addr: testOnlyCantonTransceiverEmitterHex, logOnly: true},
		}, true},
	}

	for _, tc := range tests {
		t.Run(tc.name, func(t *testing.T) {
			directEmitters, _, err := nttParseEmitters(tc.emitters, nil)
			if tc.wantError {
				require.Error(t, err)
				return
			}
			require.NoError(t, err)
			require.Len(t, directEmitters, len(tc.emitters))
			_, exists := directEmitters[emitterKey{emitterChainId: vaa.ChainIDCanton, emitterAddr: mustCantonTestAddress(t, tc.emitters[0].addr)}]
			assert.True(t, exists)
		})
	}
}

func TestCheckEmittersAreOnCanton(t *testing.T) {
	tests := []struct {
		name      string
		emitters  emitterConfig
		wantError bool
	}{
		{"empty list is accepted", emitterConfig{}, false},
		{"Canton entries are accepted", testOnlyCantonEmitterConfig(), false},
		{"non-Canton entry is rejected", emitterConfig{
			{chainId: vaa.ChainIDCanton, addr: testOnlyCantonTransceiverEmitterHex},
			{chainId: vaa.ChainIDEthereum, addr: testOnlyCantonLogOnlyEmitterHex},
		}, true},
	}

	for _, tc := range tests {
		t.Run(tc.name, func(t *testing.T) {
			err := checkEmittersAreOnCanton(tc.emitters)
			if tc.wantError {
				require.Error(t, err)
				return
			}
			require.NoError(t, err)
		})
	}
}

func TestNttCantonTransceiverEmittersAreMonitoredPerEnvironment(t *testing.T) {
	for _, env := range []common.Environment{common.MainNet, common.TestNet, common.UnsafeDevNet, common.GoTest} {
		t.Run(string(env), func(t *testing.T) {
			cantonEmitters := cantonNttTransceiverEmitters(env)
			require.NoError(t, checkEmittersAreOnCanton(cantonEmitters))

			directEmitters, _, err := nttGetEmitters(env)
			require.NoError(t, err)
			for _, emitter := range cantonEmitters {
				_, exists := directEmitters[emitterKey{emitterChainId: vaa.ChainIDCanton, emitterAddr: mustCantonTestAddress(t, emitter.addr)}]
				assert.True(t, exists, "Canton emitter %s is monitored", emitter.addr)
			}
			_, testOnlyListed := directEmitters[emitterKey{emitterChainId: vaa.ChainIDCanton, emitterAddr: mustCantonTestAddress(t, testOnlyCantonTransceiverEmitterHex)}]
			assert.False(t, testOnlyListed, "TEST-ONLY address leaked into production config")
		})
	}
}
