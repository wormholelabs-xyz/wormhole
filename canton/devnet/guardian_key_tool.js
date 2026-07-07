#!/usr/bin/env node
//
// Minimal Ed25519 keypair / signing helper for guardianGovernance owner keys.
//
// This stands in for real guardian custody (HSM/KMS/offline signer) in local
// testing: `generate` produces a keypair the way a guardian's custody system
// would (an Ed25519 SubjectPublicKeyInfo DER public key, importable into
// Canton via `keys.public.upload`, and a PKCS8 DER private key that never
// leaves this host); `sign` produces a raw 64-byte Ed25519 signature over a
// given hash the way a guardian's signer would when asked to co-sign a
// prepared topology transaction or Ledger API interactive submission.
//
// Production usage: replace the `sign` subcommand with a call into the real
// custody system (HSM CLI, KMS API, offline signing ceremony) that accepts
// the same "hex hash in, hex signature out" contract -- guardian_governance.
// canton and genesis_guardian_governance.sh only depend on that CLI contract.
'use strict';
const crypto = require('crypto');
const fs = require('fs');

const [, , cmd, ...args] = process.argv;

function generate(prefix) {
  const { publicKey, privateKey } = crypto.generateKeyPairSync('ed25519');
  const pubDer = publicKey.export({ type: 'spki', format: 'der' });
  const privDer = privateKey.export({ type: 'pkcs8', format: 'der' });
  fs.writeFileSync(`${prefix}.pub`, pubDer);
  fs.writeFileSync(`${prefix}.key`, privDer, { mode: 0o600 });
  process.stdout.write(`${prefix}.pub\n${prefix}.key\n`);
}

function sign(keyFile, hashHex) {
  const privDer = fs.readFileSync(keyFile);
  const privateKey = crypto.createPrivateKey({ key: privDer, format: 'der', type: 'pkcs8' });
  const msg = Buffer.from(hashHex, 'hex');
  const sig = crypto.sign(null, msg, privateKey);
  process.stdout.write(sig.toString('hex') + '\n');
}

if (cmd === 'generate' && args.length === 1) {
  generate(args[0]);
} else if (cmd === 'sign' && args.length === 2) {
  sign(args[0], args[1]);
} else {
  console.error('usage: guardian_key_tool.js generate <prefix> | sign <keyfile.key> <hashHex>');
  process.exit(1);
}
