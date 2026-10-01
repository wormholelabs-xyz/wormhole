package guardiand

import (
	"testing"

	"github.com/stretchr/testify/assert"
	"github.com/stretchr/testify/require"
)

const (
	testSolanaAccountantProgram = "US517G5965aydkZ46HS38QLi7UQiSojurfbQfKCELFx"
	testSolanaNoreplayProgram   = "repMHgR5BEpGLeZvM5iGoNNDPw4eu2BS6sXJzaC8K4t"
	testSolanaCoreBridgeProgram = "worm2ZoG2kUd4vFXhvjh93UUH596ayRfgQ2MgjNMTth"
)

func TestParseAccountantSolanaProgramIDs(t *testing.T) {
	zeroAddress := "11111111111111111111111111111111"
	acct := testSolanaAccountantProgram
	norep := testSolanaNoreplayProgram
	core := testSolanaCoreBridgeProgram

	tests := []struct {
		name       string
		contract   string
		noreplay   string
		coreBridge string
		wantErr    bool
	}{
		{name: "three distinct programs", contract: acct, noreplay: norep, coreBridge: core},
		{name: "contract is not base58", contract: "not-an-address", noreplay: norep, coreBridge: core, wantErr: true},
		{name: "contract is the zero address", contract: zeroAddress, noreplay: norep, coreBridge: core, wantErr: true},
		{name: "contract equals noreplay", contract: acct, noreplay: acct, coreBridge: core, wantErr: true},
		{name: "core bridge equals contract", contract: acct, noreplay: norep, coreBridge: acct, wantErr: true},
		{name: "core bridge equals noreplay", contract: acct, noreplay: norep, coreBridge: norep, wantErr: true},
	}

	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			got, err := parseAccountantSolanaProgramIDs(tt.contract, tt.noreplay, tt.coreBridge)
			if tt.wantErr {
				require.Error(t, err)
				assert.Equal(t, accountantSolanaProgramIDs{}, got)
				return
			}
			require.NoError(t, err)
			assert.Equal(t, tt.contract, got.program.String())
			assert.Equal(t, tt.noreplay, got.noreplay.String())
			assert.Equal(t, tt.coreBridge, got.coreBridge.String())
		})
	}
}

func TestCheckAccountantSolanaConnFlags(t *testing.T) {
	rpcURL := "http://solana-devnet:8899"
	wsURL := "ws://solana-devnet:8900"

	tests := []struct {
		name        string
		rpcURL      string
		wsURL       string
		priorityFee uint64
		wantErr     bool
	}{
		{name: "valid at the fee cap", rpcURL: rpcURL, wsURL: wsURL, priorityFee: maxAccountantSolanaPriorityFee},
		{name: "fee one above the cap", rpcURL: rpcURL, wsURL: wsURL, priorityFee: maxAccountantSolanaPriorityFee + 1, wantErr: true},
		{name: "rpc is none", rpcURL: "none", wsURL: wsURL, wantErr: true},
		{name: "ws is none", rpcURL: rpcURL, wsURL: "none", wantErr: true},
	}

	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			err := checkAccountantSolanaConnFlags(tt.rpcURL, tt.wsURL, tt.priorityFee)
			if tt.wantErr {
				require.Error(t, err)
				return
			}
			require.NoError(t, err)
		})
	}
}
