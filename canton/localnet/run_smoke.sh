#!/usr/bin/env bash
# Upload the smoke DAR and run LocalNet.Publish:bootstrapPublishAndVerify on one validator.
# Usage: run_smoke.sh [validator]   (default app-provider)
# Precondition: upload_dars.sh ran on the same validator.
set -euo pipefail
# shellcheck source=env.sh
source "$(dirname "${BASH_SOURCE[0]}")/env.sh"

readonly SMOKE_DAR="$LOCALNET_SCRIPT_DIR/daml/.daml/dist/wormhole-localnet-smoke-0.1.0.dar"
readonly DPM_BIN_DIR="${DPM_BIN_DIR:-$HOME/.dpm/bin}"

validator_name="${1:-app-provider}"
if [[ ! -f "$SMOKE_DAR" ]]; then
  echo "DAR not found: $SMOKE_DAR (run 'dpm build --all' in $REPO_ROOT/canton)" >&2
  exit 1
fi

"$LOCALNET_SCRIPT_DIR/create_script_user.sh" "$validator_name"

work_dir="$(mktemp -d)"
trap 'rm -rf "$work_dir"' EXIT
"$LOCALNET_SCRIPT_DIR/token.sh" "$SCRIPT_USER_ID" > "$work_dir/script.jwt"
run_label="run$(date +%s)"
printf '"%s"\n' "$run_label" > "$work_dir/input.json"

PATH="$DPM_BIN_DIR:$PATH" dpm script \
  --dar "$SMOKE_DAR" \
  --upload-dar yes \
  --script-name LocalNet.Publish:bootstrapPublishAndVerify \
  --input-file "$work_dir/input.json" \
  --ledger-host localhost \
  --ledger-port "$(grpc_ledger_api_port "$validator_name")" \
  --access-token-file "$work_dir/script.jwt" \
  --output-file "$work_dir/result.json"

cat "$work_dir/result.json"
echo
