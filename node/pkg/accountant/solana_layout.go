// Layout decoders and PDA derivations for the svm/accountant program.
// Constants mirror svm/accountant/crates/definitions/src: state.rs,
// instructions/ix_data.rs, vaa.rs, constants/log.rs, constants/noreplay.rs,
// constants/seeds.rs. Fixtures in solana_layout_test.go come from
// programs/global-accountant/tests/go_fixture_vectors.rs.

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

const (
	// AccountantDigestLog::LEN, constants/log.rs.
	accountantDigestLogLen = 8 + 2 + 32 + 8 + 32 + 4

	// PendingObservationsLayout LEN, TAG and MAX_GUARDIANS, state.rs.
	pendingObservationsLen          = 88
	pendingObservationsTag          = 1
	pendingObservationsMaxGuardians = 128

	// solana-noreplay bucket: bump(1) + bitmap(128), constants/noreplay.rs.
	noreplayBucketLen     = 129
	noreplayBitmapOffset  = 1
	noreplayBitsPerBucket = 1024

	// Instruction::SubmitObservations and SubmitObservationsIxData::LEN, ix_data.rs.
	submitObservationsDispatchLen   = 1
	submitObservationsDiscriminator = 0
	submitObservationsIxDataLen     = 278

	// HASH_TX_ID_LEN and SIGNATURE_TX_ID_LEN, ix_data.rs.
	hashTxIDLen      = 32
	signatureTxIDLen = 64

	// ObservationFieldsAndDigest, ix_data.rs. The hashed record.
	observationFieldsLen = 143

	// TokenBridgeTransfer::LEN and MAX_TRANSFER_PAYLOAD_LEN, vaa.rs.
	tokenBridgeTransferLen = 133
	maxTransferPayloadLen  = 2000
)

// Field offsets inside the 143-byte hashed record, ix_data.rs
// ObservationFieldsAndDigest.
const (
	fieldsActionOffset         = 0
	fieldsChainOffset          = 1
	fieldsEmitterOffset        = 3
	fieldsSequenceOffset       = 35
	fieldsTokenChainOffset     = 43
	fieldsTokenAddressOffset   = 45
	fieldsRecipientChainOffset = 77
	fieldsAmountOffset         = 79
	fieldsVaaDigestOffset      = 111
)

// Field offsets inside submit_observations instruction data, past the
// 1-byte discriminator. SubmitObservationsIxData, ix_data.rs.
const (
	submitGuardianSetIndexOffset = 1
	submitGuardianIndexOffset    = 5
	submitSignatureOffset        = 6
	submitSignatureLen           = 65
	submitTxIDLenOffset          = 71
	submitTxIDOffset             = 72
	submitFieldsOffset           = 136
)

// Compile-time equalities: tx_id fills the gap before the fields, and the fields end the data.
const (
	_ = uint(submitFieldsOffset - submitTxIDOffset - signatureTxIDLen)
	_ = uint(submitTxIDOffset + signatureTxIDLen - submitFieldsOffset)
	_ = uint(submitFieldsOffset + observationFieldsLen - submitObservationsDispatchLen - submitObservationsIxDataLen)
	_ = uint(submitObservationsDispatchLen + submitObservationsIxDataLen - submitFieldsOffset - observationFieldsLen)
)

// ACCOUNTANT_DIGEST_LOG_TAG, constants/log.rs.
var accountantDigestLogTag = [8]byte{'A', 'C', 'C', 'D', 'G', 'S', 'T', 0}

// constants/seeds.rs. GUARDIAN_SET_SEED is the Core Bridge's own seed.
var (
	pendingObservationsSeedPrefix = []byte("pending")
	noreplayAuthoritySeedPrefix   = []byte("noreplay_authority")
	balanceAccountSeedPrefix      = []byte("account")
	chainRegistrationSeedPrefix   = []byte("chain_registration")
	guardianSetSeedPrefix         = []byte("GuardianSet")
)

// GlobalAccountantError codes returned as ProgramError::Custom, error.rs. Do not renumber.
const (
	solanaErrPayerMismatch            = 4
	solanaErrAlreadyAccounted         = 7
	solanaErrInvalidSignature         = 9
	solanaErrInvalidGuardianIndex     = 10
	solanaErrAlreadySigned            = 11
	solanaErrExpiredGuardianSet       = 12
	solanaErrMissingChainRegistration = 19
	solanaErrUnregisteredEmitter      = 20
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
	// 128-bit signature bitmap as four little-endian words.
	Signatures    [4]uint32
	ContentDigest [32]byte
	Payer         solana.PublicKey
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
	}
	for word := range obs.Signatures {
		start := 8 + word*4
		obs.Signatures[word] = binary.LittleEndian.Uint32(data[start : start+4])
	}
	copy(obs.ContentDigest[:], data[24:56])
	copy(obs.Payer[:], data[56:88])
	return obs, nil
}

// hasSignature mirrors PendingObservationsLayout::has_signature. Bit N lives in
// word N/32 at position N%32.
func (o *solanaPendingObs) hasSignature(index uint8) (bool, error) {
	if uint32(index) >= pendingObservationsMaxGuardians {
		return false, fmt.Errorf("pending observations account: guardian index %d is at or past the %d-bit bitmap", index, pendingObservationsMaxGuardians)
	}
	return o.Signatures[index/32]&(1<<(index%32)) != 0, nil
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
	var chainBE [2]byte
	binary.BigEndian.PutUint16(chainBE[:], uint16(chain))
	var sequenceBE [8]byte
	binary.BigEndian.PutUint64(sequenceBE[:], sequence)
	var guardianSetIndexBE [4]byte
	binary.BigEndian.PutUint32(guardianSetIndexBE[:], guardianSetIndex)

	pda, _, err := solana.FindProgramAddress([][]byte{
		pendingObservationsSeedPrefix,
		chainBE[:],
		emitter[:],
		sequenceBE[:],
		guardianSetIndexBE[:],
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

// deriveBalanceAccountPDA mirrors accounts/balance.rs derive_pda. chain is the side of
// the transfer the balance belongs to: the emitter chain for the source, the recipient
// chain for the destination.
func deriveBalanceAccountPDA(program solana.PublicKey, chain vaa.ChainID, tokenChain vaa.ChainID, tokenAddress [32]byte) (solana.PublicKey, error) {
	var chainBE [2]byte
	binary.BigEndian.PutUint16(chainBE[:], uint16(chain))
	var tokenChainBE [2]byte
	binary.BigEndian.PutUint16(tokenChainBE[:], uint16(tokenChain))

	pda, _, err := solana.FindProgramAddress([][]byte{
		balanceAccountSeedPrefix,
		chainBE[:],
		tokenChainBE[:],
		tokenAddress[:],
	}, program)
	if err != nil {
		return solana.PublicKey{}, fmt.Errorf("derive balance account PDA: %w", err)
	}
	return pda, nil
}

// deriveChainRegistrationPDA mirrors accounts/chain_registration.rs derive_pda.
func deriveChainRegistrationPDA(program solana.PublicKey, chain vaa.ChainID) (solana.PublicKey, error) {
	var chainBE [2]byte
	binary.BigEndian.PutUint16(chainBE[:], uint16(chain))

	pda, _, err := solana.FindProgramAddress([][]byte{
		chainRegistrationSeedPrefix,
		chainBE[:],
	}, program)
	if err != nil {
		return solana.PublicKey{}, fmt.Errorf("derive chain registration PDA: %w", err)
	}
	return pda, nil
}

// deriveGuardianSetPDA is the Core Bridge GuardianSet account for one set index.
func deriveGuardianSetPDA(coreBridge solana.PublicKey, guardianSetIndex uint32) (solana.PublicKey, error) {
	var indexBE [4]byte
	binary.BigEndian.PutUint32(indexBE[:], guardianSetIndex)

	pda, _, err := solana.FindProgramAddress([][]byte{
		guardianSetSeedPrefix,
		indexBE[:],
	}, coreBridge)
	if err != nil {
		return solana.PublicKey{}, fmt.Errorf("derive guardian set PDA: %w", err)
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
	// keccak256(keccak256(body)); equals the pending entry's inner digest field.
	VaaDigest [32]byte

	// keccak256(keccak256(pack())): the pending-PDA seed and the commit-log digest. Set by
	// the constructors.
	contentDigest [32]byte
}

// pack serializes the record in the program's field order.
func (f *solanaObservationFields) pack() [observationFieldsLen]byte {
	var out [observationFieldsLen]byte
	out[fieldsActionOffset] = f.Action
	binary.BigEndian.PutUint16(out[fieldsChainOffset:fieldsChainOffset+2], uint16(f.Chain))
	copy(out[fieldsEmitterOffset:fieldsEmitterOffset+32], f.Emitter[:])
	binary.BigEndian.PutUint64(out[fieldsSequenceOffset:fieldsSequenceOffset+8], f.Sequence)
	binary.BigEndian.PutUint16(out[fieldsTokenChainOffset:fieldsTokenChainOffset+2], uint16(f.TokenChain))
	copy(out[fieldsTokenAddressOffset:fieldsTokenAddressOffset+32], f.TokenAddress[:])
	binary.BigEndian.PutUint16(out[fieldsRecipientChainOffset:fieldsRecipientChainOffset+2], uint16(f.RecipientChain))
	copy(out[fieldsAmountOffset:fieldsAmountOffset+32], f.Amount[:])
	copy(out[fieldsVaaDigestOffset:fieldsVaaDigestOffset+32], f.VaaDigest[:])
	return out
}

func (f *solanaObservationFields) computeContentDigest() [32]byte {
	packed := f.pack()
	return [32]byte(crypto.Keccak256(crypto.Keccak256(packed[:])))
}

func (f *solanaObservationFields) setContentDigest() {
	f.contentDigest = f.computeContentDigest()
}

// unpackObservationFields reads the record from an exactly 143-byte slice.
//
// SECURITY: precondition len(data) == observationFieldsLen.
func unpackObservationFields(data []byte) (solanaObservationFields, error) {
	var f solanaObservationFields
	if len(data) != observationFieldsLen {
		return f, fmt.Errorf("observation fields: want %d bytes, got %d", observationFieldsLen, len(data))
	}
	f.Action = data[fieldsActionOffset]
	f.Chain = vaa.ChainID(binary.BigEndian.Uint16(data[fieldsChainOffset : fieldsChainOffset+2]))
	copy(f.Emitter[:], data[fieldsEmitterOffset:fieldsEmitterOffset+32])
	f.Sequence = binary.BigEndian.Uint64(data[fieldsSequenceOffset : fieldsSequenceOffset+8])
	f.TokenChain = vaa.ChainID(binary.BigEndian.Uint16(data[fieldsTokenChainOffset : fieldsTokenChainOffset+2]))
	copy(f.TokenAddress[:], data[fieldsTokenAddressOffset:fieldsTokenAddressOffset+32])
	f.RecipientChain = vaa.ChainID(binary.BigEndian.Uint16(data[fieldsRecipientChainOffset : fieldsRecipientChainOffset+2]))
	copy(f.Amount[:], data[fieldsAmountOffset:fieldsAmountOffset+32])
	copy(f.VaaDigest[:], data[fieldsVaaDigestOffset:fieldsVaaDigestOffset+32])
	f.setContentDigest()
	return f, nil
}

// solanaObservationFieldsFromPayload builds the record a guardian submits, mirroring
// ix.rs observation_ix_from_body and vaa.rs parse_token_bridge_payload. payload is the
// VAA body past its 51-byte header; vaaDigest is keccak256(keccak256(body)).
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
		fields.setContentDigest()
		return fields, nil
	}

	if len(payload) < tokenBridgeTransferLen {
		return nil, fmt.Errorf("observation fields: transfer payload wants at least %d bytes, got %d", tokenBridgeTransferLen, len(payload))
	}
	if extra := len(payload) - tokenBridgeTransferLen; extra > maxTransferPayloadLen {
		return nil, fmt.Errorf("observation fields: transfer payload carries %d extra bytes, limit %d", extra, maxTransferPayloadLen)
	}

	// Offsets match TokenBridgeTransfer, vaa.rs, and vaa.DecodeTransferPayloadHdr.
	copy(fields.Amount[:], payload[1:33])
	copy(fields.TokenAddress[:], payload[33:65])
	fields.TokenChain = vaa.ChainID(binary.BigEndian.Uint16(payload[65:67]))
	fields.RecipientChain = vaa.ChainID(binary.BigEndian.Uint16(payload[99:101]))
	fields.setContentDigest()
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
// SECURITY: the length is exactly 32 or 64 and every byte past it is zero, so one id has one
// encoding.
func parseSolanaTxID(length uint8, padded []byte) (solanaTxID, error) {
	var txID solanaTxID
	if len(padded) != signatureTxIDLen {
		return txID, fmt.Errorf("tx id: want a %d-byte padded field, got %d", signatureTxIDLen, len(padded))
	}
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
	copy(txID.padded[:], padded)
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

// solanaSubmitObservationsIx is decoded submit_observations instruction data. The
// audit path reads TxID, Chain, Emitter and Sequence from this one record.
type solanaSubmitObservationsIx struct {
	GuardianSetIndex uint32
	GuardianIndex    uint8
	// r ‖ s ‖ recovery_id.
	Signature [submitSignatureLen]byte
	TxID      solanaTxID
	solanaObservationFields
}

// parseSubmitObservationsIxData decodes raw submit_observations instruction data.
// Layout: discriminator(1) ‖ SubmitObservationsIxData(278), fixed size.
func parseSubmitObservationsIxData(instructionData []byte) (*solanaSubmitObservationsIx, error) {
	wantLen := submitObservationsDispatchLen + submitObservationsIxDataLen
	if len(instructionData) != wantLen {
		return nil, fmt.Errorf("submit_observations instruction data: want %d bytes, got %d", wantLen, len(instructionData))
	}
	if instructionData[0] != submitObservationsDiscriminator {
		return nil, fmt.Errorf("submit_observations instruction data: discriminator mismatch, want %d got %d", submitObservationsDiscriminator, instructionData[0])
	}

	fields, err := unpackObservationFields(instructionData[submitFieldsOffset:])
	if err != nil {
		return nil, fmt.Errorf("submit_observations instruction data: %w", err)
	}
	txID, err := parseSolanaTxID(instructionData[submitTxIDLenOffset], instructionData[submitTxIDOffset:submitFieldsOffset])
	if err != nil {
		return nil, fmt.Errorf("submit_observations instruction data: %w", err)
	}

	ix := &solanaSubmitObservationsIx{
		GuardianSetIndex:        binary.LittleEndian.Uint32(instructionData[submitGuardianSetIndexOffset : submitGuardianSetIndexOffset+4]),
		GuardianIndex:           instructionData[submitGuardianIndexOffset],
		TxID:                    txID,
		solanaObservationFields: fields,
	}
	copy(ix.Signature[:], instructionData[submitSignatureOffset:submitSignatureOffset+submitSignatureLen])
	return ix, nil
}
