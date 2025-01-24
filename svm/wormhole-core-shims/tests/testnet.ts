import { getSignedVAAWithRetry } from "@certusone/wormhole-sdk/lib/cjs/rpc";
import { parseVaa } from "@certusone/wormhole-sdk/lib/cjs/vaa/wormhole";
import {
  AnchorProvider,
  Program,
  setProvider,
  Wallet,
  web3,
} from "@coral-xyz/anchor";
import { NodeHttpTransport } from "@improbable-eng/grpc-web-node-http-transport";
import { getSequenceFromTx } from "./helpers";
import { WormholePostMessageShim } from "./idls/testnet/wormhole_post_message_shim";
import WormholePostMessageShimIdl from "./idls/testnet/wormhole_post_message_shim.json";
import { WormholeVerifyVaaShim } from "./idls/testnet/wormhole_verify_vaa_shim";
import WormholeVerifyVaaShimIdl from "./idls/testnet/wormhole_verify_vaa_shim.json";
import { bs58 } from "@coral-xyz/anchor/dist/cjs/utils/bytes";
import { getSequenceTracker } from "@certusone/wormhole-sdk/lib/cjs/solana/wormhole";
import { keccak256 } from "@certusone/wormhole-sdk/lib/cjs/utils/keccak";

// https://wormholescan.io/#/tx/0x1b22a99bc3af19f2fc762a9901b991f2fd0c35a3b0480a7cd2a89055100adff5?network=Testnet
const VAA =
  "AQAAAAABAFb8aX3pIM2rbZViiNOTZCSmOYwXMc3e8xDpNOdmamTGXXpS/cUZrEMWlPzmTiSieve+VWZ0F2oMhSeGi/RqGb8BZ5PDRgAAAAAnFAAAAAAAAAAAAAAAAJO61T3fthMrCsjjf2ApFj5jNyzuAAAAAAAAXrjIAScVAAAAAAAAAAAAAAAAyiEOhYdM3kqU5doNCndVx6gGCQMAAACCAAAAAAAAAAAAAAAAGOvKrDe1QDQAUdXIq480J69nj6EAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAZAAAAAAAAAAAAAAAAIrMoswP1is1/oZ8jsbn9mc7XBGBAAAAAAAAAAAAAAAAGOvKrDe1QDQAUdXIq480J69nj6EnFQAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAABgAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAehIAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAam/WbbJxUAAAAAAAAAAAAAAAAY68qsN7VANABR1cirjzQnr2ePoQAAAAAAAAAAAAAAAHoKU4R3dvfpTMNXQpcayyIXsNuBAAAAAAAAAAAAAAAAegpThHd29+lMw1dClxrLIhew24EAAAAAAAAAAAAAAADcQROCYO6p+yN/S2/6yWwvOjTujAA=";

(async () => {
  const SOLANA_RPC_URL = "https://api.devnet.solana.com";
  const GUARDIAN_URL = "https://api.testnet.wormholescan.io";

  const coreBridgeAddress = new web3.PublicKey(
    "3u8hJUVTA4jH1wYAyUur7FFZVQ8H635K3tSHHF4ssjQ5"
  );

  const connection = new web3.Connection(SOLANA_RPC_URL, "confirmed");

  const key = process.env.SOLANA_KEY;

  if (!key) {
    throw new Error("SOLANA_KEY is required");
  }

  const payer = web3.Keypair.fromSecretKey(
    key.endsWith(".json") ? new Uint8Array(require(key)) : bs58.decode(key)
  );
  const provider = new AnchorProvider(connection, new Wallet(payer));
  setProvider(provider);

  const getShimProgram = async () => {
    const program = new Program<WormholePostMessageShim>(
      WormholePostMessageShimIdl as WormholePostMessageShim
    );
    return program;
  };

  const postMessage = async (
    program: Program<WormholePostMessageShim>,
    msg: string,
    consistencyLevel: number
  ): Promise<string> =>
    await program.methods
      .postMessage(
        0,
        consistencyLevel === 0 ? { confirmed: {} } : { finalized: {} },
        Buffer.from(msg, "ascii")
      )
      .accounts({
        emitter: program.provider.publicKey,
        sequence: web3.PublicKey.findProgramAddressSync(
          [Buffer.from("Sequence"), program.provider.publicKey.toBuffer()],
          coreBridgeAddress
        )[0],
      })
      .preInstructions([
        // gotta pay the fee
        web3.SystemProgram.transfer({
          fromPubkey: program.provider.publicKey,
          toPubkey: new web3.PublicKey(
            "7s3a1ycs16d6SNDumaRtjcoyMaTDZPavzgsmS3uUZYWX"
          ), // fee collector
          lamports: 100, // hardcoded for tilt in devnet_setup.sh
        }),
      ])
      .rpc();

  const testTopLevel = async (
    program: Program<WormholePostMessageShim>,
    msg: string,
    expectedSeq: bigint,
    consistencyLevel: number,
    testDescription: string
  ) => {
    const tx = await postMessage(program, msg, consistencyLevel);
    console.log(`tx: ${tx}`);
    const emitter = program.provider.publicKey.toBuffer().toString("hex");
    const seq = (await getSequenceFromTx(tx)).toString();
    console.log(`id: 1/${emitter}/${seq}`);

    const { vaaBytes } = await getSignedVAAWithRetry(
      [GUARDIAN_URL],
      1,
      emitter,
      seq,
      {
        transport: NodeHttpTransport(),
      },
      500, // every .5 secs
      100 // 100 times, or 50 seconds
    );

    const vaa = parseVaa(vaaBytes);
    if (
      vaa.sequence === expectedSeq &&
      vaa.consistencyLevel === consistencyLevel &&
      vaa.payload.equals(Buffer.from(msg, "ascii"))
    ) {
      console.log(`✅ ${testDescription} success!`);
    } else {
      throw new Error(`❌ ${testDescription} failed!`);
    }
  };

  const verifyVAA = async (vaaBytes: string) => {
    const program = new Program<WormholeVerifyVaaShim>(
      WormholeVerifyVaaShimIdl as WormholeVerifyVaaShim
    );
    const signatureKeypair = web3.Keypair.generate();
    const buf = Buffer.from(VAA, "base64");
    const vaa = parseVaa(buf);
    const tx = await program.methods
      .postSignatures(
        vaa.guardianSetIndex,
        vaa.guardianSignatures.length,
        vaa.guardianSignatures.map((s) => [s.index, ...s.signature])
      )
      .accounts({ guardianSignatures: signatureKeypair.publicKey })
      .signers([signatureKeypair])
      .rpc();
    console.log(`verify tx1: ${tx}`);

    const tx2 = await program.methods
      .verifyVaa([...keccak256(vaa.hash)])
      .accounts({
        guardianSignatures: signatureKeypair.publicKey,
      })
      .preInstructions([
        web3.ComputeBudgetProgram.setComputeUnitLimit({
          units: 420_000,
        }),
      ])
      .postInstructions([
        await program.methods
          .closeSignatures()
          .accounts({ guardianSignatures: signatureKeypair.publicKey })
          .instruction(),
      ])
      .rpc();
    console.log(`verify tx2: ${tx2}`);
  };

  {
    const program = await getShimProgram();
    let currentSequence = BigInt(0);
    try {
      currentSequence = (
        await getSequenceTracker(
          connection,
          web3.PublicKey.findProgramAddressSync(
            [Buffer.from("emitter")],
            program.programId
          )[0],
          coreBridgeAddress
        )
      ).sequence;
    } catch (e) {}
    await testTopLevel(
      program,
      "hello world",
      currentSequence,
      0,
      "Top level initial message, confirmed"
    );
    await testTopLevel(
      program,
      "hello everyone",
      currentSequence + BigInt(1),
      0,
      "Top level subsequent message, confirmed"
    );
  }
  {
    verifyVAA(VAA);
  }
})();
