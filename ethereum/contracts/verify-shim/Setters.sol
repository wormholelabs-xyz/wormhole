// contracts/Setters.sol
// SPDX-License-Identifier: Apache 2

pragma solidity ^0.8.0;

import "./State.sol";

abstract contract Setters is State {
    function storeGuardianSet(IWormhole.GuardianSet memory set, uint32 index) internal {
        uint setLength = set.keys.length;
        for (uint i = 0; i < setLength; i++) {
            require(set.keys[i] != address(0), "Invalid key");
        }
        State.guardianSets[index] = set;
    }

    function cacheGuardianSet(uint32 index) internal returns (bool updated) {
        // we delay loading the guardian set from the wormhole contract until we know we need it,
        // to save gas in case the guardian set is already cached.
        //
        // we observe the following update pattern in the underlying wormhole contract:
        // 1. a guardian set is created, with an expiration time of 0
        // 2. when a new guardian set is created, the previous one is updated with a non-zero expiration time
        //
        // no other updates to the guardian set are made.
        //
        // this means that once we cached the keys of a guardian set, we no
        // longer need to cache them again. But we still need to update the expiration time later.
        // once both the keys and expiration time are set, we consider the guardian set fully cached.

        // check if the guardian set is already cached
        if (guardianSets[index].keys.length > 0) {
            // keys already cached, check if the expiration time is set (which, once set, is never changed)
            if (guardianSets[index].expirationTime > 0) {
                return false; // no update needed, already cached
            } else {
                IWormhole.GuardianSet memory guardianSet = wormhole.getGuardianSet(index);
                if (guardianSet.keys.length == 0) {
                    revert("Guardian set not found");
                }
                if (guardianSet.expirationTime == 0) {
                    return false; // no update needed, still not expired
                }
                guardianSets[index].expirationTime = guardianSet.expirationTime;
                return true; // updated expiration time
            }
        } else {
            IWormhole.GuardianSet memory guardianSet = wormhole.getGuardianSet(index);
            if (guardianSet.keys.length == 0) {
                revert("Guardian set not found");
            }
            // not cached yet, store it
            storeGuardianSet(guardianSet, index);
            return true;
        }
    }

    /// @notice Caches the guardian sets from the wormhole contract.
    /// @dev Returns true iff the guardian set cache was updated.
    /// @dev An off-chain process can simulate this function to determine if a
    /// guardian set is available for caching.
    /// @dev It can be called by anyone.
    function syncGuardianSets() public returns (bool) {
        uint32 currentIndex = wormhole.getCurrentGuardianSetIndex();
        uint32 currentCachedIndex = State.guardianSetIndex;
        bool updated = false;
        for (uint32 i = 0; i <= currentIndex; i++) {
            if (cacheGuardianSet(i)) {
                updated = true;
            }
            if (i > currentCachedIndex) {
                State.guardianSetIndex = i;
                updated = true;
            }

        }
        return updated;
    }
}
