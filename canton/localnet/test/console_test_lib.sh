# Shared functions of the offline console tests. Source this file; do not run it.
# The tests run the Canton console jar against console/test/offline-participant.conf. They need no LocalNet.
# shellcheck shell=bash
# shellcheck source=SCRIPTDIR/../env.sh
source "$(dirname "${BASH_SOURCE[0]}")/../env.sh"

readonly OFFLINE_CONSOLE_CONFIG="$LOCALNET_SCRIPT_DIR/console/test/offline-participant.conf"
readonly OFFLINE_CONSOLE_TIMEOUT_SECONDS=300

# require_console_jar: fails unless the console jar of CANTON_RELEASE is in the vendor directory.
require_console_jar() {
  if [[ ! -f "$CANTON_CONSOLE_JAR" ]]; then
    echo "FAIL: missing $CANTON_CONSOLE_JAR. Copy it from a canton $CANTON_RELEASE image, or set CANTON_VENDOR_DIR." >&2
    exit 1
  fi
}

# run_offline_console <output-file> <environment-assignment>... -- <console-script>...
# Runs the scripts in a clean environment with only PATH, HOME, the vendor directory and the given NAME=VALUE
# assignments. CANTON_CONSOLE_OFFLINE=true makes canton_console.sh fail on a missing jar instead of calling Docker.
# Writes stdout and stderr to the output file. Returns the console exit status.
run_offline_console() {
  local output_file="$1" console_state_dir
  shift
  local -a environment_assignments=()
  while [[ $# -gt 0 && "$1" != "--" ]]; do
    environment_assignments+=("$1")
    shift
  done
  if [[ $# -lt 2 ]]; then
    echo "run_offline_console: expected -- and at least one console script" >&2
    return 2
  fi
  shift
  console_state_dir="$(mktemp -d)"
  local console_status=0
  env -i PATH="$PATH" HOME="$HOME" LOCALNET_STATE_DIR="$console_state_dir" CANTON_CONSOLE_CONFIG="$OFFLINE_CONSOLE_CONFIG" \
    CANTON_VENDOR_DIR="$VENDOR_DIR" CANTON_CONSOLE_OFFLINE=true \
    CANTON_CONSOLE_TIMEOUT_SECONDS="$OFFLINE_CONSOLE_TIMEOUT_SECONDS" ${environment_assignments[@]+"${environment_assignments[@]}"} \
    "$LOCALNET_SCRIPT_DIR/canton_console.sh" "$@" > "$output_file" 2>&1 || console_status=$?
  rm -rf "$console_state_dir"
  return "$console_status"
}
