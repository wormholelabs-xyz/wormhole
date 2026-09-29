// Layout decoders and PDA derivations for the svm/accountant NTT program. The wire structs,
// layout constants and test fixtures are generated: run `just go-codegen` in svm/accountant.

package accountant

import (
	"fmt"

	"github.com/ethereum/go-ethereum/crypto"
	"github.com/gagliardetto/solana-go"
	"github.com/wormhole-foundation/wormhole/sdk/vaa"
)

// solanaNttObservationFields is the 117-byte record the NTT program hashes for both the
// content digest and the signing digest. ix_data.rs NttObservationFieldsAndDigest.
type solanaNttObservationFields struct {
	Chain    vaa.ChainID
	Emitter  vaa.Address
	Sequence uint64
	// The transceiver: the emitter, or the DeliveryInstruction sender for a relayed message.
	Sender         vaa.Address
	RecipientChain vaa.ChainID
	// Decimals of TrimmedAmount.
	TrimmedDecimals uint8
	// Raw TrimmedAmount. The program normalizes it at quorum.
	TrimmedAmount uint64
	// keccak256(keccak256(body)). Equals the inner digest field of the pending entry.
	VaaDigest [32]byte

	// keccak256(keccak256(pack())): the pending-PDA seed and the commit-log digest. The
	// constructors set it.
	contentDigest [32]byte
}

// wire is the record in the program's layout.
func (f *solanaNttObservationFields) wire() nttObservationFieldsWire {
	return nttObservationFieldsWire{
		Chain:           newBE16(uint16(f.Chain)),
		Emitter:         f.Emitter,
		Sequence:        newBE64(f.Sequence),
		Sender:          f.Sender,
		RecipientChain:  newBE16(uint16(f.RecipientChain)),
		TrimmedDecimals: f.TrimmedDecimals,
		TrimmedAmount:   newBE64(f.TrimmedAmount),
		VaaDigest:       f.VaaDigest,
	}
}

// pack serializes the record in the program's field order.
func (f *solanaNttObservationFields) pack() ([]byte, error) {
	wire := f.wire()
	return encodeWire(&wire)
}

func (f *solanaNttObservationFields) computeContentDigest() ([32]byte, error) {
	packed, err := f.pack()
	if err != nil {
		return [32]byte{}, err
	}
	return [32]byte(crypto.Keccak256(crypto.Keccak256(packed))), nil
}

func (f *solanaNttObservationFields) setContentDigest() error {
	digest, err := f.computeContentDigest()
	if err != nil {
		return err
	}
	f.contentDigest = digest
	return nil
}

func (f *solanaNttObservationFields) identity() (vaa.ChainID, vaa.Address, uint64) {
	return f.Chain, f.Emitter, f.Sequence
}

func (f *solanaNttObservationFields) committedDigest() [32]byte {
	return f.contentDigest
}

// submitInstructionData is the discriminator and NttSubmitObservationsIxData for f.
func (f *solanaNttObservationFields) submitInstructionData(head solanaSubmitHead) ([]byte, error) {
	return encodeWire(&nttSubmitObservationsInstructionWire{
		Discriminator: nttSubmitObservationsDiscriminator,
		Data: nttSubmitObservationsIxDataWire{
			GuardianSetIndex: head.guardianSetIndex,
			GuardianIndex:    head.guardianIndex,
			Signature:        head.signature,
			TxIDLen:          head.txID.length,
			TxID:             head.txID.padded,
			Fields:           f.wire(),
		},
	})
}

// nttObservationFieldsFromWire converts the program's layout and sets the content digest.
func nttObservationFieldsFromWire(wire *nttObservationFieldsWire) (solanaNttObservationFields, error) {
	f := solanaNttObservationFields{
		Chain:           vaa.ChainID(wire.Chain.Uint16()),
		Emitter:         wire.Emitter,
		Sequence:        wire.Sequence.Uint64(),
		Sender:          wire.Sender,
		RecipientChain:  vaa.ChainID(wire.RecipientChain.Uint16()),
		TrimmedDecimals: wire.TrimmedDecimals,
		TrimmedAmount:   wire.TrimmedAmount.Uint64(),
		VaaDigest:       wire.VaaDigest,
	}
	return f, f.setContentDigest()
}

// unpackNttObservationFields reads the record from exactly nttObservationFieldsLen bytes.
func unpackNttObservationFields(data []byte) (solanaNttObservationFields, error) {
	var wire nttObservationFieldsWire
	if err := decodeWire(data, &wire); err != nil {
		return solanaNttObservationFields{}, err
	}
	return nttObservationFieldsFromWire(&wire)
}

// solanaNttSubmitObservationsIx holds decoded NTT submit_observations instruction data.
type solanaNttSubmitObservationsIx struct {
	GuardianSetIndex uint32
	GuardianIndex    uint8
	// r ‖ s ‖ recovery_id.
	Signature [submitSignatureLen]byte
	TxID      solanaTxID
	solanaNttObservationFields
}

// parseNttSubmitObservationsIxData decodes raw NTT submit_observations instruction data: the
// discriminator and NttSubmitObservationsIxData, fixed size.
func parseNttSubmitObservationsIxData(instructionData []byte) (*solanaNttSubmitObservationsIx, error) {
	var wire nttSubmitObservationsInstructionWire
	if err := decodeWire(instructionData, &wire); err != nil {
		return nil, err
	}
	if wire.Discriminator != nttSubmitObservationsDiscriminator {
		return nil, fmt.Errorf("ntt submit_observations instruction data: discriminator mismatch, want %d got %d", nttSubmitObservationsDiscriminator, wire.Discriminator)
	}

	fields, err := nttObservationFieldsFromWire(&wire.Data.Fields)
	if err != nil {
		return nil, fmt.Errorf("ntt submit_observations instruction data: %w", err)
	}
	txID, err := parseSolanaTxID(wire.Data.TxIDLen, wire.Data.TxID)
	if err != nil {
		return nil, fmt.Errorf("ntt submit_observations instruction data: %w", err)
	}
	return &solanaNttSubmitObservationsIx{
		GuardianSetIndex:           wire.Data.GuardianSetIndex,
		GuardianIndex:              wire.Data.GuardianIndex,
		Signature:                  wire.Data.Signature,
		TxID:                       txID,
		solanaNttObservationFields: fields,
	}, nil
}

// deriveTransceiverHubPDA mirrors pda.rs TransceiverHubKey.
func deriveTransceiverHubPDA(program solana.PublicKey, chain vaa.ChainID, address vaa.Address) (solana.PublicKey, error) {
	pda, _, err := solana.FindProgramAddress([][]byte{
		transceiverHubSeedPrefix,
		newBE16(uint16(chain)).bytes(),
		address[:],
	}, program)
	if err != nil {
		return solana.PublicKey{}, fmt.Errorf("derive transceiver hub PDA: %w", err)
	}
	return pda, nil
}

// deriveTransceiverPeerPDA mirrors pda.rs TransceiverPeerKey.
func deriveTransceiverPeerPDA(program solana.PublicKey, chain vaa.ChainID, address vaa.Address, destChain vaa.ChainID) (solana.PublicKey, error) {
	pda, _, err := solana.FindProgramAddress([][]byte{
		transceiverPeerSeedPrefix,
		newBE16(uint16(chain)).bytes(),
		address[:],
		newBE16(uint16(destChain)).bytes(),
	}, program)
	if err != nil {
		return solana.PublicKey{}, fmt.Errorf("derive transceiver peer PDA: %w", err)
	}
	return pda, nil
}

// solanaNttHub is the token identity a transceiver's balances are keyed on.
type solanaNttHub struct {
	Chain   vaa.ChainID
	Address vaa.Address
}

// checkTransceiverKey requires the key fields of a hub or peer account to equal the ones
// its PDA was derived from.
//
// SECURITY: the chain and address are PDA seeds, so a mismatch means the account is not the
// one derived for the transceiver.
func checkTransceiverKey(name string, chain vaa.ChainID, address vaa.Address, wantChain vaa.ChainID, wantAddress vaa.Address) error {
	if chain != wantChain {
		return fmt.Errorf("%s account: chain %d, want %d", name, chain, wantChain)
	}
	if address != wantAddress {
		return fmt.Errorf("%s account: address %s, want %s", name, address, wantAddress)
	}
	return nil
}

// parseTransceiverHubAccount decodes the TransceiverHubLayout at (chain, address).
func parseTransceiverHubAccount(data []byte, chain vaa.ChainID, address vaa.Address) (solanaNttHub, error) {
	var wire transceiverHubWire
	if err := decodeWire(data, &wire); err != nil {
		return solanaNttHub{}, err
	}
	if wire.Tag != transceiverHubTag {
		return solanaNttHub{}, fmt.Errorf("transceiver hub account: tag mismatch, want %d got %d", transceiverHubTag, wire.Tag)
	}
	if err := checkTransceiverKey("transceiver hub", vaa.ChainID(wire.Chain), wire.Address, chain, address); err != nil {
		return solanaNttHub{}, err
	}
	return solanaNttHub{Chain: vaa.ChainID(wire.HubChain), Address: wire.HubAddress}, nil
}

// parseTransceiverPeerAccount decodes the TransceiverPeerLayout at (chain, address,
// destChain) and returns the peer address.
func parseTransceiverPeerAccount(data []byte, chain vaa.ChainID, address vaa.Address, destChain vaa.ChainID) (vaa.Address, error) {
	var wire transceiverPeerWire
	if err := decodeWire(data, &wire); err != nil {
		return vaa.Address{}, err
	}
	if wire.Tag != transceiverPeerTag {
		return vaa.Address{}, fmt.Errorf("transceiver peer account: tag mismatch, want %d got %d", transceiverPeerTag, wire.Tag)
	}
	if err := checkTransceiverKey("transceiver peer", vaa.ChainID(wire.Chain), wire.Address, chain, address); err != nil {
		return vaa.Address{}, err
	}
	if got := vaa.ChainID(wire.DestChain); got != destChain {
		return vaa.Address{}, fmt.Errorf("transceiver peer account: dest chain %d, want %d", got, destChain)
	}
	return wire.PeerAddress, nil
}
