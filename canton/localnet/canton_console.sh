#!/usr/bin/env bash
# Run a Canton console script against the three LocalNet participants (admin APIs 4902, 3902, 2902).
# Usage: canton_console.sh <bootstrap-script>
# The console jar is copied from the running `canton` container on first use, so it matches the node release.
set -euo pipefail
# shellcheck source=env.sh
source "$(dirname "${BASH_SOURCE[0]}")/env.sh"

readonly CONSOLE_HEAP="${CANTON_CONSOLE_HEAP:-1g}"
bootstrap_script="${1:?usage: canton_console.sh <bootstrap-script>}"

if [[ ! -f "$CANTON_CONSOLE_JAR" ]]; then
  mkdir -p "$(dirname "$CANTON_CONSOLE_JAR")"
  docker cp "canton:/app/lib/$(basename "$CANTON_CONSOLE_JAR")" "$CANTON_CONSOLE_JAR"
fi

bootstrap_script="$(cd "$(dirname "$bootstrap_script")" && pwd)/$(basename "$bootstrap_script")"
# The console writes log/ and compiled script files to its working directory.
mkdir -p "$LOCALNET_STATE_DIR/console-work"
cd "$LOCALNET_STATE_DIR/console-work"
java "-Xmx$CONSOLE_HEAP" -jar "$CANTON_CONSOLE_JAR" --no-tty \
  -c "$CANTON_CONSOLE_CONFIG" --bootstrap "$bootstrap_script" < /dev/null
