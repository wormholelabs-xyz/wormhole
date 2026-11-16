#!/usr/bin/env bash
# Print an HS256 JWT for the LocalNet Ledger API (shared-secret auth).
# Usage: token.sh [subject]   (default subject: ledger-api-user, an admin on every validator)
set -euo pipefail

# Documented LocalNet development secret. Do not use outside LocalNet.
readonly LOCALNET_JWT_SECRET="unsafe"
readonly LOCALNET_JWT_AUDIENCE="https://canton.network.global"
readonly TOKEN_LIFETIME_SECONDS=86400

subject="${1:-ledger-api-user}"
if [[ ! "$subject" =~ ^[A-Za-z0-9._:-]+$ ]]; then
  echo "subject has characters outside [A-Za-z0-9._:-]: $subject" >&2
  exit 1
fi

base64url() { openssl base64 -A | tr '+/' '-_' | tr -d '='; }

expiry_unix=$(($(date +%s) + TOKEN_LIFETIME_SECONDS))
header="$(printf '%s' '{"alg":"HS256","typ":"JWT"}' | base64url)"
claims="$(printf '{"sub":"%s","aud":"%s","exp":%d}' "$subject" "$LOCALNET_JWT_AUDIENCE" "$expiry_unix" | base64url)"
signature="$(printf '%s.%s' "$header" "$claims" | openssl dgst -sha256 -hmac "$LOCALNET_JWT_SECRET" -binary | base64url)"
printf '%s.%s.%s\n' "$header" "$claims" "$signature"
