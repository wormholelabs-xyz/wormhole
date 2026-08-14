import yargs from "yargs";
import { chains } from "@wormhole-foundation/sdk";
import { TERRA2 } from "../chains/terra2";

export const command = "chains";
export const desc = "Print the list of supported chains";
export const builder = (y: typeof yargs) => {
  // No positional parameters needed
  return y;
};
export const handler = () => {
  // Terra2 is gone from the SDK but kept alive by the CLI's compat layer
  console.log([...chains, TERRA2]);
};
