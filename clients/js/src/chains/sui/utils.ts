import {
  Connection,
  Ed25519Keypair,
  JsonRpcProvider,
  PaginatedObjectsResponse,
  RawSigner,
  SUI_CLOCK_OBJECT_ID,
  SuiObjectResponse,
  SuiTransactionBlockResponse,
  TransactionBlock,
  fromB64,
  getPublishedObjectChanges,
  isValidSuiAddress as isValidFullSuiAddress,
  normalizeSuiAddress,
} from "@mysten/sui.js";
import { DynamicFieldPage } from "@mysten/sui.js/dist/types/dynamic_fields";
import { NETWORKS } from "../../consts";
import { Payload, VAA, parse, serialiseVAA } from "../../vaa";
import { SuiRpcValidationError } from "./error";
import { Chain, Network, chainToChainId } from "@wormhole-foundation/sdk";

const UPGRADE_CAP_TYPE = "0x2::package::UpgradeCap";

export const assertSuccess = (
  res: SuiTransactionBlockResponse,
  error: string
): void => {
  if (res?.effects?.status?.status !== "success") {
    throw new Error(`${error} Response: ${JSON.stringify(res)}`);
  }
};

export const executeTransactionBlock = async (
  signer: RawSigner,
  transactionBlock: TransactionBlock
): Promise<SuiTransactionBlockResponse> => {
  // As of version 0.32.2, Sui SDK outputs a RPC validation warning when the
  // SDK falls behind the Sui version used by the RPC. We silence these
  // warnings since the SDK is often out of sync with the RPC.
  const consoleWarnTemp = console.warn;
  console.warn = () => {};

  // Let caller handle parsing and logging info
  const res = await signer.signAndExecuteTransactionBlock({
    transactionBlock,
    options: {
      showInput: true,
      showEffects: true,
      showEvents: true,
      showObjectChanges: true,
    },
  });

  console.warn = consoleWarnTemp;
  return res;
};

export const findOwnedObjectByType = async (
  provider: JsonRpcProvider,
  owner: string,
  type: string,
  cursor?: string
): Promise<string | null> => {
  const res: PaginatedObjectsResponse = await provider.getOwnedObjects({
    owner,
    filter: undefined, // Filter must be undefined to avoid 504 responses
    cursor: cursor || undefined,
    options: {
      showType: true,
    },
  });

  if (!res || !res.data) {
    throw new SuiRpcValidationError(res);
  }

  const object = res.data.find((d) => d.data?.type === type);

  if (!object && res.hasNextPage) {
    return findOwnedObjectByType(
      provider,
      owner,
      type,
      res.nextCursor as string
    );
  } else if (!object && !res.hasNextPage) {
    return null;
  } else {
    return object?.data?.objectId ?? null;
  }
};

export const getCreatedObjects = (
  res: SuiTransactionBlockResponse
): { type: string; objectId: string; owner: string }[] =>
  res.objectChanges?.filter(isSuiCreateEvent).map((e) => {
    let owner: string;
    if (typeof e.owner === "string") {
      owner = e.owner;
    } else if ("AddressOwner" in e.owner) {
      owner = e.owner.AddressOwner;
    } else if ("ObjectOwner" in e.owner) {
      owner = e.owner.ObjectOwner;
    } else {
      owner = "Shared";
    }

    return {
      owner,
      type: e.objectType,
      objectId: e.objectId,
    };
  }) ?? [];

export const getOwnedObjectId = async (
  provider: JsonRpcProvider,
  owner: string,
  packageId: string,
  moduleName: string,
  structName: string
): Promise<string | null> => {
  const type = `${packageId}::${moduleName}::${structName}`;

  // Upgrade caps are a special case
  if (normalizeSuiType(type) === normalizeSuiType(UPGRADE_CAP_TYPE)) {
    throw new Error(
      "`getOwnedObjectId` should not be used to get the object ID of an `UpgradeCap`. Use `getUpgradeCapObjectId` instead."
    );
  }

  try {
    const res = await provider.getOwnedObjects({
      owner,
      filter: { StructType: type },
      options: {
        showContent: true,
      },
    });
    if (!res || !res.data) {
      throw new SuiRpcValidationError(res);
    }

    const objects = res.data.filter((o) => o.data?.objectId);
    if (objects.length === 1) {
      return objects[0].data?.objectId ?? null;
    } else if (objects.length > 1) {
      const objectsStr = JSON.stringify(objects, null, 2);
      throw new Error(
        `Found multiple objects owned by ${owner} of type ${type}. This may mean that we've received an unexpected response from the Sui RPC and \`worm\` logic needs to be updated to handle this. Objects: ${objectsStr}`
      );
    } else {
      return null;
    }
  } catch (error) {
    // Handle 504 error by using findOwnedObjectByType method
    const is504HttpError = `${error}`.includes("504 Gateway Time-out");
    if (error && is504HttpError) {
      return findOwnedObjectByType(provider, owner, type);
    } else {
      throw error;
    }
  }
};

// TODO(kp): remove this once it's in the sdk
export const getPackageId = async (
  provider: JsonRpcProvider,
  objectId: string
): Promise<string> => {
  let currentPackage;
  let nextCursor;
  do {
    const dynamicFields: DynamicFieldPage = await provider.getDynamicFields({
      parentId: objectId,
      cursor: nextCursor,
    });
    currentPackage = dynamicFields.data.find(
      (field: DynamicFieldPage["data"][number]) =>
        field.name.type.endsWith("CurrentPackage")
    );
    nextCursor = dynamicFields.hasNextPage ? dynamicFields.nextCursor : null;
  } while (nextCursor && !currentPackage);
  if (!currentPackage) {
    throw new Error("CurrentPackage not found");
  }

  const obj = await provider.getObject({
    id: currentPackage.objectId,
    options: {
      showContent: true,
    },
  });
  const packageId =
    obj.data?.content && "fields" in obj.data.content
      ? obj.data.content.fields.value?.fields?.package
      : null;
  if (!packageId) {
    throw new Error("Unable to get current package");
  }

  return packageId;
};

export const getProvider = (
  network?: Network,
  rpc?: string
): JsonRpcProvider => {
  if (!network && !rpc) {
    throw new Error("Must provide network or RPC to initialize provider");
  }

  rpc = rpc || NETWORKS[network!].Sui.rpc;
  if (!rpc) {
    throw new Error(`No default RPC found for Sui ${network}`);
  }

  return new JsonRpcProvider(new Connection({ fullnode: rpc }));
};

export const getPublishedPackageId = (
  res: SuiTransactionBlockResponse
): string => {
  const publishEvents = getPublishedObjectChanges(res);
  if (publishEvents.length !== 1) {
    throw new Error(
      "Unexpected number of publish events found:" +
        JSON.stringify(publishEvents, null, 2)
    );
  }

  return publishEvents[0].packageId;
};

export const getSigner = (
  provider: JsonRpcProvider,
  network: Network,
  customPrivateKey?: string
): RawSigner => {
  const privateKey: string | undefined =
    customPrivateKey || NETWORKS[network].Sui.key;
  if (!privateKey) {
    throw new Error(`No private key found for Sui ${network}`);
  }

  let bytes = privateKey.startsWith("0x")
    ? Buffer.from(privateKey.slice(2), "hex")
    : fromB64(privateKey);
  if (bytes.length === 33) {
    // remove the first flag byte after checking it is indeed the Ed25519 scheme flag 0x00
    if (bytes[0] !== 0) {
      throw new Error("Only the Ed25519 scheme flag is supported");
    }
    bytes = bytes.subarray(1);
  }
  const keypair = Ed25519Keypair.fromSecretKey(bytes);
  return new RawSigner(keypair, provider);
};

/**
 * This function returns the object ID of the `UpgradeCap` that belongs to the
 * given package and owner if it exists.
 *
 * Structs created by the Sui framework such as `UpgradeCap`s all have the same
 * type (e.g. `0x2::package::UpgradeCap`) and have a special field, `package`,
 * we can use to differentiate them.
 * @param provider Sui RPC provider
 * @param owner Address of the current owner of the `UpgradeCap`
 * @param packageId ID of the package that the `UpgradeCap` was created for
 * @returns The object ID of the `UpgradeCap` if it exists, otherwise `null`
 */
export const getUpgradeCapObjectId = async (
  provider: JsonRpcProvider,
  owner: string,
  packageId: string
): Promise<string | null> => {
  const res = await provider.getOwnedObjects({
    owner,
    filter: { StructType: UPGRADE_CAP_TYPE },
    options: {
      showContent: true,
    },
  });
  if (!res || !res.data) {
    throw new SuiRpcValidationError(res);
  }

  const objects = res.data.filter(
    (o) =>
      o.data?.objectId &&
      o.data?.content?.dataType === "moveObject" &&
      o.data?.content?.fields?.package === packageId
  );
  if (objects.length === 1) {
    // We've found the object we're looking for
    return objects[0].data?.objectId ?? null;
  } else if (objects.length > 1) {
    const objectsStr = JSON.stringify(objects, null, 2);
    throw new Error(
      `Found multiple upgrade capabilities owned by ${owner} from package ${packageId}. Objects: ${objectsStr}`
    );
  } else {
    return null;
  }
};

export const isSameType = (a: string, b: string) => {
  try {
    return normalizeSuiType(a) === normalizeSuiType(b);
  } catch (e) {
    return false;
  }
};

export const isSuiCreateEvent = <
  T extends NonNullable<SuiTransactionBlockResponse["objectChanges"]>[number],
  K extends Extract<T, { type: "created" }>
>(
  event: T
): event is K => event?.type === "created";

export const isSuiPublishEvent = <
  T extends NonNullable<SuiTransactionBlockResponse["objectChanges"]>[number],
  K extends Extract<T, { type: "published" }>
>(
  event: T
): event is K => event?.type === "published";

// todo(aki): this needs to correctly handle types such as
// 0x2::dynamic_field::Field<0x3c6d386861470e6f9cb35f3c91f69e6c1f1737bd5d217ca06a15f582e1dc1ce3::state::MigrationControl, bool>
export const normalizeSuiType = (type: string): string => {
  const tokens = type.split("::");
  if (tokens.length !== 3 || !isValidSuiAddress(tokens[0])) {
    throw new Error(`Invalid Sui type: ${type}`);
  }

  return [normalizeSuiAddress(tokens[0]), tokens[1], tokens[2]].join("::");
};

export const registerChain = async (
  provider: JsonRpcProvider,
  network: Network,
  vaa: Buffer,
  coreBridgeStateObjectId: string,
  tokenBridgeStateObjectId: string,
  transactionBlock?: TransactionBlock
): Promise<TransactionBlock> => {
  if (network === "Devnet") {
    // Modify the VAA to only have 1 guardian signature
    // TODO: remove this when we can deploy the devnet core contract
    // deterministically with multiple guardians in the initial guardian set
    // Currently the core contract is setup with only 1 guardian in the set
    const parsedVaa = parse(vaa);
    parsedVaa.signatures = [parsedVaa.signatures[0]];
    vaa = Buffer.from(serialiseVAA(parsedVaa as VAA<Payload>), "hex");
  }

  // Get package IDs
  const coreBridgePackageId = await getPackageId(
    provider,
    coreBridgeStateObjectId
  );
  const tokenBridgePackageId = await getPackageId(
    provider,
    tokenBridgeStateObjectId
  );

  // Register chain
  let tx = transactionBlock;
  if (!tx) {
    tx = new TransactionBlock();
    tx.setGasBudget(1000000);
  }

  // Get VAA
  const [verifiedVaa] = tx.moveCall({
    target: `${coreBridgePackageId}::vaa::parse_and_verify`,
    arguments: [
      tx.object(coreBridgeStateObjectId),
      tx.pure([...vaa]),
      tx.object(SUI_CLOCK_OBJECT_ID),
    ],
  });

  // Get decree ticket
  const [decreeTicket] = tx.moveCall({
    target: `${tokenBridgePackageId}::register_chain::authorize_governance`,
    arguments: [tx.object(tokenBridgeStateObjectId)],
  });

  // Get decree receipt
  const [decreeReceipt] = tx.moveCall({
    target: `${coreBridgePackageId}::governance_message::verify_vaa`,
    arguments: [tx.object(coreBridgeStateObjectId), verifiedVaa, decreeTicket],
    typeArguments: [
      `${tokenBridgePackageId}::register_chain::GovernanceWitness`,
    ],
  });

  // Register chain
  tx.moveCall({
    target: `${tokenBridgePackageId}::register_chain::register_chain`,
    arguments: [tx.object(tokenBridgeStateObjectId), decreeReceipt],
  });

  return tx;
};

/**
 * Currently, (Sui SDK version 0.32.2 and Sui 1.0.0 testnet), there is a
 * mismatch in the max gas budget that causes an error when executing a
 * transaction. Because these values are hardcoded, we set the max gas budget
 * as a temporary workaround.
 * @param network
 * @param tx
 */
export const setMaxGasBudgetDevnet = (
  network: Network,
  tx: TransactionBlock
) => {
  if (network === "Devnet") {
    // Avoid Error checking transaction input objects: GasBudgetTooHigh { gas_budget: 50000000000, max_budget: 10000000000 }
    tx.setGasBudget(10000000000);
  }
};

export async function getForeignAssetSui(
  provider: JsonRpcProvider,
  tokenBridgeStateObjectId: string,
  originChain: Chain,
  originAddress: Uint8Array
): Promise<string | null> {
  const originChainId = chainToChainId(originChain);
  return getTokenCoinType(
    provider,
    tokenBridgeStateObjectId,
    originAddress,
    originChainId
  );
}

export const getTokenCoinType = async (
  provider: JsonRpcProvider,
  tokenBridgeStateObjectId: string,
  tokenAddress: Uint8Array,
  tokenChain: number
): Promise<string | null> => {
  const tokenBridgeStateFields = await getObjectFields(
    provider,
    tokenBridgeStateObjectId
  );
  if (!tokenBridgeStateFields) {
    throw new Error("Unable to fetch object fields from token bridge state");
  }

  const coinTypes = tokenBridgeStateFields?.token_registry?.fields?.coin_types;
  const coinTypesObjectId = coinTypes?.fields?.id?.id;
  if (!coinTypesObjectId) {
    throw new Error("Unable to fetch coin types");
  }

  const keyType = getTableKeyType(coinTypes?.type);
  if (!keyType) {
    throw new Error("Unable to get key type");
  }

  const response = await provider.getDynamicFieldObject({
    parentId: coinTypesObjectId,
    name: {
      type: keyType,
      value: {
        addr: [...tokenAddress],
        chain: tokenChain,
      },
    },
  });
  if (response.error) {
    if (response.error.code === "dynamicFieldNotFound") {
      return null;
    }
    throw new Error(
      `Unexpected getDynamicFieldObject response ${response.error}`
    );
  }
  const fields = getFieldsFromObjectResponse(response);
  return fields?.value ? trimSuiType(ensureHexPrefix(fields.value)) : null;
};

export const getObjectFields = async (
  provider: JsonRpcProvider,
  objectId: string
): Promise<Record<string, any> | null> => {
  if (!isValidSuiAddress(objectId)) {
    throw new Error(`Invalid object ID: ${objectId}`);
  }

  const res = await provider.getObject({
    id: objectId,
    options: {
      showContent: true,
    },
  });
  return getFieldsFromObjectResponse(res);
};

export const getFieldsFromObjectResponse = (object: SuiObjectResponse) => {
  const content = object.data?.content;
  return content && content.dataType === "moveObject" ? content.fields : null;
};

export function ensureHexPrefix(x: string): string {
  return x.substring(0, 2) !== "0x" ? `0x${x}` : x;
}

/**
 * This method validates any Sui address, even if it's not 32 bytes long, i.e.
 * "0x2". This differs from Mysten's implementation, which requires that the
 * given address is 32 bytes long.
 * @param address Address to check
 * @returns If given address is a valid Sui address or not
 */
export const isValidSuiAddress = (address: string): boolean =>
  isValidFullSuiAddress(normalizeSuiAddress(address));

export const getTableKeyType = (tableType: string): string | null => {
  if (!tableType) return null;
  const match = trimSuiType(tableType).match(/0x2::table::Table<(.*)>/);
  if (!match) return null;
  const [keyType] = match[1].split(",");
  if (!isValidSuiType(keyType)) return null;
  return keyType;
};

/**
 * This method removes leading zeroes for types in order to normalize them
 * since some types returned from the RPC have leading zeroes and others don't.
 */
export const trimSuiType = (type: string): string =>
  type.replace(/(0x)(0*)/g, "0x");

export const isValidSuiType = (type: string): boolean => {
  const tokens = type.split("::");
  if (tokens.length !== 3) {
    return false;
  }

  return isValidSuiAddress(tokens[0]) && !!tokens[1] && !!tokens[2];
};
