#!/usr/bin/env bash
# Table-driven test: each curl call of the core upgrade scripts has a maximum time.
# A "helper" file calls curl through curl_bounded of gg_lib.sh. Each curl call of a "standalone" file sets its own
# --connect-timeout and --max-time.
# Usage: canton/localnet/test/curl_calls_are_bounded_test.sh
set -uo pipefail
LOCALNET_TEST_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
readonly LOCALNET_TEST_DIR
readonly LOCALNET_DIR="$LOCALNET_TEST_DIR/.."
readonly CANTON_DIR="$LOCALNET_TEST_DIR/../.."

# <file> <helper|standalone>
readonly CHECKED_FILES=(
  "$LOCALNET_DIR/gg_lib.sh standalone"
  "$LOCALNET_DIR/core_upgrade_rehearsal.sh helper"
  "$LOCALNET_DIR/watcher_harness_lib.sh helper"
  "$CANTON_DIR/tools/publish_message_scan.sh standalone"
)

failure_count=0

# curl_command_lines <file>: "<line number>:<text>" of each line outside a comment that runs curl. A curl command has
# an option or a line continuation after the word curl.
curl_command_lines() {
  grep -nE '(^|[^A-Za-z0-9_-])curl[[:space:]]+(-|\\$)' "$1" | grep -vE '^[0-9]+:[[:space:]]*#'
}

# check_file <file> <helper|standalone>
check_file() {
  local file_path="$1" file_kind="$2" curl_lines line
  if [[ ! -f "$file_path" ]]; then
    echo "FAIL: $file_path does not exist" >&2
    failure_count=$((failure_count + 1))
    return
  fi
  curl_lines="$(curl_command_lines "$file_path")"
  case "$file_kind" in
    helper)
      if [[ -n "$curl_lines" ]]; then
        while IFS= read -r line; do
          echo "FAIL: $(basename "$file_path"):$line calls curl, not curl_bounded" >&2
          failure_count=$((failure_count + 1))
        done <<<"$curl_lines"
        return
      fi
      ;;
    standalone)
      if [[ -z "$curl_lines" ]]; then
        echo "FAIL: $(basename "$file_path") has no curl call; the table is out of date" >&2
        failure_count=$((failure_count + 1))
        return
      fi
      local unbounded_line_count=0
      while IFS= read -r line; do
        if [[ "$line" != *"--max-time"* || "$line" != *"--connect-timeout"* ]]; then
          echo "FAIL: $(basename "$file_path"):$line has no --connect-timeout and --max-time" >&2
          unbounded_line_count=$((unbounded_line_count + 1))
        fi
      done <<<"$curl_lines"
      if [[ "$unbounded_line_count" -ne 0 ]]; then
        failure_count=$((failure_count + unbounded_line_count))
        return
      fi
      ;;
    *)
      echo "FAIL: unknown file kind $file_kind" >&2
      failure_count=$((failure_count + 1))
      return
      ;;
  esac
  echo "ok: $(basename "$file_path") ($file_kind)"
}

# The detector must find a bare curl call and ignore curl_bounded and comments.
detector_sample="$(mktemp)"
trap 'rm -f "$detector_sample"' EXIT
# shellcheck disable=SC2016,SC1003 # literal sample lines
printf '%s\n' '# curl in a comment' 'curl_bounded -sS x' 'value="$(curl -sS x)"' '  curl -sS y \' \
  'echo "read failed with curl status 7"' '  curl \' > "$detector_sample"
if [[ "$(curl_command_lines "$detector_sample" | cut -d: -f1 | paste -sd, -)" == "3,4,6" ]]; then
  echo "ok: the detector finds lines 3, 4 and 6 of the sample"
else
  echo "FAIL: the detector finds lines '$(curl_command_lines "$detector_sample" | cut -d: -f1 | paste -sd, -)', expected 3,4,6" >&2
  failure_count=$((failure_count + 1))
fi

for checked_file in "${CHECKED_FILES[@]}"; do
  # shellcheck disable=SC2086 # two words: path and kind
  check_file $checked_file
done

if [[ "$failure_count" -ne 0 ]]; then
  echo "curl_calls_are_bounded_test.sh: $failure_count failures" >&2
  exit 1
fi
echo "curl_calls_are_bounded_test.sh: PASS"
