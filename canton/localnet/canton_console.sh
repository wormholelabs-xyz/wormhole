#!/usr/bin/env bash
# Run a Canton console script against the three LocalNet participants (admin APIs 4902, 3902, 2902).
# Usage: canton_console.sh <console-script>...
#   Several scripts run as one bootstrap file, in argument order (for example a prelude, then the script).
# The console jar is copied from the running `canton` container on first use, so it matches the node release.
# CANTON_CONSOLE_OFFLINE=true: fail on a missing jar instead of the copy.
# CANTON_CONSOLE_TIMEOUT_SECONDS (default 600, at most 3600) stops a console run that does not finish; exit status 124.
set -euo pipefail
# shellcheck source=env.sh
source "$(dirname "${BASH_SOURCE[0]}")/env.sh"
# shellcheck source=bounded_run.sh
source "$(dirname "${BASH_SOURCE[0]}")/bounded_run.sh"

readonly CONSOLE_HEAP="${CANTON_CONSOLE_HEAP:-1g}"
readonly CONSOLE_TIMEOUT_SECONDS="${CANTON_CONSOLE_TIMEOUT_SECONDS:-600}"
readonly MAX_CONSOLE_SCRIPTS=8
if [[ $# -lt 1 || $# -gt $MAX_CONSOLE_SCRIPTS ]]; then
  echo "usage: canton_console.sh <console-script>... (1 to $MAX_CONSOLE_SCRIPTS scripts)" >&2
  exit 2
fi
for console_script in "$@"; do
  if [[ ! -f "$console_script" ]]; then
    echo "Missing console script $console_script" >&2
    exit 2
  fi
done

if [[ ! -f "$CANTON_CONSOLE_JAR" && "${CANTON_CONSOLE_OFFLINE:-}" == true ]]; then
  echo "Missing $CANTON_CONSOLE_JAR. An offline run (CANTON_CONSOLE_OFFLINE=true) does not copy it from Docker." >&2
  exit 2
fi
if [[ ! -f "$CANTON_CONSOLE_JAR" ]]; then
  mkdir -p "$(dirname "$CANTON_CONSOLE_JAR")"
  docker cp "canton:/app/lib/$(basename "$CANTON_CONSOLE_JAR")" "$CANTON_CONSOLE_JAR"
fi

# The console writes log/ and compiled script files to its working directory.
mkdir -p "$LOCALNET_STATE_DIR/console-work"
bootstrap_dir="$(mktemp -d "$LOCALNET_STATE_DIR/console-work/bootstrap-XXXXXX")"
trap 'rm -rf "$bootstrap_dir"' EXIT
bootstrap_script="$bootstrap_dir/bootstrap.canton"
cat "$@" > "$bootstrap_script"
cd "$LOCALNET_STATE_DIR/console-work"
run_with_timeout_seconds "$CONSOLE_TIMEOUT_SECONDS" java "-Xmx$CONSOLE_HEAP" -jar "$CANTON_CONSOLE_JAR" --no-tty \
  -c "$CANTON_CONSOLE_CONFIG" --bootstrap "$bootstrap_script" < /dev/null
