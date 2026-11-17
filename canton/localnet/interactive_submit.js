#!/usr/bin/env node
// WARNING: LocalNet only. This is not a production signer. It signs the preparedTransactionHash that the
// participant returns and does not recompute it from preparedTransaction, so it trusts the participant
// to describe the transaction correctly. A production signer must decode preparedTransaction, check its
// content, and compute the hash itself (Canton transaction hashing scheme) before it signs.
// Prepare, externally sign and execute one Ledger API interactive submission (JSON Ledger API v2).
// The prepare endpoint takes exactly one command ("Preparing multiple commands is currently not supported").
// Usage: interactive_submit.js <json-api-url> <token-file> <submission-spec.json>
// Spec: { commandId, actAs: [party], commands: [Command], disclosedContracts?: [DisclosedContract],
//         synchronizerId?, packageIdSelectionPreference?: [packageId],
//         signers: { <party>: [ { keyFile: <PKCS8 DER Ed25519 key>, fingerprint } ] },
//         awaitCompletion?: true }
// Prints the committed transaction as JSON. On failure prints the API error and exits 1.
// With awaitCompletion, executes asynchronously and prints the command completion instead,
// so the synchronizer verdict (for example a mediator timeout) is visible. Exits 1 unless it is a success.
'use strict';
const crypto = require('crypto');
const fs = require('fs');

const REQUEST_TIMEOUT_MS = 300000;
const COMPLETION_POLL_MAX_ATTEMPTS = 180;
const COMPLETION_STREAM_IDLE_TIMEOUT_MS = 5000;

function signHashEd25519(keyFile, hashBytes) {
  const privateKey = crypto.createPrivateKey({ key: fs.readFileSync(keyFile), format: 'der', type: 'pkcs8' });
  return crypto.sign(null, hashBytes, privateKey);
}

// LEDGER_EFFECTS keeps exercised events, so callers can read choice results such as the WormholeMessage.
function ledgerEffectsFormat(parties) {
  const wildcard = { cumulative: [{ identifierFilter: { WildcardFilter: { value: { includeCreatedEventBlob: false } } } }] };
  return {
    eventFormat: { filtersByParty: Object.fromEntries(parties.map((party) => [party, wildcard])), verbose: true },
    transactionShape: 'TRANSACTION_SHAPE_LEDGER_EFFECTS',
  };
}

async function getJson(baseUrl, token, path) {
  const response = await fetch(`${baseUrl}${path}`, {
    headers: { Authorization: `Bearer ${token}` },
    signal: AbortSignal.timeout(REQUEST_TIMEOUT_MS),
  });
  if (!response.ok) throw new Error(`${path} failed with HTTP ${response.status}: ${await response.text()}`);
  return response.json();
}

async function awaitCompletion(baseUrl, token, parties, beginExclusive, submissionId) {
  for (let attempt = 0; attempt < COMPLETION_POLL_MAX_ATTEMPTS; attempt += 1) {
    const responses = await postJson(baseUrl, token,
      `/v2/commands/completions?stream_idle_timeout_ms=${COMPLETION_STREAM_IDLE_TIMEOUT_MS}`,
      { parties, beginExclusive });
    for (const entry of responses) {
      const completion = entry.completionResponse && entry.completionResponse.Completion
        ? entry.completionResponse.Completion.value : undefined;
      if (completion && completion.submissionId === submissionId) return completion;
    }
  }
  throw new Error(`no completion for ${submissionId} after ${COMPLETION_POLL_MAX_ATTEMPTS} polls`);
}

async function postJson(baseUrl, token, path, body) {
  const response = await fetch(`${baseUrl}${path}`, {
    method: 'POST',
    headers: { 'Content-Type': 'application/json', Authorization: `Bearer ${token}` },
    body: JSON.stringify(body),
    signal: AbortSignal.timeout(REQUEST_TIMEOUT_MS),
  });
  const text = await response.text();
  if (!response.ok) {
    process.stderr.write(`${path} failed with HTTP ${response.status}: ${text}\n`);
    process.exit(1);
  }
  return JSON.parse(text);
}

async function main() {
  const [, , baseUrl, tokenFile, specFile] = process.argv;
  if (!baseUrl || !tokenFile || !specFile) {
    process.stderr.write('usage: interactive_submit.js <json-api-url> <token-file> <submission-spec.json>\n');
    process.exit(2);
  }
  const token = fs.readFileSync(tokenFile, 'utf8').trim();
  const spec = JSON.parse(fs.readFileSync(specFile, 'utf8'));
  if (!Array.isArray(spec.actAs) || spec.actAs.length === 0) throw new Error('spec.actAs must be non-empty');
  if (!Array.isArray(spec.commands) || spec.commands.length !== 1) throw new Error('spec.commands must hold exactly one command');

  const prepared = await postJson(baseUrl, token, '/v2/interactive-submission/prepare', {
    commandId: spec.commandId,
    actAs: spec.actAs,
    readAs: [],
    commands: spec.commands,
    disclosedContracts: spec.disclosedContracts || [],
    synchronizerId: spec.synchronizerId || '',
    packageIdSelectionPreference: spec.packageIdSelectionPreference || [],
    verboseHashing: false,
    prefetchContractKeys: [],
  });
  // Trusts the participant's hash; see the warning at the top of this file.
  const hashBytes = Buffer.from(prepared.preparedTransactionHash, 'base64');
  if (hashBytes.length !== 32) throw new Error(`prepared hash has ${hashBytes.length} bytes, expected 32`);
  process.stderr.write(`prepared ${spec.commandId}: hash ${hashBytes.toString('hex')}\n`);

  const partySignatures = Object.entries(spec.signers || {}).map(([party, partySigners]) => ({
    party,
    signatures: partySigners.map((signer) => ({
      format: 'SIGNATURE_FORMAT_CONCAT',
      signature: signHashEd25519(signer.keyFile, hashBytes).toString('base64'),
      signedBy: signer.fingerprint,
      signingAlgorithmSpec: 'SIGNING_ALGORITHM_SPEC_ED25519',
    })),
  }));

  const submissionId = `${spec.commandId}-${crypto.randomUUID()}`;
  if (spec.awaitCompletion) {
    const ledgerEnd = await getJson(baseUrl, token, '/v2/state/ledger-end');
    const startedMs = Date.now();
    await postJson(baseUrl, token, '/v2/interactive-submission/execute', {
      preparedTransaction: prepared.preparedTransaction,
      partySignatures: { signatures: partySignatures },
      submissionId,
      hashingSchemeVersion: prepared.hashingSchemeVersion,
      deduplicationPeriod: { Empty: {} },
    });
    const completion = await awaitCompletion(baseUrl, token, spec.actAs, ledgerEnd.offset, submissionId);
    process.stdout.write(JSON.stringify({ elapsedMs: Date.now() - startedMs, completion }) + '\n');
    process.exit(completion.status && completion.status.code ? 1 : 0);
  }
  const executed = await postJson(baseUrl, token, '/v2/interactive-submission/executeAndWaitForTransaction', {
    preparedTransaction: prepared.preparedTransaction,
    partySignatures: { signatures: partySignatures },
    submissionId,
    hashingSchemeVersion: prepared.hashingSchemeVersion,
    deduplicationPeriod: { Empty: {} },
    transactionFormat: ledgerEffectsFormat(spec.actAs),
  });
  process.stdout.write(JSON.stringify(executed.transaction) + '\n');
}

main().catch((error) => {
  process.stderr.write(`${error.stack || error}\n`);
  process.exit(1);
});
