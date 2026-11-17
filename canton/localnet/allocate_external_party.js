#!/usr/bin/env node
// Allocate a 1-of-1 Ed25519 external party on one participant through the JSON Ledger API v2.
// Usage: allocate_external_party.js <json-api-url> <token-file> <synchronizer-id> <party-hint> <key-file-prefix>
// Writes <key-file-prefix>.key (PKCS8 DER, the format interactive_submit.js reads) and <key-file-prefix>.pub
// (SPKI DER). Prints {partyId, fingerprint, keyFile}. Fails with EEXIST if either key file exists.
'use strict';
const crypto = require('crypto');
const fs = require('fs');

const REQUEST_TIMEOUT_MS = 120000;
const ED25519_SIGNATURE_BYTES = 64;

async function postJson(baseUrl, token, path, body) {
  const response = await fetch(`${baseUrl}${path}`, {
    method: 'POST',
    headers: { 'Content-Type': 'application/json', Authorization: `Bearer ${token}` },
    body: JSON.stringify(body),
    signal: AbortSignal.timeout(REQUEST_TIMEOUT_MS),
  });
  const text = await response.text();
  if (!response.ok) throw new Error(`${path} failed with HTTP ${response.status}: ${text}`);
  return JSON.parse(text);
}

async function main() {
  const [, , baseUrl, tokenFile, synchronizerId, partyHint, keyFilePrefix] = process.argv;
  if (!baseUrl || !tokenFile || !synchronizerId || !partyHint || !keyFilePrefix) {
    process.stderr.write('usage: allocate_external_party.js <json-api-url> <token-file> <synchronizer-id> <party-hint> <key-file-prefix>\n');
    process.exit(2);
  }
  const keyFile = `${keyFilePrefix}.key`;
  const token = fs.readFileSync(tokenFile, 'utf8').trim();

  const { publicKey, privateKey } = crypto.generateKeyPairSync('ed25519');
  const publicKeyDer = publicKey.export({ format: 'der', type: 'spki' });
  // flag 'wx': EEXIST stops the run if a key file of this prefix exists.
  fs.writeFileSync(keyFile, privateKey.export({ format: 'der', type: 'pkcs8' }), { mode: 0o600, flag: 'wx' });
  fs.writeFileSync(`${keyFilePrefix}.pub`, publicKeyDer, { flag: 'wx' });

  const topology = await postJson(baseUrl, token, '/v2/parties/external/generate-topology', {
    synchronizer: synchronizerId,
    partyHint,
    publicKey: {
      format: 'CRYPTO_KEY_FORMAT_DER_X509_SUBJECT_PUBLIC_KEY_INFO',
      keyData: publicKeyDer.toString('base64'),
      keySpec: 'SIGNING_KEY_SPEC_EC_CURVE25519',
    },
    localParticipantObservationOnly: false,
    otherConfirmingParticipantUids: [],
    confirmationThreshold: 1,
    observingParticipantUids: [],
  });
  // LocalNet only: signs the participant multiHash without recomputing it. Do not use on a production network.
  const multiHashSignature = crypto.sign(null, Buffer.from(topology.multiHash, 'base64'), privateKey);
  if (multiHashSignature.length !== ED25519_SIGNATURE_BYTES) {
    throw new Error(`signature has ${multiHashSignature.length} bytes, expected ${ED25519_SIGNATURE_BYTES}`);
  }
  const allocated = await postJson(baseUrl, token, '/v2/parties/external/allocate', {
    synchronizer: synchronizerId,
    onboardingTransactions: topology.topologyTransactions.map((transaction) => ({ transaction })),
    multiHashSignatures: [{
      format: 'SIGNATURE_FORMAT_CONCAT',
      signature: multiHashSignature.toString('base64'),
      signedBy: topology.publicKeyFingerprint,
      signingAlgorithmSpec: 'SIGNING_ALGORITHM_SPEC_ED25519',
    }],
    identityProviderId: '',
  });
  if (allocated.partyId !== topology.partyId) {
    throw new Error(`allocated ${allocated.partyId}, expected ${topology.partyId}`);
  }
  process.stdout.write(JSON.stringify({ partyId: allocated.partyId, fingerprint: topology.publicKeyFingerprint, keyFile }) + '\n');
}

main().catch((error) => {
  process.stderr.write(`${error.stack || error}\n`);
  process.exit(1);
});
