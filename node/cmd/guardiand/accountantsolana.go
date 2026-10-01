package guardiand

import (
	"errors"
	"fmt"

	"github.com/gagliardetto/solana-go"
)

// REVIEW: what should this cap be?
//
// maxAccountantSolanaPriorityFee caps --accountantSolanaPriorityFee in micro-lamports per compute unit.
// At the 150k compute unit submit limit, the maximum cost is 0.0015 SOL per transaction.
const maxAccountantSolanaPriorityFee = 10_000_000

type accountantSolanaProgramIDs struct {
	program    solana.PublicKey
	noreplay   solana.PublicKey
	coreBridge solana.PublicKey
}

// parseAccountantSolanaProgramIDs decodes the WTT accountant, NoReplay and Core Bridge program ids.
//
// SECURITY: each id must not be the zero address. The three ids must be different from each other.
// If one id fills two slots, every derived PDA is wrong.
func parseAccountantSolanaProgramIDs(globalAccountantContract string, noreplayContract string, coreBridgeContract string) (accountantSolanaProgramIDs, error) {
	globalAccountantProgram, err := parseSolanaProgramID("accountantSolanaContract", globalAccountantContract)
	if err != nil {
		return accountantSolanaProgramIDs{}, err
	}
	noreplay, err := parseSolanaProgramID("accountantSolanaNoreplayContract", noreplayContract)
	if err != nil {
		return accountantSolanaProgramIDs{}, err
	}
	coreBridge, err := parseSolanaProgramID("solanaContract", coreBridgeContract)
	if err != nil {
		return accountantSolanaProgramIDs{}, err
	}

	if globalAccountantProgram.Equals(noreplay) {
		return accountantSolanaProgramIDs{}, errors.New("accountantSolanaContract and accountantSolanaNoreplayContract are the same address")
	}
	if coreBridge.Equals(globalAccountantProgram) {
		return accountantSolanaProgramIDs{}, errors.New("solanaContract and accountantSolanaContract are the same address")
	}
	if coreBridge.Equals(noreplay) {
		return accountantSolanaProgramIDs{}, errors.New("solanaContract and accountantSolanaNoreplayContract are the same address")
	}

	return accountantSolanaProgramIDs{program: globalAccountantProgram, noreplay: noreplay, coreBridge: coreBridge}, nil
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

// checkAccountantSolanaConnFlags checks the endpoint and fee flags of an enabled Solana WTT accountant.
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
