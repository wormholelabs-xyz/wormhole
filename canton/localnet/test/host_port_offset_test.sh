#!/usr/bin/env bash
# Table-driven test: LOCALNET_HOST_PORT_OFFSET moves the host ports of the Ledger, JSON and validator APIs.
# A tunnel to a remote LocalNet then uses ports that do not collide with a local LocalNet.
# Usage: canton/localnet/test/host_port_offset_test.sh
set -uo pipefail
LOCALNET_TEST_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
readonly LOCALNET_TEST_DIR
readonly ENV_FILE="$LOCALNET_TEST_DIR/../env.sh"

failure_count=0

# <offset or "unset"> <expression> <expected output or "error">
readonly CASES=(
  "unset|json_api_url app-user|http://localhost:2975"
  "unset|json_api_url app-provider|http://localhost:3975"
  "unset|grpc_ledger_api_port sv|4901"
  "unset|validator_api_port app-user|2903"
  "0|json_api_url app-user|http://localhost:2975"
  "10000|json_api_url app-user|http://localhost:12975"
  "10000|json_api_url app-provider|http://localhost:13975"
  "10000|grpc_ledger_api_port sv|14901"
  "10000|validator_api_port app-user|12903"
  "60000|json_api_url sv|http://localhost:64975"
  "60001|json_api_url app-user|error"
  "-1|json_api_url app-user|error"
  "abc|json_api_url app-user|error"
  "1e3|json_api_url app-user|error"
  "|json_api_url app-user|error"
  "10000|json_api_url unknown|error"
)

for test_case in "${CASES[@]}"; do
  IFS='|' read -r offset expression expected <<<"$test_case"
  # shellcheck disable=SC1090 # env.sh path is resolved at run time
  actual="$(
    if [[ "$offset" == "unset" ]]; then unset LOCALNET_HOST_PORT_OFFSET; else export LOCALNET_HOST_PORT_OFFSET="$offset"; fi
    source "$ENV_FILE" 2> /dev/null || exit 1
    # shellcheck disable=SC2086 # the expression is a function name and its arguments
    $expression 2> /dev/null
  )" || actual="error"
  if [[ "$actual" == "$expected" ]]; then
    echo "PASS: offset '$offset' $expression -> $expected"
  else
    echo "FAIL: offset '$offset' $expression -> '$actual', expected '$expected'" >&2
    failure_count=$((failure_count + 1))
  fi
done

if (( failure_count > 0 )); then
  echo "host_port_offset_test: $failure_count failed" >&2
  exit 1
fi
echo "host_port_offset_test: all ${#CASES[@]} cases passed"
