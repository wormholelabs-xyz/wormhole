// contracts/Setters.sol
// SPDX-License-Identifier: Apache 2

pragma solidity ^0.8.0;

import "./State.sol";

abstract contract Setters is State {
    function updateGuardianSetIndex(uint32 newIndex) internal {
        State.guardianSetIndex = newIndex;
    }

    function expireGuardianSet(uint32 index) internal {
        State.guardianSets[index].expirationTime = uint32(block.timestamp) + 86400;
    }

    function storeGuardianSet(IWormhole.GuardianSet memory set, uint32 index) internal {
        uint setLength = set.keys.length;
        for (uint i = 0; i < setLength; i++) {
            require(set.keys[i] != address(0), "Invalid key");
        }
        State.guardianSets[index] = set;
    }

}
