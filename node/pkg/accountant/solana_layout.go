// Layout decoders and PDA derivations for the svm/accountant program. The wire structs,
// layout constants and test fixtures are generated: run `just go-codegen` in svm/accountant.

package accountant

import (
	"bytes"
	"encoding/binary"
	"errors"
	"fmt"

	"github.com/ethereum/go-ethereum/crypto"
	"github.com/gagliardetto/solana-go"
	"github.com/wormhole-foundation/wormhole/sdk/vaa"
)

// Bits in one word of the pending-account signature bitmap.
const pendingObservationsBitsPerWord = pendingObservationsMaxGuardians / pendingObservationsSignatureWords

// Compile-time equalities: the bitmap words hold exactly MaxGuardians bits.
const (
	_ = uint(pendingObservationsBitsPerWord*pendingObservationsSignatureWords - pendingObservationsMaxGuardians)
	_ = uint(pendingObservationsMaxGuardians - pendingObservationsBitsPerWord*pendingObservationsSignatureWords)
)

// solanaCommitEvent is a decoded ACCDGST commit log. Digest is the content digest from
// submit_observations, or the VAA digest from submit_vaas and the backfill.
type solanaCommitEvent struct {
	Chain            vaa.ChainID
	Emitter          vaa.Address
	Sequence         uint64
	Digest           [32]byte
	GuardianSetIndex uint32
}

// parseAccountantDigestLog decodes an ACCDGST commit log.
func parseAccountantDigestLog(data []byte) (*solanaCommitEvent, error) {
	var wire accountantDigestLogWire
	if err := decodeWire(data, &wire); err != nil {
		return nil, err
	}
	if wire.Tag != accountantDigestLogTag {
		return nil, fmt.Errorf("accountant digest log: tag mismatch, got %x", wire.Tag)
	}
	return &solanaCommitEvent{
		Chain:            vaa.ChainID(wire.Chain.Uint16()),
		Emitter:          wire.Emitter,
		Sequence:         wire.Sequence.Uint64(),
		Digest:           wire.Digest,
		GuardianSetIndex: wire.GuardianSetIndex,
	}, nil
}

// solanaPendingObs is a decoded PendingObservationsLayout account.
// Emitter and sequence exist only in the PDA seeds.
type solanaPendingObs struct {
	Chain            vaa.ChainID
	GuardianSetIndex uint32
	// 128-bit signature bitmap as little-endian words.
	Signatures    [pendingObservationsSignatureWords]uint32
	ContentDigest [32]byte
	Payer         solana.PublicKey
}

// parsePendingObservationsAccount decodes a PendingObservationsLayout account.
func parsePendingObservationsAccount(data []byte) (*solanaPendingObs, error) {
	var wire pendingObservationsWire
	if err := decodeWire(data, &wire); err != nil {
		return nil, err
	}
	if wire.Tag != pendingObservationsTag {
		return nil, fmt.Errorf("pending observations account: tag mismatch, want %d got %d", pendingObservationsTag, wire.Tag)
	}
	return &solanaPendingObs{
		Chain:            vaa.ChainID(wire.Chain),
		GuardianSetIndex: wire.GuardianSetIndex,
		Signatures:       wire.Signatures,
		ContentDigest:    wire.ContentDigest,
		Payer:            wire.Payer,
	}, nil
}

// hasSignature mirrors PendingObservationsLayout::has_signature. Bit N lives in word
// N / pendingObservationsBitsPerWord.
func (o *solanaPendingObs) hasSignature(index uint8) (bool, error) {
	if uint32(index) >= pendingObservationsMaxGuardians {
		return false, fmt.Errorf("pending observations account: guardian index %d is at or past the %d-bit bitmap", index, pendingObservationsMaxGuardians)
	}
	word, bit := index/pendingObservationsBitsPerWord, index%pendingObservationsBitsPerWord
	return o.Signatures[word]&(1<<bit) != 0, nil
}

// checkPendingObservationsAccount decodes a pending account, requires its content digest
// to equal wantDigest, and reports whether guardianIndex signed it.
//
// SECURITY: the content digest is a PDA seed, so a mismatch means the address is not the
// one derived for the transfer.
func checkPendingObservationsAccount(data []byte, wantDigest [32]byte, guardianIndex uint8) (*solanaPendingObs, bool, error) {
	obs, err := parsePendingObservationsAccount(data)
	if err != nil {
		return nil, false, err
	}
	if obs.ContentDigest != wantDigest {
		return nil, false, errors.New("pending observations account: content digest mismatch")
	}
	signed, err := obs.hasSignature(guardianIndex)
	if err != nil {
		return nil, false, err
	}
	return obs, signed, nil
}

// derivePendingObservationsPDA mirrors quorum.rs derive_pending_pda.
func derivePendingObservationsPDA(program solana.PublicKey, chain vaa.ChainID, emitter vaa.Address, sequence uint64, guardianSetIndex uint32, contentDigest [32]byte) (solana.PublicKey, error) {
	pda, _, err := solana.FindProgramAddress([][]byte{
		pendingObservationsSeedPrefix,
		newBE16(uint16(chain)).bytes(),
		emitter[:],
		newBE64(sequence).bytes(),
		newBE32(guardianSetIndex).bytes(),
		contentDigest[:],
	}, program)
	if err != nil {
		return solana.PublicKey{}, fmt.Errorf("derive pending observations PDA: %w", err)
	}
	return pda, nil
}

// deriveNoreplayAuthorityPDA mirrors noreplay.rs derive_authority.
func deriveNoreplayAuthorityPDA(accountantProgram solana.PublicKey) (solana.PublicKey, error) {
	pda, _, err := solana.FindProgramAddress([][]byte{
		noreplayAuthoritySeedPrefix,
	}, accountantProgram)
	if err != nil {
		return solana.PublicKey{}, fmt.Errorf("derive noreplay authority PDA: %w", err)
	}
	return pda, nil
}

// deriveNoreplayBucketPDA mirrors noreplay.rs derive_bucket_pda. The namespace splits at
// NoReplayNamespace::seed_chunks, because one seed holds at most solana.MaxSeedLength bytes.
func deriveNoreplayBucketPDA(noreplayProgram, authority solana.PublicKey, chain vaa.ChainID, emitter vaa.Address, sequence uint64) (solana.PublicKey, error) {
	namespace, err := encodeWire(&noreplayNamespaceWire{Chain: newBE16(uint16(chain)), Emitter: emitter})
	if err != nil {
		return solana.PublicKey{}, fmt.Errorf("derive noreplay bucket PDA: %w", err)
	}
	seedA, seedB := namespace[:noreplayNamespaceSeedSplit], namespace[noreplayNamespaceSeedSplit:]
	if len(seedA) > solana.MaxSeedLength {
		return solana.PublicKey{}, fmt.Errorf("derive noreplay bucket PDA: first namespace seed is %d bytes, limit %d", len(seedA), solana.MaxSeedLength)
	}
	if len(seedB) > solana.MaxSeedLength {
		return solana.PublicKey{}, fmt.Errorf("derive noreplay bucket PDA: second namespace seed is %d bytes, limit %d", len(seedB), solana.MaxSeedLength)
	}

	pda, _, err := solana.FindProgramAddress([][]byte{
		authority[:],
		seedA,
		seedB,
		binary.LittleEndian.AppendUint64(nil, sequence/noreplayBitsPerBucket),
	}, noreplayProgram)
	if err != nil {
		return solana.PublicKey{}, fmt.Errorf("derive noreplay bucket PDA: %w", err)
	}
	return pda, nil
}

// deriveBalanceAccountPDA mirrors accounts/balance.rs derive_pda. chain is the side of the
// transfer that owns the balance. Use the emitter chain for the source. Use the recipient
// chain for the destination.
func deriveBalanceAccountPDA(program solana.PublicKey, chain vaa.ChainID, tokenChain vaa.ChainID, tokenAddress [32]byte) (solana.PublicKey, error) {
	pda, _, err := solana.FindProgramAddress([][]byte{
		balanceAccountSeedPrefix,
		newBE16(uint16(chain)).bytes(),
		newBE16(uint16(tokenChain)).bytes(),
		tokenAddress[:],
	}, program)
	if err != nil {
		return solana.PublicKey{}, fmt.Errorf("derive balance account PDA: %w", err)
	}
	return pda, nil
}

// deriveChainRegistrationPDA mirrors accounts/chain_registration.rs derive_pda.
func deriveChainRegistrationPDA(program solana.PublicKey, chain vaa.ChainID) (solana.PublicKey, error) {
	pda, _, err := solana.FindProgramAddress([][]byte{
		chainRegistrationSeedPrefix,
		newBE16(uint16(chain)).bytes(),
	}, program)
	if err != nil {
		return solana.PublicKey{}, fmt.Errorf("derive chain registration PDA: %w", err)
	}
	return pda, nil
}

// deriveGuardianSetPDA is the Core Bridge GuardianSet account for one set index.
func deriveGuardianSetPDA(coreBridge solana.PublicKey, guardianSetIndex uint32) (solana.PublicKey, error) {
	pda, _, err := solana.FindProgramAddress([][]byte{
		guardianSetSeedPrefix,
		newBE32(guardianSetIndex).bytes(),
	}, coreBridge)
	if err != nil {
		return solana.PublicKey{}, fmt.Errorf("derive guardian set PDA: %w", err)
	}
	return pda, nil
}

// noreplayBitSet mirrors noreplay.rs is_marked.
func noreplayBitSet(bucketData []byte, sequence uint64) (bool, error) {
	var wire noreplayBucketWire
	if err := decodeWire(bucketData, &wire); err != nil {
		return false, err
	}
	index, mask := noreplayBitLocation(sequence)
	return wire.Bitmap[index]&mask != 0, nil
}

// noreplayBitLocation is the bitmap byte and mask of sequence, low bit first, as noreplay.rs
// is_marked reads them.
func noreplayBitLocation(sequence uint64) (uint64, byte) {
	const bitsPerByte = uint64(noreplayBitsPerBucket / len(noreplayBucketWire{}.Bitmap))
	bit := sequence % noreplayBitsPerBucket
	return bit / bitsPerByte, byte(1) << (bit % bitsPerByte)
}

// solanaObservationFields is the 143-byte record the program hashes for both the
// content digest and the signing digest. ix_data.rs ObservationFieldsAndDigest.
type solanaObservationFields struct {
	Action         uint8
	Chain          vaa.ChainID
	Emitter        vaa.Address
	Sequence       uint64
	TokenChain     vaa.ChainID
	TokenAddress   [32]byte
	RecipientChain vaa.ChainID
	// Big-endian Uint256.
	Amount [32]byte
	// keccak256(keccak256(body)). Equals the inner digest field of the pending entry.
	VaaDigest [32]byte

	// keccak256(keccak256(pack())): the pending-PDA seed and the commit-log digest. The
	// constructors set it.
	contentDigest [32]byte
}

// wire is the record in the program's layout.
func (f *solanaObservationFields) wire() observationFieldsWire {
	return observationFieldsWire{
		Action:         f.Action,
		Chain:          newBE16(uint16(f.Chain)),
		Emitter:        f.Emitter,
		Sequence:       newBE64(f.Sequence),
		TokenChain:     newBE16(uint16(f.TokenChain)),
		TokenAddress:   f.TokenAddress,
		RecipientChain: newBE16(uint16(f.RecipientChain)),
		Amount:         f.Amount,
		VaaDigest:      f.VaaDigest,
	}
}

// pack serializes the record in the program's field order.
func (f *solanaObservationFields) pack() ([]byte, error) {
	wire := f.wire()
	return encodeWire(&wire)
}

func (f *solanaObservationFields) computeContentDigest() ([32]byte, error) {
	packed, err := f.pack()
	if err != nil {
		return [32]byte{}, err
	}
	return [32]byte(crypto.Keccak256(crypto.Keccak256(packed))), nil
}

func (f *solanaObservationFields) setContentDigest() error {
	digest, err := f.computeContentDigest()
	if err != nil {
		return err
	}
	f.contentDigest = digest
	return nil
}

// observationFieldsFromWire converts the program's layout and sets the content digest.
func observationFieldsFromWire(wire *observationFieldsWire) (solanaObservationFields, error) {
	f := solanaObservationFields{
		Action:         wire.Action,
		Chain:          vaa.ChainID(wire.Chain.Uint16()),
		Emitter:        wire.Emitter,
		Sequence:       wire.Sequence.Uint64(),
		TokenChain:     vaa.ChainID(wire.TokenChain.Uint16()),
		TokenAddress:   wire.TokenAddress,
		RecipientChain: vaa.ChainID(wire.RecipientChain.Uint16()),
		Amount:         wire.Amount,
		VaaDigest:      wire.VaaDigest,
	}
	return f, f.setContentDigest()
}

// unpackObservationFields reads the record from exactly observationFieldsLen bytes.
func unpackObservationFields(data []byte) (solanaObservationFields, error) {
	var wire observationFieldsWire
	if err := decodeWire(data, &wire); err != nil {
		return solanaObservationFields{}, err
	}
	return observationFieldsFromWire(&wire)
}

// solanaObservationFieldsFromPayload builds the record a guardian submits. It mirrors
// ix.rs observation_ix_from_body and vaa.rs parse_token_bridge_payload. payload is the
// VAA body after its 51-byte header. vaaDigest is keccak256(keccak256(body)).
//
// SECURITY: a transfer action requires a 133-byte fixed head and at most
// maxTransferPayloadLen trailing bytes. Any other action carries zeroed transfer fields.
func solanaObservationFieldsFromPayload(chain vaa.ChainID, emitter vaa.Address, sequence uint64, payload []byte, vaaDigest [32]byte) (*solanaObservationFields, error) {
	if len(payload) == 0 {
		return nil, errors.New("observation fields: empty payload")
	}

	fields := &solanaObservationFields{
		Action:    payload[0],
		Chain:     chain,
		Emitter:   emitter,
		Sequence:  sequence,
		VaaDigest: vaaDigest,
	}
	if !vaa.IsTransfer(payload) {
		if err := fields.setContentDigest(); err != nil {
			return nil, err
		}
		return fields, nil
	}

	if len(payload) < tokenBridgeTransferLen {
		return nil, fmt.Errorf("observation fields: transfer payload wants at least %d bytes, got %d", tokenBridgeTransferLen, len(payload))
	}
	if extra := len(payload) - tokenBridgeTransferLen; extra > maxTransferPayloadLen {
		return nil, fmt.Errorf("observation fields: transfer payload carries %d extra bytes, limit %d", extra, maxTransferPayloadLen)
	}

	// TestTokenBridgeTransferWireMatchesSDK checks this layout against vaa.DecodeTransferPayloadHdr.
	var transfer tokenBridgeTransferWire
	if err := decodeWire(payload[:tokenBridgeTransferLen], &transfer); err != nil {
		return nil, fmt.Errorf("observation fields: %w", err)
	}
	fields.Amount = transfer.Amount
	fields.TokenAddress = transfer.TokenAddress
	fields.TokenChain = vaa.ChainID(transfer.TokenChain.Uint16())
	fields.RecipientChain = vaa.ChainID(transfer.RecipientChain.Uint16())
	if err := fields.setContentDigest(); err != nil {
		return nil, err
	}
	return fields, nil
}

// solanaTxID is a source-chain transaction id in a form ix_data.rs TxId accepts: exactly
// hashTxIDLen or signatureTxIDLen bytes. Build with newSolanaTxID or parseSolanaTxID.
type solanaTxID struct {
	length uint8
	padded [signatureTxIDLen]byte
}

// newSolanaTxID copies id, which must be exactly hashTxIDLen or signatureTxIDLen bytes.
func newSolanaTxID(id []byte) (solanaTxID, error) {
	var txID solanaTxID
	if len(id) != hashTxIDLen && len(id) != signatureTxIDLen {
		return txID, fmt.Errorf("tx id: want %d or %d bytes, got %d", hashTxIDLen, signatureTxIDLen, len(id))
	}
	txID.length = uint8(len(id)) // #nosec G115 -- len(id) is 32 or 64, checked above
	copy(txID.padded[:], id)
	return txID, nil
}

// parseSolanaTxID mirrors ix_data.rs TxId::parse over the tx_id_len byte and the padded field.
//
// SECURITY: the length is exactly 32 or 64. Every byte after the length is zero. Thus one id
// has one encoding.
func parseSolanaTxID(length uint8, padded [signatureTxIDLen]byte) (solanaTxID, error) {
	var txID solanaTxID
	switch int(length) {
	case hashTxIDLen:
		if !bytes.Equal(padded[hashTxIDLen:], make([]byte, signatureTxIDLen-hashTxIDLen)) {
			return txID, errors.New("tx id: nonzero padding past a 32-byte id")
		}
	case signatureTxIDLen:
	default:
		return txID, fmt.Errorf("tx id: length byte %d is neither %d nor %d", length, hashTxIDLen, signatureTxIDLen)
	}
	txID.length = length
	txID.padded = padded
	return txID, nil
}

// Bytes is a copy of the id, without padding.
func (t solanaTxID) Bytes() []byte {
	return bytes.Clone(t.padded[:t.length])
}

// valid reports whether t came from a constructor rather than the zero value.
func (t solanaTxID) valid() bool {
	return t.length == hashTxIDLen || t.length == signatureTxIDLen
}

// solanaSubmitObservationsIx holds decoded submit_observations instruction data. The
// audit path reads TxID, Chain, Emitter and Sequence from this one record.
type solanaSubmitObservationsIx struct {
	GuardianSetIndex uint32
	GuardianIndex    uint8
	// r ‖ s ‖ recovery_id.
	Signature [submitSignatureLen]byte
	TxID      solanaTxID
	solanaObservationFields
}

// parseSubmitObservationsIxData decodes raw submit_observations instruction data: the
// discriminator and SubmitObservationsIxData, fixed size.
func parseSubmitObservationsIxData(instructionData []byte) (*solanaSubmitObservationsIx, error) {
	var wire submitObservationsInstructionWire
	if err := decodeWire(instructionData, &wire); err != nil {
		return nil, err
	}
	if wire.Discriminator != submitObservationsDiscriminator {
		return nil, fmt.Errorf("submit_observations instruction data: discriminator mismatch, want %d got %d", submitObservationsDiscriminator, wire.Discriminator)
	}

	fields, err := observationFieldsFromWire(&wire.Data.Fields)
	if err != nil {
		return nil, fmt.Errorf("submit_observations instruction data: %w", err)
	}
	txID, err := parseSolanaTxID(wire.Data.TxIDLen, wire.Data.TxID)
	if err != nil {
		return nil, fmt.Errorf("submit_observations instruction data: %w", err)
	}
	return &solanaSubmitObservationsIx{
		GuardianSetIndex:        wire.Data.GuardianSetIndex,
		GuardianIndex:           wire.Data.GuardianIndex,
		Signature:               wire.Data.Signature,
		TxID:                    txID,
		solanaObservationFields: fields,
	}, nil
}
