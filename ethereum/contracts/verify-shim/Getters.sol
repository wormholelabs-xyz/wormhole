// contracts/Getters.sol
// SPDX-License-Identifier: Apache 2

pragma solidity ^0.8.0;

import "./State.sol";
import "../interfaces/IWormhole.sol";

abstract contract Getters is State {
    function getGuardianSet(uint32 index) public view returns (IWormhole.GuardianSet memory) {
        return State.guardianSets[index];
    }

    function getCurrentGuardianSetIndex() public view returns (uint32) {
        return State.guardianSetIndex;
    }
}
