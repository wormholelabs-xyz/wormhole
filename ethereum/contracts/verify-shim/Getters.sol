// contracts/Getters.sol
// SPDX-License-Identifier: Apache 2

pragma solidity ^0.8.0;

import "./State.sol";

contract Getters is State {
    function getGuardianSet(uint32 index) public view returns (Structs.GuardianSet memory) {
        return _state.guardianSets[index];
    }

    function getCurrentGuardianSetIndex() public view returns (uint32) {
        return _state.guardianSetIndex;
    }

    function getGuardianSetExpiry() public view returns (uint32) {
        return _state.guardianSetExpiry;
    }

    function chainId() public view returns (uint16) {
        return _state.provider.chainId;
    }

    function governanceChainId() public view returns (uint16){
        return _state.provider.governanceChainId;
    }

    function governanceContract() public view returns (bytes32){
        return _state.provider.governanceContract;
    }
}
