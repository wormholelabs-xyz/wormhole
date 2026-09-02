// Layout decoders and PDA derivations for the svm/accountant program.
// Constants mirror svm/accountant/crates/definitions. Fixtures in
// solana_layout_test.go come from programs/global-accountant/tests/go_fixture_vectors.rs.

package accountant

import (
	"encoding/binary"
	"fmt"

	"github.com/gagliardetto/solana-go"
	"github.com/wormhole-foundation/wormhole/sdk/vaa"
)

const (
	// ACCOUNTANT_DIGEST_LOG_LEN, constants/log.rs.
	accountantDigestLogLen = 8 + 2 + 32 + 8 + 32 + 4

	// PendingObservationsLayout::LEN and ::TAG, state.rs.
	pendingObservationsLen = 76
	pendingObservationsTag = 1

	// solana-noreplay bucket: bump(1) + bitmap(128), operational-core noreplay.rs.
	noreplayBucketLen     = 129
	noreplayBitmapOffset  = 1
	noreplayBitsPerBucket = 1024

	// SUBMIT_FIXED_LEN, operational-core quorum.rs: guardian_set_index(4) + guardian_index(1) + signature(65).
	submitObservationsDispatchLen = 1
	submitFixedLen                = 4 + 1 + 65
	submitObservationsTxHashLen   = 32
)

// ACCOUNTANT_DIGEST_LOG_TAG, constants/log.rs.
var accountantDigestLogTag = [8]byte{'A', 'C', 'C', 'D', 'G', 'S', 'T', 0}

// constants/seeds.rs.
var (
	pendingObservationsSeedPrefix = []byte("pending")
	noreplayAuthoritySeedPrefix   = []byte("noreplay_authority")
)

// solanaCommitEvent is a decoded ACCDGST commit log.
type solanaCommitEvent struct {
	Chain            vaa.ChainID
	Emitter          vaa.Address
	Sequence         uint64
	Digest           [32]byte
	GuardianSetIndex uint32
}

// parseAccountantDigestLog decodes an ACCDGST commit log.
func parseAccountantDigestLog(data []byte) (*solanaCommitEvent, error) {
	if len(data) != accountantDigestLogLen {
		return nil, fmt.Errorf("accountant digest log: want %d bytes, got %d", accountantDigestLogLen, len(data))
	}
	var tag [8]byte
	copy(tag[:], data[:8])
	if tag != accountantDigestLogTag {
		return nil, fmt.Errorf("accountant digest log: tag mismatch, got %x", tag)
	}

	evt := &solanaCommitEvent{
		Chain:            vaa.ChainID(binary.BigEndian.Uint16(data[8:10])),
		Sequence:         binary.BigEndian.Uint64(data[42:50]),
		GuardianSetIndex: binary.LittleEndian.Uint32(data[82:86]),
	}
	copy(evt.Emitter[:], data[10:42])
	copy(evt.Digest[:], data[50:82])
	return evt, nil
}

// solanaPendingObs is a decoded PendingObservationsLayout account.
// Emitter and sequence exist only in the PDA seeds.
type solanaPendingObs struct {
	Chain            vaa.ChainID
	GuardianSetIndex uint32
	Signatures       uint32
	Digest           [32]byte
	Payer            solana.PublicKey
}

// parsePendingObservationsAccount decodes a PendingObservationsLayout account.
func parsePendingObservationsAccount(data []byte) (*solanaPendingObs, error) {
	if len(data) != pendingObservationsLen {
		return nil, fmt.Errorf("pending observations account: want %d bytes, got %d", pendingObservationsLen, len(data))
	}
	if data[0] != pendingObservationsTag {
		return nil, fmt.Errorf("pending observations account: tag mismatch, want %d got %d", pendingObservationsTag, data[0])
	}

	obs := &solanaPendingObs{
		Chain:            vaa.ChainID(binary.LittleEndian.Uint16(data[2:4])),
		GuardianSetIndex: binary.LittleEndian.Uint32(data[4:8]),
		Signatures:       binary.LittleEndian.Uint32(data[8:12]),
	}
	copy(obs.Digest[:], data[12:44])
	copy(obs.Payer[:], data[44:76])
	return obs, nil
}

// derivePendingObservationsPDA mirrors quorum.rs create_pending_pda.
func derivePendingObservationsPDA(program solana.PublicKey, chain vaa.ChainID, emitter vaa.Address, sequence uint64, digest [32]byte) (solana.PublicKey, error) {
	var chainBE [2]byte
	binary.BigEndian.PutUint16(chainBE[:], uint16(chain))
	var sequenceBE [8]byte
	binary.BigEndian.PutUint64(sequenceBE[:], sequence)

	pda, _, err := solana.FindProgramAddress([][]byte{
		pendingObservationsSeedPrefix,
		chainBE[:],
		emitter[:],
		sequenceBE[:],
		digest[:],
	}, program)
	if err != nil {
		return solana.PublicKey{}, fmt.Errorf("derive pending observations PDA: %w", err)
	}
	return pda, nil
}

// deriveNoreplayAuthorityPDA mirrors noreplay.rs mark_used.
func deriveNoreplayAuthorityPDA(accountantProgram solana.PublicKey) (solana.PublicKey, error) {
	pda, _, err := solana.FindProgramAddress([][]byte{
		noreplayAuthoritySeedPrefix,
	}, accountantProgram)
	if err != nil {
		return solana.PublicKey{}, fmt.Errorf("derive noreplay authority PDA: %w", err)
	}
	return pda, nil
}

// deriveNoreplayBucketPDA mirrors noreplay.rs derive_bucket_pda.
// namespace = chain_be(2) ++ emitter(32), split at byte 32 for the seed length limit.
func deriveNoreplayBucketPDA(noreplayProgram, authority solana.PublicKey, chain vaa.ChainID, emitter vaa.Address, sequence uint64) (solana.PublicKey, error) {
	var namespace [34]byte
	binary.BigEndian.PutUint16(namespace[:2], uint16(chain))
	copy(namespace[2:], emitter[:])

	var bucketIndexLE [8]byte
	binary.LittleEndian.PutUint64(bucketIndexLE[:], sequence/noreplayBitsPerBucket)

	pda, _, err := solana.FindProgramAddress([][]byte{
		authority[:],
		namespace[:32],
		namespace[32:],
		bucketIndexLE[:],
	}, noreplayProgram)
	if err != nil {
		return solana.PublicKey{}, fmt.Errorf("derive noreplay bucket PDA: %w", err)
	}
	return pda, nil
}

// noreplayBitSet mirrors noreplay.rs is_marked.
func noreplayBitSet(bucketData []byte, sequence uint64) (bool, error) {
	if len(bucketData) != noreplayBucketLen {
		return false, fmt.Errorf("noreplay bucket account: want %d bytes, got %d", noreplayBucketLen, len(bucketData))
	}
	bit := sequence % noreplayBitsPerBucket
	byteOffset := noreplayBitmapOffset + int(bit/8)
	mask := byte(1) << (bit % 8)
	return bucketData[byteOffset]&mask != 0, nil
}

// parseSubmitObservationsTxHash reads tx_hash from raw submit_observations instruction data.
// Layout: discriminator(1) ‖ SUBMIT_FIXED_LEN ‖ tx_hash(32) ‖ body_len(2 LE) ‖ body.
func parseSubmitObservationsTxHash(instructionData []byte) ([]byte, error) {
	minLen := submitObservationsDispatchLen + submitFixedLen + submitObservationsTxHashLen
	if len(instructionData) < minLen {
		return nil, fmt.Errorf("submit_observations instruction data: want at least %d bytes, got %d", minLen, len(instructionData))
	}
	start := submitObservationsDispatchLen + submitFixedLen
	txHash := make([]byte, submitObservationsTxHashLen)
	copy(txHash, instructionData[start:start+submitObservationsTxHashLen])
	return txHash, nil
}
