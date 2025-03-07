import * as web3s from "@solana/web3.js";
import fs from "fs";
import sha3 from "js-sha3";
import yargs from "yargs";
import {
  GOVERNANCE_CHAIN,
  GOVERNANCE_EMITTER,
  NETWORKS,
  NETWORK_OPTIONS,
  RPC_OPTIONS,
} from "../consts";
import { evm_address, getNetwork } from "../utils";
import { chainToChainId } from "@wormhole-foundation/sdk-base";
import { createInitializeInstruction } from "@certusone/wormhole-sdk/lib/esm/solana/wormhole";
import base58 from "bs58";

export const command = "solana";
export const desc = "Solana utilities";
export const builder = (y: typeof yargs) =>
  y
    .command(
      "private-key-base58",
      "Convert a private key to base58",
      (yargs) =>
        yargs.option("keypair", {
          describe: "path to the keypair file",
          type: "string",
          demandOption: true,
        }),
      (argv) => {
        const privateKey = JSON.parse(fs.readFileSync(argv.keypair, "utf-8"));
        console.log(base58.encode(privateKey));
      })
    .command(
      "init-wormhole",
      "Init Wormhole core contract",
      (yargs) =>
        yargs
          .option("network", NETWORK_OPTIONS)
          .option("rpc", RPC_OPTIONS)
          .option("guardian-set-expiration-time", {
            describe: "Guardian set expiration time",
            type: "number",
            default: 86400,
            demandOption: false,
          })
          .option("fee", {
            describe: "Fee",
            type: "number",
            default: 0,
            demandOption: false,
          })
          .option("guardian-address", {
            alias: "g",
            demandOption: true,
            describe: "Initial guardian's addresses (CSV)",
            type: "string",
          })
          .option("contract-address", {
            describe: "Core contract address",
            type: "string",
            demandOption: true,
          }),
      async (argv) => {
        const network = getNetwork(argv.network);

        const contract_address =
          argv["contract-address"];
        const guardian_addresses = argv["guardian-address"]
          .split(",")
          .map((address) => evm_address(address).substring(24));
        const guardian_set_expiration_time = argv["guardian-set-expiration-time"];

        const chain = "Solana";
        const { rpc, key } = NETWORKS[network][chain];
        if (!key) {
          throw Error(`No ${network} key defined for ${chain}`);
        }

        if (!rpc) {
          throw Error(`No ${network} rpc defined for ${chain}`);
        }

        const from = web3s.Keypair.fromSecretKey(base58.decode(key));
        const connection = setupConnection(rpc);

        const ix = createInitializeInstruction(
          contract_address,
          from.publicKey,
          guardian_set_expiration_time,
          BigInt(argv["fee"]),
          guardian_addresses.map((x) => Buffer.from(x, "hex"))
        );

        const transaction = new web3s.Transaction().add(ix);

        const signature = await web3s.sendAndConfirmTransaction(
          connection,
          transaction,
          [from],
          {
            skipPreflight: true,
          }
        );
        console.log(`Transaction confirmed: ${signature}`);
      }
    )
    .strict()
    .demandCommand();
export const handler = () => {};

const setupConnection = (rpc: string): web3s.Connection =>
  new web3s.Connection(rpc, "confirmed");
