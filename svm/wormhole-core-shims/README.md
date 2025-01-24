# Wormhole Core Shims

The intent of the following programs is to reduce the cost of Core Bridge message emission and verification on Solana without making changes to the existing core bridge.

- [Wormhole Post Message Shim](programs/wormhole-post-message-shim/README.md)
- [Wormhole Verify VAA Shim](programs/wormhole-verify-vaa-shim/README.md)

The following are provided for example purposes only

- [Wormhole Integrator Example](programs/wormhole-integrator-example/src/lib.rs)

## Tests

See examples of integration testing each program in [tests](tests); run with `anchor test`.

For initial end-to-end (e2e) testing the post message shim with the guardian, the programs were built with the following:

```bash
anchor build -- --no-default-features --features localnet
```

The resulting `.so`s were then

- copied to [../../solana/tests/artifacts](../../solana/tests/artifacts)
- copied into the test validator dockerfile in [../../solana/Dockerfile.test-validator](../../solana/Dockerfile.test-validator)
- and loaded into the test validator at startup in [../../devnet/solana-devnet.yaml](../../devnet/solana-devnet.yaml)

The IDLs for devnet (as the Wormhole core program is different) were copied into [tests/idls/devnet](tests/idls/devnet).

The e2e tests can then be run with `npx tsx tests/e2e.ts` against a running Tilt environment with at least `tilt up -- --solana --manual`.

These are _not_ currently run within Tilt. It would be prudent to both build and run these within Tilt once the shim approach has been finalized.

## Building

### Tilt

```bash
anchor build -- --no-default-features --features localnet
```

### Wormhole Testnet / Solana Devnet

```bash
anchor build --verifiable -- --no-default-features --features testnet
```

### Mainnet

```bash
anchor build --verifiable
```

## Deploying

### Tilt

See [Tests](#tests)

### Wormhole Testnet / Solana Devnet

```bash
anchor deploy --provider.cluster devnet --provider.wallet ~/.config/solana/your-key.json -p <PROGRAM_NAME>
```

### Mainnet

```bash
anchor deploy --provider.cluster mainnet --provider.wallet ~/.config/solana/your-key.json -p <PROGRAM_NAME>
```

## Upgrading

```
anchor upgrade --provider.cluster <network> --provider.wallet ~/.config/solana/your-key.json --program-id <PROGRAM_ID> target/deploy/<program>.so
```

If you get an error like this

```
Error: Deploying program failed: RPC response error -32002: Transaction simulation failed: Error processing Instruction 0: account data too small for instruction [3 log messages]
```

Don't fret! Just extend the program size.

```
solana program -u <network> -k ~/.config/solana/your-key.json extend <PROGRAM_ID> <ADDITIONAL_BYTES>
```

You can view the current program size with `solana program -u <network> show <PROGRAM_ID>`.

## Changing the Program ID

Build the program with `anchor build`.

Get the newly generated program ID with `solana address -k target/deploy/<program>-keypair.json`.

Update the `declare_id!` macro in [lib.rs](./programs/<program>/src/lib.rs) and the program's address in [Anchor.toml](./Anchor.toml) with the above key.
