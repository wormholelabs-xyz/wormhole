package guardiand

import (
	"errors"
	"fmt"

	"github.com/gagliardetto/solana-go"
)

// REVIEW: what should this cap be?
//
// maxAccountantSolanaPriorityFee caps --accountantSolanaPriorityFee in micro-lamports per compute unit.
// At the 150k compute unit submit limit, the maximum cost is 0.0015 SOL per transaction, for either program.
const maxAccountantSolanaPriorityFee = 10_000_000

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
func checkAccountantSolanaConnFlags(rpcURL string, wsURL string, priorityFee uint64) error {
	if rpcURL == "" || rpcURL == "none" {
		return fmt.Errorf("accountantSolanaRPC %q is not an RPC URL", rpcURL)
	}
	if wsURL == "" || wsURL == "none" {
		return fmt.Errorf("accountantSolanaWS %q is not a websocket URL", wsURL)
	}
	if priorityFee > maxAccountantSolanaPriorityFee {
		return fmt.Errorf("accountantSolanaPriorityFee %d exceeds the maximum of %d micro-lamports per compute unit", priorityFee, maxAccountantSolanaPriorityFee)
	}
	return nil
}
