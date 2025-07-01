// contracts/State.sol
// SPDX-License-Identifier: Apache 2

pragma solidity ^0.8.0;

import "../interfaces/IWormhole.sol";

abstract contract State {
    constructor(
        IWormhole _wormhole
    ) {
        wormhole = _wormhole;
        chainId = _wormhole.chainId();
    }

    IWormhole immutable wormhole;

    uint16 immutable chainId;

    // Mapping of guardian_set_index => guardian set
    mapping(uint32 => IWormhole.GuardianSet) guardianSets;

    // Current active guardian set index
    uint32 guardianSetIndex;
}
