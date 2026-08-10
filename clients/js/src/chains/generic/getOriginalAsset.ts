import {
  WormholeWrappedInfo,
  getOriginalAssetAlgorand,
  getOriginalAssetAptos,
  getOriginalAssetEth,
  getOriginalAssetNear,
  getOriginalAssetSolana,
} from "@certusone/wormhole-sdk/lib/esm/token_bridge/getOriginalAsset";
import { getOriginalAssetSui } from "../../sdk/sui";
import { getOriginalAssetInjective } from "@certusone/wormhole-sdk/lib/esm/token_bridge/injective";
import { ethers } from "ethers";
import { getOriginalAssetSei } from "../sei/sdk";
import { getProviderForChain } from "./provider";
import {
  Chain,
  ChainId,
  Network,
  chainToPlatform,
  contracts,
  toChain,
} from "@wormhole-foundation/sdk-base";
import { toLegacyChainId } from "../../sdk/array";

export const getOriginalAsset = async (
  chain: ChainId | Chain,
  network: Network,
  assetAddress: string,
  rpc?: string
): Promise<WormholeWrappedInfo> => {
  const chainName = toChain(chain);
  const tokenBridgeAddress = contracts.tokenBridge.get(network, chainName);
  if (!tokenBridgeAddress) {
    throw new Error(
      `Token bridge address not defined for ${chainName} ${network}`
    );
  }

  if (chainToPlatform(chainName) === "Evm") {
    const provider = getProviderForChain(chainName, network, {
      rpc,
    }) as ethers.providers.JsonRpcProvider;
    return getOriginalAssetEth(
      tokenBridgeAddress,
      provider,
      assetAddress,
      toLegacyChainId(chain)
    );
  }

  switch (chainName) {
    case "Solana": {
      const provider = getProviderForChain(chainName, network, { rpc });
      return getOriginalAssetSolana(provider, tokenBridgeAddress, assetAddress);
    }
    case "Injective": {
      const provider = getProviderForChain(chainName, network, { rpc });
      // the legacy SDK bundles its own (older) @injectivelabs/sdk-ts; the
      // wasm api client is runtime-compatible
      return getOriginalAssetInjective(assetAddress, provider as any);
    }
    case "Sei": {
      const provider = await getProviderForChain(chainName, network, { rpc });
      return getOriginalAssetSei(assetAddress, provider);
    }
    case "Algorand": {
      const provider = getProviderForChain(chainName, network, { rpc });
      return getOriginalAssetAlgorand(
        provider,
        BigInt(tokenBridgeAddress),
        BigInt(assetAddress)
      );
    }
    case "Near": {
      const provider = await getProviderForChain(chainName, network, { rpc });
      return getOriginalAssetNear(provider, tokenBridgeAddress, assetAddress);
    }
    case "Aptos": {
      const provider = getProviderForChain(chainName, network, { rpc });
      return getOriginalAssetAptos(provider, tokenBridgeAddress, assetAddress);
    }
    case "Sui": {
      const provider = getProviderForChain(chainName, network, { rpc });
      return (await getOriginalAssetSui(
        provider,
        tokenBridgeAddress,
        assetAddress
      )) as WormholeWrappedInfo;
    }
    default:
      throw new Error(`${chainName} not supported`);
  }
};
