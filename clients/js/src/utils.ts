import {
  Chain,
  ChainId,
  Network,
  Platform,
  PlatformToChains,
  assertChainId,
  chainIdToChain,
  chainToChainId,
  chainToPlatform,
  toChain,
} from "@wormhole-foundation/sdk-base";
import { spawnSync } from "child_process";
import { ethers } from "ethers";
import {
  TERRA2,
  TERRA2_CHAIN_ID,
  Terra2,
  Terra2Like,
  isTerra2Like,
} from "./chains/terra2/consts";

/**
 * A chain the CLI supports: everything the SDK knows, plus the Terra2
 * compatibility layer (the SDK removed Terra2, the CLI keeps supporting it).
 */
export type CliChain = Chain | Terra2;

export const checkBinary = (binaryName: string, readmeUrl?: string): void => {
  const binary = spawnSync(binaryName, ["--version"]);
  if (binary.status !== 0) {
    console.error(
      `${binaryName} is not installed. Please install ${binaryName} and try again.`
    );
    if (readmeUrl) {
      console.error(`See ${readmeUrl} for instructions.`);
    }
    process.exit(1);
  }
};

export const evm_address = (x: string): string => {
  return hex(x).substring(2).padStart(64, "0");
};

export const hex = (x: string): string => {
  return ethers.utils.hexlify(x, { allowMissingPrefix: true });
};

export function assertEVMChain(
  chain: ChainId | CliChain | Terra2Like
): asserts chain is PlatformToChains<"Evm"> {
  if (cliChainToPlatform(chain) !== "Evm") {
    throw Error(`Expected an EVM chain, but ${chain} is not`);
  }
}

export function cliChainToChainId(chain: CliChain): number {
  return chain === TERRA2 ? TERRA2_CHAIN_ID : chainToChainId(chain);
}

export function cliChainIdToChain(chainId: number): CliChain {
  if (isTerra2Like(chainId)) {
    return TERRA2;
  }
  assertChainId(chainId);
  return chainIdToChain(chainId);
}

export function cliChainToPlatform(
  chain: ChainId | CliChain | Terra2Like
): Platform {
  return isTerra2Like(chain) ? "Cosmwasm" : chainToPlatform(toChain(chain));
}

export function getNetwork(network: string): Network {
  const lcNetwork: string = network.toLowerCase();
  if (lcNetwork === "mainnet") {
    return "Mainnet";
  }
  if (lcNetwork === "testnet") {
    return "Testnet";
  }
  if (lcNetwork === "devnet") {
    return "Devnet";
  }
  throw new Error(`Unknown network: ${network}`);
}

export function chainToChain(input: string): CliChain {
  if (input.length < 2) {
    throw new Error(`Invalid chain: ${input}`);
  }
  const chainStr = input[0].toUpperCase() + input.slice(1).toLowerCase();
  if (chainStr === TERRA2) {
    return TERRA2;
  }
  return toChain(chainStr);
}
