import yargs from "yargs";
import { impossible } from "../../vaa";
import { contracts } from "@wormhole-foundation/sdk-base";
import { chainToChain, getNetwork } from "../../utils";
import { TERRA2, terra2Contracts } from "../../chains/terra2";

export const command = "contract <network> <chain> <module>";
export const desc = "Print contract address";
export const builder = (y: typeof yargs) =>
  y
    .positional("network", {
      describe: "Network",
      choices: ["mainnet", "testnet", "devnet"],
      demandOption: true,
    } as const)
    .positional("chain", {
      describe:
        "Chain to query. To see a list of supported chains, run `worm chains`",
      type: "string",
      demandOption: true,
    } as const)
    .positional("module", {
      describe: "Module to query",
      choices: ["Core", "NFTBridge", "TokenBridge", "WormholeRelayer"],
      demandOption: true,
    } as const);
export const handler = async (
  argv: Awaited<ReturnType<typeof builder>["argv"]>
) => {
  const network = getNetwork(argv.network);
  const chain = chainToChain(argv.chain);
  const module = argv["module"];

  let addr: string | undefined;
  if (chain === TERRA2) {
    // Terra2 compat: the SDK no longer carries these addresses
    const t2 = terra2Contracts(network);
    addr = module === "Core" ? t2.core : module === "TokenBridge" ? t2.tokenBridge : undefined;
    if (!addr) {
      throw new Error(`${module} not deployed on ${chain}`);
    }
    console.log(addr);
    return;
  }
  switch (module) {
    case "Core":
      addr = contracts.coreBridge.get(network, chain);
      break;
    case "NFTBridge":
      addr = contracts.nftBridge.get(network, chain);
      if (!addr) {
        throw new Error(`NFTBridge not deployed on ${chain}`);
      }

      break;
    case "TokenBridge":
      addr = contracts.tokenBridge.get(network, chain);
      break;
    case "WormholeRelayer":
      addr = contracts.relayer.get(network, chain);
      break;
    default:
      impossible(module);
  }

  if (!addr) {
    throw new Error(`${module} not deployed on ${chain}`);
  }

  console.log(addr);
};
