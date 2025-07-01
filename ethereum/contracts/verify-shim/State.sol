// contracts/State.sol
// SPDX-License-Identifier: Apache 2

pragma solidity ^0.8.0;

import "./Structs.sol";

contract Storage {
    struct WormholeState {

        Structs.Provider provider;

        // Mapping of guardian_set_index => guardian set
        mapping(uint32 => Structs.GuardianSet) guardianSets;

        // Current active guardian set index
        uint32 guardianSetIndex;

        // Period for which a guardian set stays active after it has been replaced
        uint32 guardianSetExpiry;
    }
}

contract State {
    Storage.WormholeState _state;
}
