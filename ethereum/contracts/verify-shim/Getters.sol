// contracts/Getters.sol
// SPDX-License-Identifier: Apache 2

pragma solidity ^0.8.0;

import "./State.sol";
import "../interfaces/IWormhole.sol";

abstract contract Getters is State {
    function getGuardianSet(uint32 index) public view returns (IWormhole.GuardianSet memory) {
        return State.guardianSets[index];
    }

    /// @dev Returns a sparse guardian set structure, with only the desired keys present.
    /// This is useful for reducing gas costs when only a subset of the keys is needed.
    /// A simpler option would be to just return a storage pointer, which would
    /// then only SLOAD the keys that are actually used. However, we also
    /// provide a fallback option here where if the guardian set is not yet
    /// cached in this contract, we fall back to reading from the wormhole
    /// contract, which, given it's an external contract, cannot return a
    /// storage pointer.
    /// Instead, we return a memory pointer with the right shape, meaning this
    /// function is a drop-in replacement for `getGuardianSet`.
    /// When doing ecrecover, the `verifySignatures` function checks that the
    /// recovered signer is not 0 (which it would be in case of invalid
    /// signatures), so returning a guardian set with 0 keys is not a security risk.
    /// (And it wouldn't be anyway since the 0 addresses will never be accessed).
    ///
    /// INVARIANT: The returned guardian set has the correct keys at the needed
    /// indices, but might have outdated expiration time.
    function getGuardianSetSparse(uint32 index, uint8[] memory indices) internal view returns (IWormhole.GuardianSet memory) {
        // First see if we have the guardian set in our state. Instead of
        // loading the entire set into memory ahead of time, we just load the
        // keys length, which is a single storage read.
        uint256 length = State.guardianSets[index].keys.length;
        if (length == 0) {
            // If we don't have the guardian set in our state, we can read it
            // from the wormhole contract. This is a fallback option.
            return wormhole.getGuardianSet(index);

        }
        // Otherwise, construct the guardian set with only the desired keys.
        IWormhole.GuardianSet memory guardianSet;
        guardianSet.keys = new address[](length);
        guardianSet.expirationTime = State.guardianSets[index].expirationTime;
        for (uint256 i = 0; i < indices.length; i++) {
            uint8 idx = indices[i];
            require(idx < length, "Index out of bounds");
            guardianSet.keys[idx] = State.guardianSets[index].keys[idx];
        }
        return guardianSet;
    }

    function getCurrentGuardianSetIndex() public view returns (uint32) {
        return wormhole.getCurrentGuardianSetIndex();
    }

    function getCachedCurrentGuardianSetIndex() public view returns (uint32) {
        return State.guardianSetIndex;
    }
}
