import { arrayify, zeroPad } from "@ethersproject/bytes";
import { PublicKey } from "@solana/web3.js";
import {
  hexValue,
  hexZeroPad,
  keccak256,
  sha256,
  stripZeros,
} from "ethers/lib/utils";
// import { Provider as NearProvider } from "near-api-js/lib/providers";
// import { ethers } from "ethers";
import {
  nativeStringToHexAlgorand,
  // hexToNativeAssetStringAlgorand,
  // nativeStringToHexAlgorand,
  uint8ArrayToNativeStringAlgorand,
} from "./algorand";
import { bech32 } from "bech32";
// import { canonicalAddress, humanAddress } from "./cosmos";
// import { buildTokenId } from "./cosmwasm/address";
// import { isNativeDenom } from "./terra";
// import { hashLookup } from "./near";
// import { getExternalAddressFromType, isValidAptosType } from "./aptos";
// import { isValidSuiAddress } from "@mysten/sui.js";
// import { isValidSuiType } from "./sui";
import {
  Chain,
  ChainId,
  chainToChainId,
  chainToPlatform,
  toChain,
  toChainId,
} from "@wormhole-foundation/sdk-base";
import {
  PlatformToChains,
  UniversalAddress,
  // canonicalAddress,
  encoding,
  platformToChains,
} from "@wormhole-foundation/sdk";
import { isValidSuiAddress } from "@mysten/sui.js";
import { isValidSuiType } from "./chains/sui";
import { sha3_256 } from "js-sha3";

// /**
//  *
//  * Returns true iff the hex string represents a native Terra denom.
//  *
//  * Native assets on terra don't have an associated smart contract address, just
//  * like eth isn't an ERC-20 contract on Ethereum.
//  *
//  * The difference is that the EVM implementations of Portal don't support eth
//  * directly, and instead require swapping to an ERC-20 wrapped eth (WETH)
//  * contract first.
//  *
//  * The Terra implementation instead supports Terra-native denoms without
//  * wrapping to CW-20 token first. As these denoms don't have an address, they
//  * are encoded in the Portal payloads by the setting the first byte to 1.  This
//  * encoding is safe, because the first 12 bytes of the 32-byte wormhole address
//  * space are not used on Terra otherwise, as cosmos addresses are 20 bytes wide.
//  */
export const isHexNativeTerra = (h: string): boolean => h.startsWith("01");

const isLikely20ByteCosmwasm = (h: string): boolean =>
  h.startsWith("000000000000000000000000");

export const nativeTerraHexToDenom = (h: string): string =>
  Buffer.from(stripZeros(hexToUint8Array(h.substr(2)))).toString("ascii");

export const isNativeDenom = (string = "") =>
  isNativeTerra(string) || string === "uluna";

export const isNativeTerra = (string = "") =>
  string.startsWith("u") && string.length === 4;

export const uint8ArrayToHex = (a: Uint8Array): string =>
  encoding.hex.encode(a);

export const hexToUint8Array = (h: string): Uint8Array =>
  encoding.hex.decode(h);

export function canonicalAddress(humanAddress: string) {
  return new Uint8Array(bech32.fromWords(bech32.decode(humanAddress).words));
}

export function humanAddress(hrp: string, canonicalAddress: Uint8Array) {
  return bech32.encode(hrp, bech32.toWords(canonicalAddress));
}

export function buildTokenId(
  chain: Exclude<PlatformToChains<"Cosmwasm">, "Terra">,
  address: string
) {
  return (
    (isNativeCosmWasmDenom(chain, address) ? "01" : "00") +
    keccak256(Buffer.from(address, "utf-8")).substring(4)
  );
}

// export function buildTokenId_old(
//   chain: Exclude<
//     CosmWasmChainId | CosmWasmChainName,
//     typeof CHAIN_ID_TERRA | "terra"
//   >,
//   address: string
// ) {
//   const chainId: CosmWasmChainId = coalesceCosmWasmChainId(chain);
//   return (
//     (isNativeCosmWasmDenom(chainId, address) ? "01" : "00") +
//     keccak256(Buffer.from(address, "utf-8")).substring(4)
//   );
// }

/**
 *
 * Convert an address in a wormhole's 32-byte array representation into a chain's
 * native string representation.
 *
 * @throws if address is not the right length for the given chain
 */

export const tryUint8ArrayToNative = (
  a: Uint8Array,
  chain: ChainId | Chain
): string => {
  const chainName = toChain(chain);
  if (chainToPlatform(chainName) === "Evm") {
    // if (isEVMChain(chainId)) {
    return hexZeroPad(hexValue(a), 20);
  } else if (chainToPlatform(chainName) === "Solana") {
    return new PublicKey(a).toString();
  } else if (chainName === "Terra" || chainName === "Terra2") {
    const h = uint8ArrayToHex(a);
    if (isHexNativeTerra(h)) {
      return nativeTerraHexToDenom(h);
    } else {
      if (chainName === "Terra2" && !isLikely20ByteCosmwasm(h)) {
        // terra 2 has 32 byte addresses for contracts and 20 for wallets
        return humanAddress("terra", a);
      }
      return humanAddress("terra", a.slice(-20));
    }
  } else if (chainName === "Injective") {
    const h = uint8ArrayToHex(a);
    return humanAddress("inj", isLikely20ByteCosmwasm(h) ? a.slice(-20) : a);
  } else if (chainName === "Algorand") {
    return uint8ArrayToNativeStringAlgorand(a);
  } else if (chainName == "Wormchain") {
    const h = uint8ArrayToHex(a);
    return humanAddress(
      "wormhole",
      isLikely20ByteCosmwasm(h) ? a.slice(-20) : a
    );
  } else if (chainName === "Xpla") {
    const h = uint8ArrayToHex(a);
    return humanAddress("xpla", isLikely20ByteCosmwasm(h) ? a.slice(-20) : a);
  } else if (chainName === "Sei") {
    const h = uint8ArrayToHex(a);
    return humanAddress("sei", isLikely20ByteCosmwasm(h) ? a.slice(-20) : a);
  } else if (chainName === "Near") {
    throw Error("uint8ArrayToNative: Use tryHexToNativeStringNear instead.");
  } else if (chainName === "Osmosis") {
    throw Error("uint8ArrayToNative: Osmosis not supported yet.");
  } else if (chainName === "Cosmoshub") {
    throw Error("uint8ArrayToNative: CosmosHub not supported yet.");
  } else if (chainName === "Evmos") {
    throw Error("uint8ArrayToNative: Evmos not supported yet.");
  } else if (chainName === "Kujira") {
    throw Error("uint8ArrayToNative: Kujira not supported yet.");
  } else if (chainName === "Neutron") {
    throw Error("uint8ArrayToNative: Neutron not supported yet.");
  } else if (chainName === "Celestia") {
    throw Error("uint8ArrayToNative: Celestia not supported yet.");
  } else if (chainName === "Stargaze") {
    throw Error("uint8ArrayToNative: Stargaze not supported yet.");
  } else if (chainName === "Seda") {
    throw Error("uint8ArrayToNative: Seda not supported yet.");
  } else if (chainName === "Dymension") {
    throw Error("uint8ArrayToNative: Dymension not supported yet.");
  } else if (chainName === "Provenance") {
    throw Error("uint8ArrayToNative: Provenance not supported yet.");
  } else if (chainName === "Sui") {
    throw Error("uint8ArrayToNative: Sui not supported yet.");
  } else if (chainName === "Aptos") {
    throw Error("uint8ArrayToNative: Aptos not supported yet.");
  } else if (chainName === "Btc") {
    throw Error("uint8ArrayToNative: Btc not supported");
  } else {
    // This case is never reached
    // const _: never = chainName;
    throw Error("Don't know how to convert address for chain " + chainName);
  }
};

// export const tryHexToNativeStringNear = async (
//   provider: NearProvider,
//   tokenBridge: string,
//   address: string
// ): Promise<string> => {
//   const { found, value } = await hashLookup(provider, tokenBridge, address);
//   if (!found) {
//     throw new Error("Address not found");
//   }
//   return value;
// };

/**
 *
 * Convert an address in a wormhole's 32-byte hex representation into a chain's native
 * string representation.
 *
 * @throws if address is not the right length for the given chain
 */
export const tryHexToNativeAssetString = (h: string, c: ChainId): string =>
  c === chainToChainId("Algorand")
    ? // Algorand assets are represented by their asset ids, not an address
      new UniversalAddress(h).toNative("Algorand").toBigInt().toString()
    : new UniversalAddress(h).toNative(toChain(c)).toString();

// /**
//  *
//  * Convert an address in a wormhole's 32-byte hex representation into a chain's native
//  * string representation.
//  *
//  * @deprecated since 0.3.0, use [[tryHexToNativeString]] instead.
//  */
// export const hexToNativeAssetString = (
//   h: string | undefined,
//   c: ChainId
// ): string | undefined => {
//   if (!h) {
//     return undefined;
//   }
//   try {
//     return tryHexToNativeAssetString(h, c);
//   } catch (e) {
//     return undefined;
//   }
// };

// /**
//  *
//  * Convert an address in a wormhole's 32-byte hex representation into a chain's native
//  * string representation.
//  *
//  * @throws if address is not the right length for the given chain
//  */
// export const tryHexToNativeString = (h: string, c: ChainId | Chain): string =>
//   tryUint8ArrayToNative(hexToUint8Array(h), c);

// /**
//  *
//  * Convert an address in a wormhole's 32-byte hex representation into a chain's native
//  * string representation.
//  *
//  * @deprecated since 0.3.0, use [[tryHexToNativeString]] instead.
//  */
// export const hexToNativeString = (
//   h: string | undefined,
//   c: ChainId | Chain
// ): string | undefined => {
//   if (!h) {
//     return undefined;
//   }

//   try {
//     return tryHexToNativeString(h, c);
//   } catch (e) {
//     return undefined;
//   }
// };

/**
 *
 * Convert an address in a chain's native representation into a 32-byte hex string
 * understood by wormhole (UniversalAddress).
 *
 * @throws if address is a malformed string for the given chain id
 */
export const tryNativeToHexString = (
  address: string,
  chain: ChainId | Chain
): string => {
  const chainName = toChain(chain);
  if (chainToPlatform(chainName) === "Evm") {
    return uint8ArrayToHex(zeroPad(arrayify(address), 32));
  } else if (chainToPlatform(chainName) === "Solana") {
    return uint8ArrayToHex(zeroPad(new PublicKey(address).toBytes(), 32));
  } else if (chainName === "Terra") {
    if (isNativeDenom(address)) {
      return (
        "01" +
        uint8ArrayToHex(
          zeroPad(new Uint8Array(Buffer.from(address, "ascii")), 31)
        )
      );
    } else {
      return uint8ArrayToHex(zeroPad(canonicalAddress(address), 32));
    }
  } else if (
    chainName === "Terra2" ||
    chainName === "Injective" ||
    chainName === "Xpla" ||
    chainName === "Sei"
  ) {
    return buildTokenId(chainName, address);
  } else if (chainName === "Algorand") {
    return nativeStringToHexAlgorand(address);
  } else if (chainName == "Wormchain") {
    return uint8ArrayToHex(zeroPad(canonicalAddress(address), 32));
  } else if (chainName === "Near") {
    return uint8ArrayToHex(arrayify(sha256(Buffer.from(address))));
  } else if (chainName === "Sui") {
    if (!isValidSuiType(address) && isValidSuiAddress(address)) {
      return uint8ArrayToHex(
        zeroPad(arrayify(address, { allowMissingPrefix: true }), 32)
      );
    }
    throw Error("nativeToHexString: Sui types not supported yet.");
  } else if (chainName === "Aptos") {
    if (isValidAptosType(address)) {
      return getExternalAddressFromType(address);
    }

    return uint8ArrayToHex(
      zeroPad(arrayify(address, { allowMissingPrefix: true }), 32)
    );
  } else {
    // If this case is reached
    throw Error(`nativeToHexString: ${chainName} not supported yet.`);
  }
};

// /**
//  *
//  * Convert an address in a chain's native representation into a 32-byte hex string
//  * understood by wormhole.
//  *
//  * @deprecated since 0.3.0, use [[tryNativeToHexString]] instead.
//  * @throws if address is a malformed string for the given chain id
//  */
// export const nativeToHexString = (
//   address: string | undefined,
//   chain: ChainId | Chain
// ): string | null => {
//   if (!address) {
//     return null;
//   }
//   return tryNativeToHexString(address, chain);
// };

/**
 *
 * Convert an address in a chain's native representation into a 32-byte array
 * understood by wormhole.
 *
 * @throws if address is a malformed string for the given chain id
 */
export function tryNativeToUint8Array(
  address: string,
  chain: ChainId | Chain
): Uint8Array {
  const chainId = toChainId(chain);
  return hexToUint8Array(tryNativeToHexString(address, chainId));
}

// /**
//  *
//  * Convert an address in a chain's native representation into a 32-byte hex string
//  * understood by wormhole.
//  *
//  * @deprecated since 0.3.0, use [[tryUint8ArrayToNative]] instead.
//  * @throws if address is a malformed string for the given chain id
//  */
// export const uint8ArrayToNative = (a: Uint8Array, chainId: ChainId) =>
//   hexToNativeString(uint8ArrayToHex(a), chainId);

// export function chunks<T>(array: T[], size: number): T[][] {
//   return Array.apply<number, T[], T[][]>(
//     0,
//     new Array(Math.ceil(array.length / size))
//   ).map((_, index) => array.slice(index * size, (index + 1) * size));
// }

// export function textToHexString(name: string): string {
//   return Buffer.from(name, "binary").toString("hex");
// }

// export function textToUint8Array(name: string): Uint8Array {
//   return new Uint8Array(Buffer.from(name, "binary"));
// }

// export function hex(x: string): Buffer {
//   return Buffer.from(
//     ethers.utils.hexlify(x, { allowMissingPrefix: true }).substring(2),
//     "hex"
//   );
// }

// export function ensureHexPrefix(x: string): string {
//   return x.substring(0, 2) !== "0x" ? `0x${x}` : x;
// }

export const isNativeDenomInjective = (denom: string) => denom === "inj";
export const isNativeDenomXpla = (denom: string) => denom === "axpla";
export const isNativeDenomSei = (denom: string) => denom === "usei";
export const isNativeDenomWormchain = (denom: string) => denom === "uworm";
export const isNativeDenomOsmosis = (denom: string) => denom === "uosmo";
export const isNativeDenomCosmosHub = (denom: string) => denom === "uatom";
export const isNativeDenomEvmos = (denom: string) =>
  denom === "aevmos" || denom === "atevmos";
export const isNativeDenomKujira = (denom: string) => denom === "ukuji";
export const isNativeDenomNeutron = (denom: string) => denom === "untrn";
export const isNativeDenomCelestia = (denom: string) => denom === "utia";
export const isNativeDenomStargaze = (denom: string) => denom === "ustars";
export const isNativeDenomSeda = (denom: string) => denom === "aseda";
export const isNativeDenomDymension = (denom: string) => denom === "adym";
export const isNativeDenomProvenance = (denom: string) => denom === "nhash";

export function isNativeCosmWasmDenom(
  chain: PlatformToChains<"Cosmwasm">,
  address: string
) {
  return (
    ((chain === "Terra" || chain === "Terra2") && isNativeDenom(address)) ||
    (chain === "Injective" && isNativeDenomInjective(address)) ||
    (chain === "Xpla" && isNativeDenomXpla(address)) ||
    (chain === "Sei" && isNativeDenomSei(address)) ||
    (chain === "Wormchain" && isNativeDenomWormchain(address)) ||
    (chain === "Osmosis" && isNativeDenomOsmosis(address)) ||
    (chain === "Cosmoshub" && isNativeDenomCosmosHub(address)) ||
    (chain === "Evmos" && isNativeDenomEvmos(address)) ||
    (chain === "Kujira" && isNativeDenomKujira(address)) ||
    (chain === "Neutron" && isNativeDenomNeutron(address)) ||
    (chain === "Celestia" && isNativeDenomCelestia(address)) ||
    (chain === "Stargaze" && isNativeDenomStargaze(address)) ||
    (chain === "Seda" && isNativeDenomSeda(address)) ||
    (chain === "Dymension" && isNativeDenomDymension(address)) ||
    (chain === "Provenance" && isNativeDenomProvenance(address))
  );
}

/**
 * Test if given string is a valid fully qualified type of moduleAddress::moduleName::structName.
 * @param str String to test
 * @returns Whether or not given string is a valid type
 */
export const isValidAptosType = (str: string): boolean =>
  /^(0x)?[0-9a-fA-F]+::\w+::\w+$/.test(str);

/**
 * Hashes the given type. Because fully qualified types are a concept unique to Aptos, this
 * output acts as the address on other chains.
 * @param fullyQualifiedType Fully qualified type on Aptos
 * @returns External address corresponding to given type
 */
export const getExternalAddressFromType = (
  fullyQualifiedType: string
): string => {
  // hash the type so it fits into 32 bytes
  return sha3_256(fullyQualifiedType);
};
