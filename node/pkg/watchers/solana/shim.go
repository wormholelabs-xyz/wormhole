// This code is responsible for processing events from the shim contract and publishing them as message observations,
// similar to what the watcher does for legacy account-based events from the core contract.
//
// There are three events that are generated for each shim event:
// - Shim PostMessage - This is the first message generated by the shim and it contains some of the data needed to generate an observation.
//
// - Core PostMessage - This is the existing event generated by the core, but in this scenario it is generated by a call from the shim contract.
//   The core event generated on behalf of the shim is always marked unreliable and has an empty payload. This code enforces that.
//
// - Shim MessageEvent - This event appears after the core PostMessage and contains the remaining fields needed to generate an observation.
//
// There are two ways the shim can publish events: Direct and integrator originated.
//
// The direct scenario is when a user directly calls the shim contract to post a message. This looks as follows:
// - The shim PostMessage appears as a top-level instruction in the transaction.
// - The inner instructions for the matching top-level instruction (innerInstructions.Index matches the top-level instruction index) will contain
//   the core PostMessage and the shim MessageEvent. Note that there may be other inner instructions that will be ignored. Also note that the
//   code enforces that the shim MessageEvent follows the core PostMessage.
//
// The integrator originated scenario happens when the shim PostMessage event appears in the inner instructions. This looks as follows:
// - Everything for this event appears in a single inner instruction set (the entries for a given innerInstructions.Index). Note that there
//   may be other instructions in that set which are ignored, and there could be the instructions for multiple shim events which are handled separately.
// - Within the given inner instruction set, there is a shim PostMessage, a core PostMessage and a ShimMessageEvent.
//
// When processing inner instructions, this code ensures that events that are processed as part of a shim transaction are not processed more than once using the ShimAlreadyProcessed map.

package solana

import (
	"bytes"
	"encoding/hex"
	"fmt"
	"time"

	"github.com/certusone/wormhole/node/pkg/common"
	"github.com/gagliardetto/solana-go"
	"github.com/gagliardetto/solana-go/rpc"
	"github.com/near/borsh-go"
	"go.uber.org/zap"
	"go.uber.org/zap/zapcore"
)

const (
	shimPostMessageDiscriminatorStr  = "d63264d12622074c"
	shimMessageEventDiscriminatorStr = "e445a52e51cb9a1d441b8f004d4c8970"
)

type (

	// ShimPostMessageData defines the shim PostMessage payload following the eight byte discriminator (shimPostMessageDiscriminatorStr)
	ShimPostMessageData struct {
		Nonce            uint32
		ConsistencyLevel ConsistencyLevel
		Payload          []byte
	}

	// ShimMessageEventData defines the shim MessageEvent payload following the sixteen byte discriminator (shimMessageEventDiscriminatorStr)
	ShimMessageEventData struct {
		EmitterAddress [32]byte
		Sequence       uint64
		Timestamp      uint32
	}

	// ShimAlreadyProcessed is a map that tracks the inner index identifier pairs already processed as part of shim transactions.
	// This allows us to avoid double processing instructions.
	ShimAlreadyProcessed map[ShimInnerIdx]struct{}

	// InnerIdx is the key to the set of instructions already processed as part of shim transactions
	ShimInnerIdx struct {
		topLevelIdx int
		innerIdx    int
	}
)

// add adds the specified inner index identifier pair into a ShimAlreadyProcessed.
func (sap *ShimAlreadyProcessed) add(outerIdx int, innerIdx int) {
	(*sap)[ShimInnerIdx{outerIdx, innerIdx}] = struct{}{}
}

// exists returns true if the specified inner index identifier pair is in a ShimAlreadyProcessed.
func (sap *ShimAlreadyProcessed) exists(outerIdx int, innerIdx int) bool {
	_, exists := (*sap)[ShimInnerIdx{outerIdx, innerIdx}]
	return exists
}

// shimSetup performs any initialization that is specific to monitoring the shim contract.
func (s *SolanaWatcher) shimSetup() {
	s.shimEnabled = s.shimContractStr != ""
	if s.shimEnabled {
		var err error
		s.shimPostMessageDiscriminator, err = hex.DecodeString(shimPostMessageDiscriminatorStr)
		if err != nil {
			panic("failed to decode shim post message discriminator")
		}
		s.shimMessageEventDiscriminator, err = hex.DecodeString(shimMessageEventDiscriminatorStr)
		if err != nil {
			panic("failed to decode shim post message discriminator")
		}
	}
}

// shimMatchPrefix verifies that the instruction data starts with the specified prefix bytes.
func shimMatchPrefix(discriminator []byte, buf []byte) bool {
	if len(buf) < len(discriminator) {
		return false
	}
	return bytes.Equal(discriminator, buf[:len(discriminator)])
}

// shimParsePostMessage parses a shim PostMessage and returns the results.
func shimParsePostMessage(shimPostMessageDiscriminator []byte, buf []byte) (*ShimPostMessageData, error) {
	if !shimMatchPrefix(shimPostMessageDiscriminator, buf) {
		return nil, nil
	}

	data := new(ShimPostMessageData)
	if err := borsh.Deserialize(data, buf[len(shimPostMessageDiscriminator):]); err != nil {
		return nil, fmt.Errorf("failed to deserialize shim post message: %w", err)
	}

	return data, nil
}

// shimVerifyCoreMessage verifies that an instruction from the core contract is what we expect to accompany a shim instruction.
// This includes being marked unreliable and having a zero length payload.
func shimVerifyCoreMessage(buf []byte) (bool, error) {
	if len(buf) == 0 {
		return false, nil
	}

	if buf[0] != postMessageUnreliableInstructionID {
		return false, nil
	}

	var data PostMessageData
	if err := borsh.Deserialize(&data, buf[1:]); err != nil {
		return false, fmt.Errorf("failed to deserialize core instruction data: %w", err)
	}

	if len(data.Payload) != 0 {
		return false, nil
	}

	return true, nil
}

// shimParseMessageEvent parses a shim MessageEvent and returns the results.
func shimParseMessageEvent(shimMessageEventDiscriminator []byte, buf []byte) (*ShimMessageEventData, error) {
	if !shimMatchPrefix(shimMessageEventDiscriminator, buf) {
		return nil, nil
	}

	data := new(ShimMessageEventData)
	if err := borsh.Deserialize(data, buf[len(shimMessageEventDiscriminator):]); err != nil {
		return nil, fmt.Errorf("failed to deserialize shim message event: %w", err)
	}

	return data, nil
}

// shimProcessTopLevelInstruction handles a top-level instruction where the program ID matches the shim contract. It does the following:
// - Verifies that the instruction is a shim PostMessage. If not, it just returns. If it is, it parses it.
// - Searches the sets of inner instructions to find the ones generated by this top-level instruction (by matching the index).
// - Searches through those inner instructions to find the entry for the core. Makes sure it is unreliable with no payload.
// - Searches for the inner shim MessageEvent to get the remaining fields needed to generate the observation.
// - Publishes the observation.
func (s *SolanaWatcher) shimProcessTopLevelInstruction(
	logger *zap.Logger,
	whProgramIndex uint16,
	shimProgramIndex uint16,
	tx *solana.Transaction,
	innerInstructions []rpc.InnerInstruction,
	topLevelIndex int,
	alreadyProcessed ShimAlreadyProcessed,
	isReobservation bool,
) (bool, error) {
	topLevelIdx := uint16(topLevelIndex)
	inst := tx.Message.Instructions[topLevelIdx]

	// The only top-level instruction generated by the shim contract is the PostMessage event. Parse that to get
	// the fields we need to generate an observation.
	postMessage, err := shimParsePostMessage(s.shimPostMessageDiscriminator, inst.Data)
	if err != nil {
		return false, fmt.Errorf("failed to parse top-level shim instruction %d: %w", topLevelIdx, err)
	}

	if postMessage == nil {
		return false, nil
	}

	// Find the set of inner instructions that go with this top level instruction by matching the index.
	outerIdx := -1
	for idx, inner := range innerInstructions {
		if inner.Index == topLevelIdx {
			outerIdx = int(idx)
			break
		}
	}

	if outerIdx == -1 {
		return false, fmt.Errorf("failed to find inner instructions for top-level shim instruction %d", topLevelIdx)
	}

	// Process the inner instructions associated with this shim top-level instruction and produce an observation event.
	err = s.shimProcessRest(logger, whProgramIndex, shimProgramIndex, tx, innerInstructions[outerIdx].Instructions, outerIdx, 0, postMessage, alreadyProcessed, isReobservation)
	if err != nil {
		return false, fmt.Errorf("failed to process inner instructions for top-level shim instruction %d: %w", topLevelIdx, err)
	}

	return true, nil
}

// shimProcessInnerInstruction handles an inner instruction where the program ID matches the shim contract. It does the following:
// - Verifies that the instruction is a shim PostMessage. If not, it just returns. If it is, it parses it.
// - Searches through the subsequent inner instructions in the set to find the entry for the core. Makes sure it is unreliable with no payload.
// - Searches for the inner shim MessageEvent to get the remaining fields needed to generate the observation.
// - Publishes the observation.
func (s *SolanaWatcher) shimProcessInnerInstruction(
	logger *zap.Logger,
	whProgramIndex uint16,
	shimProgramIndex uint16,
	tx *solana.Transaction,
	innerInstructions []solana.CompiledInstruction,
	outerIdx int,
	startIdx int,
	alreadyProcessed ShimAlreadyProcessed,
	isReobservation bool,
) (bool, error) {
	// See if this is a PostMessage event from the shim contract. If so, parse it. If not, bail out now.
	postMessage, err := shimParsePostMessage(s.shimPostMessageDiscriminator, innerInstructions[startIdx].Data)
	if err != nil {
		return false, fmt.Errorf("failed to parse inner shim post message instruction %d, %d: %w", outerIdx, startIdx, err)
	}

	if postMessage == nil {
		return false, nil
	}

	alreadyProcessed.add(outerIdx, startIdx)

	err = s.shimProcessRest(logger, whProgramIndex, shimProgramIndex, tx, innerInstructions, outerIdx, startIdx+1, postMessage, alreadyProcessed, isReobservation)
	if err != nil {
		return false, fmt.Errorf("failed to process inner instructions for inner shim instruction %d: %w", outerIdx, err)
	}

	return true, nil
}

// shimProcessRest performs the processing of the inner instructions that is common to both the direct and integrator case. It looks for the PostMessage from
// the core and the MessageEvent from the shim. Note that the startIdx parameter tells us where to start looking for these events. In the direct case, this
// will be zero. In the integrator case, it is one after the shim PostMessage event.
func (s *SolanaWatcher) shimProcessRest(
	logger *zap.Logger,
	whProgramIndex uint16,
	shimProgramIndex uint16,
	tx *solana.Transaction,
	innerInstructions []solana.CompiledInstruction,
	outerIdx int,
	startIdx int,
	postMessage *ShimPostMessageData,
	alreadyProcessed ShimAlreadyProcessed,
	isReobservation bool,
) error {
	// Loop through the inner instructions after the shim PostMessage and do the following:
	// 1) Find the core event and verify it is unreliable with an empty payload.
	// 2) Find the shim MessageEvent to get the rest of the fields we need for the observation.
	// 3) Verify that the shim MessageEvent comes after the core event.

	var verifiedCoreEvent bool
	var messageEvent *ShimMessageEventData
	var err error
	coreEventFound := false
	for idx := startIdx; idx < len(innerInstructions); idx++ {
		inst := innerInstructions[idx]
		if inst.ProgramIDIndex == whProgramIndex {
			if verifiedCoreEvent, err = shimVerifyCoreMessage(inst.Data); err != nil {
				return fmt.Errorf("failed to verify inner core instruction for shim instruction %d, %d: %w", outerIdx, idx, err)
			}
			alreadyProcessed.add(outerIdx, idx)
			coreEventFound = true
		} else if inst.ProgramIDIndex == shimProgramIndex {
			if !coreEventFound {
				return fmt.Errorf("detected an inner shim message event instruction before the core event for shim instruction %d, %d: %w", outerIdx, idx, err)
			}
			messageEvent, err = shimParseMessageEvent(s.shimMessageEventDiscriminator, inst.Data)
			if err != nil {
				return fmt.Errorf("failed to parse inner shim message event instruction for shim instruction %d, %d: %w", outerIdx, idx, err)
			}
			alreadyProcessed.add(outerIdx, idx)
		}

		if verifiedCoreEvent && messageEvent != nil {
			break
		}
	}

	if !verifiedCoreEvent {
		return fmt.Errorf("failed to find inner core instruction for shim instruction %d", outerIdx)
	}

	if messageEvent == nil {
		return fmt.Errorf("failed to find inner shim message event instruction for shim instruction %d", outerIdx)
	}

	commitment, err := postMessage.ConsistencyLevel.Commitment()
	if err != nil {
		return fmt.Errorf("failed to determine commitment for shim instruction %d: %w", outerIdx, err)
	}

	if !s.checkCommitment(commitment, isReobservation) {
		if logger.Level().Enabled(zapcore.DebugLevel) {
			logger.Debug("skipping shim message which does not match the watcher commitment",
				zap.Stringer("signature", tx.Signatures[0]),
				zap.String("message commitment", string(commitment)),
				zap.String("watcher commitment", string(s.commitment)),
			)
		}
		return nil
	}

	observation := &common.MessagePublication{
		TxID:             tx.Signatures[0][:],
		Timestamp:        time.Unix(int64(messageEvent.Timestamp), 0),
		Nonce:            postMessage.Nonce,
		Sequence:         messageEvent.Sequence,
		EmitterChain:     s.chainID,
		EmitterAddress:   messageEvent.EmitterAddress,
		Payload:          postMessage.Payload,
		ConsistencyLevel: uint8(postMessage.ConsistencyLevel),
		IsReobservation:  isReobservation,
		Unreliable:       false,
	}

	solanaMessagesConfirmed.WithLabelValues(s.networkName).Inc()

	if logger.Level().Enabled(s.msgObservedLogLevel) {
		logger.Log(s.msgObservedLogLevel, "message observed from shim",
			zap.Stringer("signature", tx.Signatures[0]),
			zap.Time("timestamp", observation.Timestamp),
			zap.Uint32("nonce", observation.Nonce),
			zap.Uint64("sequence", observation.Sequence),
			zap.Stringer("emitter_chain", observation.EmitterChain),
			zap.Stringer("emitter_address", observation.EmitterAddress),
			zap.Bool("isReobservation", false),
			zap.Binary("payload", observation.Payload),
			zap.Uint8("consistency_level", observation.ConsistencyLevel),
		)
	}

	s.msgC <- observation

	return nil
}