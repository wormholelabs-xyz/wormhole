// contracts/Implementation.sol
// SPDX-License-Identifier: Apache 2

pragma solidity ^0.8.0;
pragma experimental ABIEncoderV2;

import "./State.sol";
import "./Messages.sol";
import "./Setters.sol";

contract Implementation is Messages, Setters {
    constructor(
        IWormhole _wormhole
    ) State(_wormhole) {}

    fallback() external payable {revert("unsupported");}

    receive() external payable {revert("the Wormhole contract does not accept assets");}
}
