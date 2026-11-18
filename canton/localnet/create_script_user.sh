#!/usr/bin/env bash
# Create the Ledger API user that Daml Script runs as on one LocalNet validator.
# Usage: create_script_user.sh [validator]   (default app-provider)
# The user gets ParticipantAdmin, CanActAsAnyParty and CanReadAsAnyParty, because
# Daml Script submits as parties that it allocates during the run.
set -euo pipefail
# shellcheck source=env.sh
source "$(dirname "${BASH_SOURCE[0]}")/env.sh"

validator_name="${1:-app-provider}"
ledger_json_url="$(json_api_url "$validator_name")"
work_dir="$(mktemp -d)"
trap 'rm -rf "$work_dir"' EXIT
response_file="$work_dir/response.json"
auth_header_file="$work_dir/admin.auth"
write_auth_header_file "$auth_header_file"

http_status="$(curl -sS -o "$response_file" -w '%{http_code}' \
  -X POST "$ledger_json_url/v2/users" \
  -H "@$auth_header_file" \
  -H 'Content-Type: application/json' \
  -d "{\"user\":{\"id\":\"$SCRIPT_USER_ID\",\"primaryParty\":\"\",\"isDeactivated\":false,\"identityProviderId\":\"\"},
       \"rights\":[{\"kind\":{\"ParticipantAdmin\":{\"value\":{}}}},
                   {\"kind\":{\"CanActAsAnyParty\":{\"value\":{}}}},
                   {\"kind\":{\"CanReadAsAnyParty\":{\"value\":{}}}}]}")"

case "$http_status" in
  200) echo "Created user $SCRIPT_USER_ID on $validator_name" ;;
  409) echo "User $SCRIPT_USER_ID already exists on $validator_name" ;;
  *)
    echo "Create user failed with HTTP $http_status: $(cat "$response_file")" >&2
    exit 1
    ;;
esac
