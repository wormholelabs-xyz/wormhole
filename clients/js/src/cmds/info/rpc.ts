import yargs from "yargs";
import { NETWORKS } from "../../consts";
import { chainToChain, getNetwork } from "../../utils";
import { TERRA2, TERRA2_CONNECTIONS } from "../../chains/terra2";

export const command = "rpc <network> <chain>";
export const desc = "Print RPC address";
export const builder = (y: typeof yargs) =>
  y
    .positional("network", {
      describe: "network",
      choices: ["mainnet", "testnet", "devnet"],
      demandOption: true,
    } as const)
    .positional("chain", {
      describe:
        "Chain to query. To see a list of supported chains, run `worm chains`",
      type: "string",
      demandOption: true,
    } as const);
export const handler = async (
  argv: Awaited<ReturnType<typeof builder>["argv"]>
) => {
  const network = getNetwork(argv.network);
  const chain = chainToChain(argv.chain);
  console.log(
    chain === TERRA2
      ? TERRA2_CONNECTIONS[network].rpc
      : NETWORKS[network][chain].rpc
  );
};
