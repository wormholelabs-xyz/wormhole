import {
  getForeignAssetAlgorand,
  getForeignAssetAptos,
  getForeignAssetEth,
  getForeignAssetNear,
  getForeignAssetSolana,
  getForeignAssetTerra,
} from "@certusone/wormhole-sdk/lib/esm/token_bridge/getForeignAsset";
import {
  Terra2Like,
  getTerra2Client,
  isTerra2Like,
  terra2Contracts,
} from "../terra2";
import { getForeignAssetSui } from "../../sdk/sui";
import { getForeignAssetInjective } from "@certusone/wormhole-sdk/lib/esm/token_bridge/injective";
import { ethers } from "ethers";
import { getForeignAssetSei } from "../sei/sdk";
import { getProviderForChain } from "./provider";
import {
  Chain,
  ChainId,
  Network,
  chainToPlatform,
  contracts,
  toChain,
  toChainId,
} from "@wormhole-foundation/sdk-base";
import { toLegacyChainId, tryNativeToUint8Array } from "../../sdk/array";

export const getWrappedAssetAddress = async (
  chain: ChainId | Chain | Terra2Like,
  network: Network,
  originChain: ChainId | Chain | Terra2Like,
  originAddress: string,
  rpc?: string
): Promise<string | null> => {
  const originAddressUint8Array = tryNativeToUint8Array(
    originAddress,
    originChain
  );
  if (isTerra2Like(chain)) {
    const client = getTerra2Client(network, rpc);
    return getForeignAssetTerra(
      terra2Contracts(network).tokenBridge,
      client as any,
      toLegacyChainId(originChain),
      originAddressUint8Array
    );
  }
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
    return getForeignAssetEth(
      tokenBridgeAddress,
      provider,
      toLegacyChainId(originChain),
      originAddressUint8Array
    );
  }

  switch (chainName) {
    case "Solana": {
      const provider = getProviderForChain(chainName, network, { rpc });
      return getForeignAssetSolana(
        provider,
        tokenBridgeAddress,
        toLegacyChainId(originChain),
        originAddressUint8Array
      );
    }
    case "Injective": {
      const provider = getProviderForChain(chainName, network, { rpc });
      // the legacy SDK bundles its own (older) @injectivelabs/sdk-ts; the
      // wasm api client is runtime-compatible
      return getForeignAssetInjective(
        tokenBridgeAddress,
        provider as any,
        toLegacyChainId(originChain),
        originAddressUint8Array
      );
    }
    case "Sei": {
      const provider = await getProviderForChain(chainName, network, { rpc });
      return getForeignAssetSei(
        tokenBridgeAddress,
        provider,
        toChainId(originChain),
        originAddressUint8Array
      );
    }
    case "Algorand": {
      const provider = getProviderForChain(chainName, network, { rpc });
      return getForeignAssetAlgorand(
        provider,
        BigInt(tokenBridgeAddress),
        toLegacyChainId(originChain),
        originAddress
      ).then((x) => x?.toString() ?? null);
    }
    case "Near": {
      const provider = await getProviderForChain(chainName, network, { rpc });
      return getForeignAssetNear(
        provider,
        tokenBridgeAddress,
        toLegacyChainId(originChain),
        originAddress
      );
    }
    case "Aptos": {
      const provider = getProviderForChain(chainName, network, { rpc });
      return getForeignAssetAptos(
        provider,
        tokenBridgeAddress,
        toLegacyChainId(originChain),
        originAddress
      );
    }
    case "Sui": {
      const provider = getProviderForChain(chainName, network, { rpc });
      return getForeignAssetSui(
        provider,
        tokenBridgeAddress,
        toChain(originChain),
        originAddressUint8Array
      );
    }
    default:
      throw new Error(`${chainName} not supported`);
  }
};
