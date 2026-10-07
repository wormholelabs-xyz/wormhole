package guardiand

import (
	"testing"

	"github.com/certusone/wormhole/node/pkg/common"
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

	tlsRPC := "https://rpc.example.com/mainnet/solana"
	tlsWS := "wss://rpc.example.com/mainnet/solana"

	tests := []struct {
		name        string
		env         common.Environment
		rpcURL      string
		wsURL       string
		priorityFee uint64
		wantErr     bool
	}{
		{name: "valid at the fee cap", env: common.UnsafeDevNet, rpcURL: rpcURL, wsURL: wsURL, priorityFee: maxAccountantSolanaPriorityFee},
		{name: "fee one above the cap", env: common.UnsafeDevNet, rpcURL: rpcURL, wsURL: wsURL, priorityFee: maxAccountantSolanaPriorityFee + 1, wantErr: true},
		{name: "rpc is none", env: common.UnsafeDevNet, rpcURL: "none", wsURL: wsURL, wantErr: true},
		{name: "ws is none", env: common.UnsafeDevNet, rpcURL: rpcURL, wsURL: "none", wantErr: true},
		{name: "mainnet tls", env: common.MainNet, rpcURL: tlsRPC, wsURL: tlsWS},
		{name: "testnet tls", env: common.TestNet, rpcURL: tlsRPC, wsURL: tlsWS},
		{name: "mainnet plaintext rpc", env: common.MainNet, rpcURL: rpcURL, wsURL: tlsWS, wantErr: true},
		{name: "mainnet plaintext ws", env: common.MainNet, rpcURL: tlsRPC, wsURL: wsURL, wantErr: true},
		{name: "testnet plaintext rpc", env: common.TestNet, rpcURL: rpcURL, wsURL: tlsWS, wantErr: true},
		{name: "testnet plaintext ws", env: common.TestNet, rpcURL: tlsRPC, wsURL: wsURL, wantErr: true},
		{name: "mainnet rpc with a ws scheme", env: common.MainNet, rpcURL: tlsWS, wsURL: tlsWS, wantErr: true},
	}

	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			err := checkAccountantSolanaConnFlags(tt.env, tt.rpcURL, tt.wsURL, tt.priorityFee)
			if tt.wantErr {
				require.Error(t, err)
				return
			}
			require.NoError(t, err)
		})
	}
}

func TestCheckAccountantSolanaDeployment(t *testing.T) {
	acct := solana.MustPublicKeyFromBase58(testSolanaAccountantProgram)
	ntt := solana.MustPublicKeyFromBase58(testSolanaNttAccountantProgram)
	norep := solana.MustPublicKeyFromBase58(testSolanaNoreplayProgram)
	core := solana.MustPublicKeyFromBase58(testSolanaCoreBridgeProgram)
	other := solana.PublicKey{9}
	genesis := solana.Hash{1}

	deployments := map[common.Environment]accountantSolanaDeployment{
		common.TestNet: {ids: accountantSolanaProgramIDs{program: acct, noreplay: norep, coreBridge: core}, genesisHash: genesis},
	}
	wtt := accountantSolanaProgramIDs{program: acct, noreplay: norep, coreBridge: core}
	with := func(edit func(ids *accountantSolanaProgramIDs)) accountantSolanaProgramIDs {
		ids := wtt
		edit(&ids)
		return ids
	}

	tests := []struct {
		name    string
		env     common.Environment
		ids     accountantSolanaProgramIDs
		genesis solana.Hash
		wantErr bool
	}{
		{name: "devnet takes the flags", env: common.UnsafeDevNet, ids: with(func(ids *accountantSolanaProgramIDs) { ids.program = other })},
		{name: "testnet deployment", env: common.TestNet, ids: wtt, genesis: genesis},
		{name: "mainnet has no deployment", env: common.MainNet, ids: wtt, genesis: genesis, wantErr: true},
		{name: "testnet program differs", env: common.TestNet, ids: with(func(ids *accountantSolanaProgramIDs) { ids.program = other }), genesis: genesis, wantErr: true},
		{name: "testnet ntt is not deployed", env: common.TestNet, ids: with(func(ids *accountantSolanaProgramIDs) { ids.nttProgram = ntt }), genesis: genesis, wantErr: true},
		{name: "testnet noreplay differs", env: common.TestNet, ids: with(func(ids *accountantSolanaProgramIDs) { ids.noreplay = other }), genesis: genesis, wantErr: true},
		{name: "testnet core bridge differs", env: common.TestNet, ids: with(func(ids *accountantSolanaProgramIDs) { ids.coreBridge = other }), genesis: genesis, wantErr: true},
		{name: "testnet genesis differs", env: common.TestNet, ids: wtt, genesis: solana.Hash{2}, wantErr: true},
	}

	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			err := checkAccountantSolanaDeployment(tt.env, tt.ids, tt.genesis, deployments)
			if tt.wantErr {
				require.Error(t, err)
				return
			}
			require.NoError(t, err)
		})
	}
}
