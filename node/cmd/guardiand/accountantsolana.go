package guardiand

import (
	"context"
	"errors"
	"fmt"
	"net/url"
	"time"

	"github.com/certusone/wormhole/node/pkg/common"
	"github.com/gagliardetto/solana-go"
)

// maxAccountantSolanaPriorityFee caps --accountantSolanaPriorityFee, the total priority fee in lamports
// of one transaction v1: 0.0001 SOL per transaction, for either program.
const maxAccountantSolanaPriorityFee = 100_000

type accountantSolanaProgramIDs struct {
	program    solana.PublicKey // zero when --accountantSolanaContract is unset
	nttProgram solana.PublicKey // zero when --accountantSolanaNttContract is unset
	noreplay   solana.PublicKey
	coreBridge solana.PublicKey
}

// checkAccountantSolanaFlagPresence checks which Solana accountant flags are set. The
// connection flags are required if a WTT or NTT accountant contract is set. Both programs
// share them.
func checkAccountantSolanaFlagPresence(contract string, nttContract string, noreplayContract string, rpcURL string, wsURL string, keyPath string, priorityFee uint64) error {
	enabled := contract != "" || nttContract != ""
	for _, flag := range [...]struct {
		name  string
		value string
	}{
		{"accountantSolanaNoreplayContract", noreplayContract},
		{"accountantSolanaRPC", rpcURL},
		{"accountantSolanaWS", wsURL},
		{"accountantSolanaKeyPath", keyPath},
	} {
		if enabled && flag.value == "" {
			return fmt.Errorf("--%s is required when --accountantSolanaContract or --accountantSolanaNttContract is set", flag.name)
		}
		if !enabled && flag.value != "" {
			return fmt.Errorf("--%s requires --accountantSolanaContract or --accountantSolanaNttContract", flag.name)
		}
	}
	if !enabled && priorityFee != 0 {
		return errors.New("--accountantSolanaPriorityFee requires --accountantSolanaContract or --accountantSolanaNttContract")
	}
	return nil
}

// parseAccountantSolanaProgramIDs decodes the WTT accountant, NTT accountant, NoReplay and
// Core Bridge program ids. Either accountant may be empty, not both.
//
// SECURITY: each set id must not be the zero address. The set ids must be different from each
// other. If one id fills two slots, every derived PDA is wrong.
func parseAccountantSolanaProgramIDs(contract string, nttContract string, noreplayContract string, coreBridgeContract string) (accountantSolanaProgramIDs, error) {
	if contract == "" && nttContract == "" {
		return accountantSolanaProgramIDs{}, errors.New("accountantSolanaContract or accountantSolanaNttContract is required")
	}

	var ids accountantSolanaProgramIDs
	var err error
	if contract != "" {
		if ids.program, err = parseSolanaProgramID("accountantSolanaContract", contract); err != nil {
			return accountantSolanaProgramIDs{}, err
		}
	}
	if nttContract != "" {
		if ids.nttProgram, err = parseSolanaProgramID("accountantSolanaNttContract", nttContract); err != nil {
			return accountantSolanaProgramIDs{}, err
		}
	}
	if ids.noreplay, err = parseSolanaProgramID("accountantSolanaNoreplayContract", noreplayContract); err != nil {
		return accountantSolanaProgramIDs{}, err
	}
	if ids.coreBridge, err = parseSolanaProgramID("solanaContract", coreBridgeContract); err != nil {
		return accountantSolanaProgramIDs{}, err
	}

	named := [...]struct {
		flag string
		id   solana.PublicKey
	}{
		{"accountantSolanaContract", ids.program},
		{"accountantSolanaNttContract", ids.nttProgram},
		{"accountantSolanaNoreplayContract", ids.noreplay},
		{"solanaContract", ids.coreBridge},
	}
	for i := range named {
		for j := i + 1; j < len(named); j++ {
			if !named[i].id.IsZero() && named[i].id.Equals(named[j].id) {
				return accountantSolanaProgramIDs{}, fmt.Errorf("%s and %s are the same address", named[i].flag, named[j].flag)
			}
		}
	}
	return ids, nil
}

// parseSolanaProgramID decodes one program id that must not be the zero address. Errors include flag.
func parseSolanaProgramID(flag string, value string) (solana.PublicKey, error) {
	key, err := solana.PublicKeyFromBase58(value)
	if err != nil {
		return solana.PublicKey{}, fmt.Errorf("%s %q is not a Solana address: %w", flag, value, err)
	}
	if key.IsZero() {
		return solana.PublicKey{}, fmt.Errorf("%s is the zero address", flag)
	}
	return key, nil
}

// checkAccountantSolanaConnFlags checks the endpoint and fee flags of an enabled Solana accountant.
// RegisterFlagWithValidationOrFail skips the scheme check for "none", so this function rejects "none".
//
// SECURITY: mainnet and testnet require https and wss. The endpoints decide which transfers release.
func checkAccountantSolanaConnFlags(env common.Environment, rpcURL string, wsURL string, priorityFee uint64) error {
	if rpcURL == "" || rpcURL == "none" {
		return fmt.Errorf("accountantSolanaRPC %q is not an RPC URL", rpcURL)
	}
	if wsURL == "" || wsURL == "none" {
		return fmt.Errorf("accountantSolanaWS %q is not a websocket URL", wsURL)
	}
	if accountantSolanaPinnedEnv(env) {
		for _, endpoint := range [...]struct{ flag, value, scheme string }{
			{"accountantSolanaRPC", rpcURL, "https"},
			{"accountantSolanaWS", wsURL, "wss"},
		} {
			u, err := url.Parse(endpoint.value)
			if err != nil {
				return fmt.Errorf("%s is not a URL: %w", endpoint.flag, err)
			}
			if u.Scheme != endpoint.scheme {
				return fmt.Errorf("%s must use %s in %s", endpoint.flag, endpoint.scheme, env)
			}
		}
	}
	if priorityFee > maxAccountantSolanaPriorityFee {
		return fmt.Errorf("accountantSolanaPriorityFee %d exceeds the maximum of %d lamports per transaction", priorityFee, maxAccountantSolanaPriorityFee)
	}
	return nil
}

// accountantSolanaDeployment is the expected cluster and program ids of one environment.
type accountantSolanaDeployment struct {
	ids         accountantSolanaProgramIDs
	genesisHash solana.Hash
}

// accountantSolanaDeployments holds the deployment of each pinned environment. A release adds
// the mainnet and testnet entries with the program deployments.
var accountantSolanaDeployments = map[common.Environment]accountantSolanaDeployment{}

// accountantSolanaPinnedEnv reports if env takes its program ids and cluster from
// accountantSolanaDeployments. Other environments take them from the flags.
func accountantSolanaPinnedEnv(env common.Environment) bool {
	return env == common.MainNet || env == common.TestNet
}

const accountantSolanaGenesisTimeout = 30 * time.Second

// readAccountantSolanaGenesisHash reads the RPC genesis hash in a pinned environment. It
// returns the zero hash in other environments.
func readAccountantSolanaGenesisHash(ctx context.Context, env common.Environment, conn interface {
	GetGenesisHash(ctx context.Context) (solana.Hash, error)
}) (solana.Hash, error) {
	if !accountantSolanaPinnedEnv(env) {
		return solana.Hash{}, nil
	}
	ctx, cancel := context.WithTimeout(ctx, accountantSolanaGenesisTimeout)
	defer cancel()
	return conn.GetGenesisHash(ctx)
}

// checkAccountantSolanaDeployment checks the flag program ids and the RPC genesis hash against
// the deployment of env.
//
// SECURITY: any Solana user can deploy a program. In a pinned environment, an id or a cluster
// from a wrong configuration must stop the node.
func checkAccountantSolanaDeployment(env common.Environment, ids accountantSolanaProgramIDs, genesisHash solana.Hash, deployments map[common.Environment]accountantSolanaDeployment) error {
	if !accountantSolanaPinnedEnv(env) {
		return nil
	}
	want, exists := deployments[env]
	if !exists {
		return fmt.Errorf("the solana accountant has no deployment in %s", env)
	}
	if genesisHash != want.genesisHash {
		return fmt.Errorf("the solana accountant rpc genesis hash is %s, want %s in %s", genesisHash, want.genesisHash, env)
	}
	for _, check := range [...]struct {
		flag      string
		got, want solana.PublicKey
		optional  bool
	}{
		{"accountantSolanaContract", ids.program, want.ids.program, true},
		{"accountantSolanaNttContract", ids.nttProgram, want.ids.nttProgram, true},
		{"accountantSolanaNoreplayContract", ids.noreplay, want.ids.noreplay, false},
		{"solanaContract", ids.coreBridge, want.ids.coreBridge, false},
	} {
		// An unset accountant flag disables that program.
		if check.optional && check.got.IsZero() {
			continue
		}
		if check.want.IsZero() {
			return fmt.Errorf("%s has no deployment in %s", check.flag, env)
		}
		if !check.got.Equals(check.want) {
			return fmt.Errorf("%s is %s, want %s in %s", check.flag, check.got, check.want, env)
		}
	}
	return nil
}
