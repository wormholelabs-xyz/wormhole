# Verification steps (solana core bridge — mainnet)

## Build
```shell
wormhole/solana $ make clean
wormhole/solana $ make NETWORK=mainnet artifacts
```

This command compiles all the contracts into the `artifacts-mainnet` directory using Docker — pinned to `linux/amd64` — to ensure the build artifacts are deterministic. The core bridge program is written to `artifacts-mainnet/bridge.so`, and its sha256 is recorded in `artifacts-mainnet/checksums.txt`.

## Verify
Contract at [https://explorer.solana.com/address/worm2ZoG2kUd4vFXhvjh93UUH596ayRfgQ2MgjNMTth](https://explorer.solana.com/address/worm2ZoG2kUd4vFXhvjh93UUH596ayRfgQ2MgjNMTth)

The on-chain program lives in an upgradeable program-data account that is larger than the program itself: the bytecode is followed by trailing zero-padding (reserved upgrade space). So we don't hash the dump directly — we hash only its first `N` bytes, where `N` is the size of the local build, and separately confirm the remainder is all zeros.

First, dump the on-chain program:
```shell
# core bridge
wormhole/solana$ solana program dump -u m worm2ZoG2kUd4vFXhvjh93UUH596ayRfgQ2MgjNMTth /tmp/onchain-bridge.so
```

Then compare the on-chain prefix's hash against the local build:
```shell
wormhole/solana$ N=$(wc -c < artifacts-mainnet/bridge.so)
wormhole/solana$ head -c "$N" /tmp/onchain-bridge.so | shasum -a 256   # on-chain prefix
wormhole/solana$ shasum -a 256 artifacts-mainnet/bridge.so             # local build
```

Both commands must print the same digest:
```
8282e17feb8ffa186ab1134c48c79b844608c4c1ee43d698db56c883d638a0c7
```

Finally, confirm the rest of the on-chain account is only padding (must print `0`):
```shell
wormhole/solana$ tail -c +"$((N + 1))" /tmp/onchain-bridge.so | tr -d '\0' | wc -c
```
