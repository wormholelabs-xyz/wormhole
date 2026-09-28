package guardiand

import (
	"errors"
	"fmt"

	"github.com/gagliardetto/solana-go"
)

// REVIEW: opinions wanted on this cap. It guards the fee payer against an operator typo;
// mainnet priority fees usually stay below 1,000,000. Is 10,000,000 the right bound?
//
// maxAccountantSolanaPriorityFee caps --accountantSolanaPriorityFee in micro-lamports per compute unit.
// At the 150k compute unit submit limit this is at most 0.0015 SOL per transaction.
const maxAccountantSolanaPriorityFee = 10_000_000

type accountantSolanaProgramIDs struct {
	program    solana.PublicKey
	noreplay   solana.PublicKey
	coreBridge solana.PublicKey
}

// parseAccountantSolanaProgramIDs decodes the WTT accountant, NoReplay and Core Bridge program ids.
//
// SECURITY: the three ids are non-zero and pairwise distinct; one id in two slots
// would make every derived PDA wrong.
func parseAccountantSolanaProgramIDs(contract string, noreplayContract string, coreBridgeContract string) (accountantSolanaProgramIDs, error) {
	program, err := parseSolanaProgramID("accountantSolanaContract", contract)
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

	if program.Equals(noreplay) {
		return accountantSolanaProgramIDs{}, errors.New("accountantSolanaContract and accountantSolanaNoreplayContract are the same address")
	}
	if coreBridge.Equals(program) {
		return accountantSolanaProgramIDs{}, errors.New("solanaContract and accountantSolanaContract are the same address")
	}
	if coreBridge.Equals(noreplay) {
		return accountantSolanaProgramIDs{}, errors.New("solanaContract and accountantSolanaNoreplayContract are the same address")
	}

	return accountantSolanaProgramIDs{program: program, noreplay: noreplay, coreBridge: coreBridge}, nil
}

// parseSolanaProgramID decodes one non-zero program id; flag names it in errors.
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

// checkAccountantSolanaConnFlags validates the endpoint and fee flags of an enabled Solana WTT accountant.
// RegisterFlagWithValidationOrFail skips scheme validation for "none", so "none" is rejected here.
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
