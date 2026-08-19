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
  chains,
  contracts,
  toChain,
} from "@wormhole-foundation/sdk-base";
import { spawnSync } from "child_process";
import { ethers } from "ethers";
import {
  TERRA2,
  TERRA2_CHAIN_ID,
  TERRA2_CONNECTIONS,
  Terra2,
  Terra2Like,
  isTerra2Like,
  terra2Contracts,
} from "./chains/terra2/consts";
import { NETWORKS } from "./consts";

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

/** Normalize a chain name or id — including Terra2's — to a CliChain name. */
export function toCliChain(chain: ChainId | Chain | Terra2Like): CliChain {
  return isTerra2Like(chain) ? TERRA2 : toChain(chain);
}

export function cliChainToPlatform(
  chain: ChainId | CliChain | Terra2Like
): Platform {
  return isTerra2Like(chain) ? "Cosmwasm" : chainToPlatform(toChain(chain));
}

/**
 * Every chain the CLI supports: the SDK's list plus Terra2, which the SDK
 * removed but the CLI keeps alive (see ./chains/terra2).
 */
export const CLI_CHAINS: CliChain[] = [...chains, TERRA2];

// Per-chain config lookups that hide the Terra2 split: the SDK no longer
// carries Terra2's rpc/contracts, so these fall back to the compat layer.

export function getChainRpc(
  network: Network,
  chain: CliChain
): string | undefined {
  return chain === TERRA2
    ? TERRA2_CONNECTIONS[network].rpc
    : NETWORKS[network][chain].rpc;
}

export function getCoreContract(
  network: Network,
  chain: CliChain
): string | undefined {
  return chain === TERRA2
    ? terra2Contracts(network).core
    : contracts.coreBridge.get(network, chain);
}

export function getTokenBridgeContract(
  network: Network,
  chain: CliChain
): string | undefined {
  return chain === TERRA2
    ? terra2Contracts(network).tokenBridge
    : contracts.tokenBridge.get(network, chain);
}

export function getNftBridgeContract(
  network: Network,
  chain: CliChain
): string | undefined {
  // Terra2 never had an NFT bridge deployment
  return chain === TERRA2 ? undefined : contracts.nftBridge.get(network, chain);
}

export function getRelayerContract(
  network: Network,
  chain: CliChain
): string | undefined {
  return chain === TERRA2 ? undefined : contracts.relayer.get(network, chain);
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

export function chainToCliChain(input: string): CliChain {
  const match = CLI_CHAINS.find(
    (chain) => chain.toLowerCase() === input.toLowerCase()
  );
  if (!match) {
    throw new Error(`Invalid chain: ${input}`);
  }
  return match;
}
