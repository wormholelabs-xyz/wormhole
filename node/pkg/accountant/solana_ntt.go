// Transceiver message parsing and record construction for the Solana NTT accountant. The wire
// structs and constants are generated in solana_ntt_layout_gen.go.

package accountant

import (
	"encoding/binary"
	"errors"
	"fmt"

	"github.com/certusone/wormhole/node/pkg/common"
	"github.com/wormhole-foundation/wormhole/sdk/vaa"
)

// nttTransfer is the routing data of one NativeTokenTransfer, trimmed fields as sent.
type nttTransfer struct {
	TrimmedDecimals uint8
	TrimmedAmount   uint64
	ToChain         vaa.ChainID
}

var errMalformedNttTransfer = errors.New("ntt transfer: malformed")

// nttSplitU16Prefixed splits a u16 big-endian length-prefixed blob off the front of b.
func nttSplitU16Prefixed(b []byte) ([]byte, []byte, bool) {
	if len(b) < nttLengthPrefixLen {
		return nil, nil, false
	}
	n := int(binary.BigEndian.Uint16(b[:nttLengthPrefixLen]))
	if len(b)-nttLengthPrefixLen < n {
		return nil, nil, false
	}
	return b[nttLengthPrefixLen : nttLengthPrefixLen+n], b[nttLengthPrefixLen+n:], true
}

// nttParseTransceiverTransfer mirrors ntt/transfer.rs parse_ntt_transfer over a
// TransceiverMessage<WormholeTransceiver, NativeTokenTransfer>:
//
//	TransceiverHead | ManagerHead | NativeTokenTransfer | [additional_len | additional]
//	  | transceiver_payload_len | transceiver_payload
//
// SECURITY: each length-prefixed section must be consumed exactly and the input must end
// after the transceiver payload, as the program requires. Decimals the program cannot
// normalize are rejected here, since an observation carrying them fails at quorum.
func nttParseTransceiverTransfer(payload []byte) (nttTransfer, error) {
	if len(payload) < nttMinTransferLen {
		return nttTransfer{}, fmt.Errorf("%w: %d bytes, want at least %d", errMalformedNttTransfer, len(payload), nttMinTransferLen)
	}

	var head nttTransceiverHeadWire
	if err := decodeWire(payload[:nttTransceiverHeadLen], &head); err != nil {
		return nttTransfer{}, fmt.Errorf("%w: %w", errMalformedNttTransfer, err)
	}
	if head.Prefix != nttTransceiverMessagePrefix {
		return nttTransfer{}, fmt.Errorf("%w: transceiver prefix %x", errMalformedNttTransfer, head.Prefix)
	}
	rest := payload[nttTransceiverHeadLen:]
	managerLen := int(head.ManagerPayloadLen.Uint16())
	if len(rest) < managerLen {
		return nttTransfer{}, fmt.Errorf("%w: manager payload length", errMalformedNttTransfer)
	}
	managerBytes, rest := rest[:managerLen], rest[managerLen:]
	_, rest, ok := nttSplitU16Prefixed(rest)
	if !ok {
		return nttTransfer{}, fmt.Errorf("%w: transceiver payload length", errMalformedNttTransfer)
	}
	if len(rest) != 0 {
		return nttTransfer{}, fmt.Errorf("%w: %d trailing bytes", errMalformedNttTransfer, len(rest))
	}

	if len(managerBytes) < nttManagerHeadLen {
		return nttTransfer{}, fmt.Errorf("%w: manager payload is %d bytes", errMalformedNttTransfer, len(managerBytes))
	}
	var manager nttManagerHeadWire
	if err := decodeWire(managerBytes[:nttManagerHeadLen], &manager); err != nil {
		return nttTransfer{}, fmt.Errorf("%w: %w", errMalformedNttTransfer, err)
	}
	inner := managerBytes[nttManagerHeadLen:]
	if len(inner) != int(manager.PayloadLen.Uint16()) {
		return nttTransfer{}, fmt.Errorf("%w: transfer length", errMalformedNttTransfer)
	}

	if len(inner) < nttNativeTokenTransferLen {
		return nttTransfer{}, fmt.Errorf("%w: transfer is %d bytes", errMalformedNttTransfer, len(inner))
	}
	var transfer nttNativeTokenTransferWire
	if err := decodeWire(inner[:nttNativeTokenTransferLen], &transfer); err != nil {
		return nttTransfer{}, fmt.Errorf("%w: %w", errMalformedNttTransfer, err)
	}
	if transfer.Prefix != nttNativeTokenTransferPrefix {
		return nttTransfer{}, fmt.Errorf("%w: ntt prefix %x", errMalformedNttTransfer, transfer.Prefix)
	}
	// An additional payload is present iff bytes follow the fixed fields.
	if extra := inner[nttNativeTokenTransferLen:]; len(extra) != 0 {
		_, after, ok := nttSplitU16Prefixed(extra)
		if !ok || len(after) != 0 {
			return nttTransfer{}, fmt.Errorf("%w: additional payload length", errMalformedNttTransfer)
		}
	}

	t := nttTransfer{
		TrimmedDecimals: transfer.Decimals,
		TrimmedAmount:   transfer.Amount.Uint64(),
		ToChain:         vaa.ChainID(transfer.ToChain.Uint16()),
	}
	if t.TrimmedDecimals > maxNttTrimmedDecimals {
		return nttTransfer{}, fmt.Errorf("%w: decimals %d past %d", errMalformedNttTransfer, t.TrimmedDecimals, maxNttTrimmedDecimals)
	}
	return t, nil
}

// nttResolveSender returns the transceiver that authored msg and its transceiver message.
// A direct emitter is the transceiver. A configured relayer wraps the transceiver in a
// DeliveryInstruction.
//
// SECURITY: the program accepts sender != emitter only for the chain's registered relayer,
// so the sender must come from the relayer envelope and nowhere else.
func (acct *Accountant) nttResolveSender(msg *common.MessagePublication) (vaa.Address, []byte, error) {
	key := emitterKey{emitterChainId: msg.EmitterChain, emitterAddr: msg.EmitterAddress}
	if _, direct := acct.nttDirectEmitters[key]; direct {
		return msg.EmitterAddress, msg.Payload, nil
	}
	if _, relayer := acct.nttArEmitters[key]; relayer {
		ok, sender, payload := nttParseArPayload(msg.Payload)
		if !ok {
			return vaa.Address{}, nil, errors.New("ntt sender: malformed delivery instruction")
		}
		if vaa.Address(sender) == msg.EmitterAddress {
			return vaa.Address{}, nil, errors.New("ntt sender: a relayed message names the relayer as its sender")
		}
		return sender, payload, nil
	}
	return vaa.Address{}, nil, errors.New("ntt sender: the emitter is neither an NTT transceiver nor a relayer")
}

// solanaNttObservationFieldsFromMessage builds the record a guardian submits to the Solana
// NTT accountant, mirroring crates/test-harness/src/ntt.rs Observation::direct and ::relayed.
func (acct *Accountant) solanaNttObservationFieldsFromMessage(msg *common.MessagePublication, vaaDigest [32]byte) (*solanaNttObservationFields, error) {
	sender, message, err := acct.nttResolveSender(msg)
	if err != nil {
		return nil, err
	}
	transfer, err := nttParseTransceiverTransfer(message)
	if err != nil {
		return nil, err
	}

	fields := &solanaNttObservationFields{
		Chain:           msg.EmitterChain,
		Emitter:         msg.EmitterAddress,
		Sequence:        msg.Sequence,
		Sender:          sender,
		RecipientChain:  transfer.ToChain,
		TrimmedDecimals: transfer.TrimmedDecimals,
		TrimmedAmount:   transfer.TrimmedAmount,
		VaaDigest:       vaaDigest,
	}
	if err := fields.setContentDigest(); err != nil {
		return nil, err
	}
	return fields, nil
}
