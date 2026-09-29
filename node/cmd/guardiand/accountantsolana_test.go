package guardiand

import (
	"testing"

	"github.com/gagliardetto/solana-go"
	"github.com/stretchr/testify/assert"
	"github.com/stretchr/testify/require"
)

const (
	testSolanaAccountantProgram    = "US517G5965aydkZ46HS38QLi7UQiSojurfbQfKCELFx"
	testSolanaNttAccountantProgram = "cGfHiC6Kgg3FpFZvgwGcswsCRtp4aBP2fzuXRQPizuN"
	testSolanaNoreplayProgram      = "repMHgR5BEpGLeZvM5iGoNNDPw4eu2BS6sXJzaC8K4t"
	testSolanaCoreBridgeProgram    = "worm2ZoG2kUd4vFXhvjh93UUH596ayRfgQ2MgjNMTth"
)

func TestParseAccountantSolanaProgramIDs(t *testing.T) {
	zeroAddress := "11111111111111111111111111111111"
	acct := testSolanaAccountantProgram
	ntt := testSolanaNttAccountantProgram
	norep := testSolanaNoreplayProgram
	core := testSolanaCoreBridgeProgram

	tests := []struct {
		name        string
		contract    string
		nttContract string
		noreplay    string
		coreBridge  string
		wantErr     bool
	}{
		{name: "wtt only", contract: acct, noreplay: norep, coreBridge: core},
		{name: "ntt only", nttContract: ntt, noreplay: norep, coreBridge: core},
		{name: "wtt and ntt", contract: acct, nttContract: ntt, noreplay: norep, coreBridge: core},
		{name: "contract is not base58", contract: "not-an-address", noreplay: norep, coreBridge: core, wantErr: true},
		{name: "ntt contract is not base58", nttContract: "not-an-address", noreplay: norep, coreBridge: core, wantErr: true},
		{name: "contract is the zero address", contract: zeroAddress, noreplay: norep, coreBridge: core, wantErr: true},
		{name: "ntt contract is the zero address", nttContract: zeroAddress, noreplay: norep, coreBridge: core, wantErr: true},
		{name: "contract equals noreplay", contract: acct, noreplay: acct, coreBridge: core, wantErr: true},
		{name: "core bridge equals contract", contract: acct, noreplay: norep, coreBridge: acct, wantErr: true},
		{name: "core bridge equals noreplay", contract: acct, noreplay: norep, coreBridge: norep, wantErr: true},
		{name: "ntt contract equals contract", contract: acct, nttContract: acct, noreplay: norep, coreBridge: core, wantErr: true},
		{name: "ntt contract equals noreplay", nttContract: norep, noreplay: norep, coreBridge: core, wantErr: true},
		{name: "empty contracts", noreplay: norep, coreBridge: core, wantErr: true},
	}

	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			got, err := parseAccountantSolanaProgramIDs(tt.contract, tt.nttContract, tt.noreplay, tt.coreBridge)
			if tt.wantErr {
				require.Error(t, err)
				assert.Equal(t, accountantSolanaProgramIDs{}, got)
				return
			}
			require.NoError(t, err)
			wantProgram, wantNtt := solana.PublicKey{}, solana.PublicKey{}
			if tt.contract != "" {
				wantProgram = solana.MustPublicKeyFromBase58(tt.contract)
			}
			if tt.nttContract != "" {
				wantNtt = solana.MustPublicKeyFromBase58(tt.nttContract)
			}
			assert.Equal(t, wantProgram, got.program)
			assert.Equal(t, wantNtt, got.nttProgram)
			assert.Equal(t, tt.noreplay, got.noreplay.String())
			assert.Equal(t, tt.coreBridge, got.coreBridge.String())
		})
	}
}

func TestCheckAccountantSolanaFlagPresence(t *testing.T) {
	type flags struct {
		contract, nttContract, noreplay, rpc, ws, keyPath string
		priorityFee                                       uint64
	}
	conn := flags{noreplay: testSolanaNoreplayProgram, rpc: "http://solana-devnet:8899", ws: "ws://solana-devnet:8900", keyPath: "/keys/solana.json"}
	with := func(edit func(f *flags)) flags {
		f := conn
		edit(&f)
		return f
	}

	tests := []struct {
		name    string
		flags   flags
		wantErr bool
	}{
		{name: "all unset", flags: flags{}},
		{name: "wtt with connection", flags: with(func(f *flags) { f.contract = testSolanaAccountantProgram })},
		{name: "ntt with connection", flags: with(func(f *flags) { f.nttContract = testSolanaNttAccountantProgram })},
		{name: "both with connection and fee", flags: with(func(f *flags) {
			f.contract = testSolanaAccountantProgram
			f.nttContract = testSolanaNttAccountantProgram
			f.priorityFee = 5
		})},
		{name: "connection without a contract", flags: conn, wantErr: true},
		{name: "rpc without a contract", flags: flags{rpc: conn.rpc}, wantErr: true},
		{name: "fee without a contract", flags: flags{priorityFee: 5}, wantErr: true},
		{name: "ntt without noreplay", flags: with(func(f *flags) { f.nttContract = testSolanaNttAccountantProgram; f.noreplay = "" }), wantErr: true},
		{name: "ntt without rpc", flags: with(func(f *flags) { f.nttContract = testSolanaNttAccountantProgram; f.rpc = "" }), wantErr: true},
		{name: "ntt without ws", flags: with(func(f *flags) { f.nttContract = testSolanaNttAccountantProgram; f.ws = "" }), wantErr: true},
		{name: "ntt without key path", flags: with(func(f *flags) { f.nttContract = testSolanaNttAccountantProgram; f.keyPath = "" }), wantErr: true},
		{name: "wtt without key path", flags: with(func(f *flags) { f.contract = testSolanaAccountantProgram; f.keyPath = "" }), wantErr: true},
	}

	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			f := tt.flags
			err := checkAccountantSolanaFlagPresence(f.contract, f.nttContract, f.noreplay, f.rpc, f.ws, f.keyPath, f.priorityFee)
			if tt.wantErr {
				require.Error(t, err)
				return
			}
			require.NoError(t, err)
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
