# Package ids and DARs of the wormhole-core 0.6.0 to 0.7.0 rehearsal (core_upgrade_rehearsal.sh).
# Source this file after env.sh and ntt_offline_lib.sh; do not run it.
# shellcheck shell=bash disable=SC2034

readonly CORE_V6_PACKAGE_ID="701a57c867e02d962f231bd9f0ff5186e16462a77289aa1b7aaba7fe95074c15"
readonly CORE_V7_PACKAGE_ID="7a22a1c4026b2a037b021063c4ff2bd5506be2e748aa9de8b7a6057c90c356eb"
readonly CORE_V6_VERSION="0.6.0"
readonly CORE_V7_VERSION="0.7.0"
# ntt 0.2.0 calls core 0.6.0 by package id. ntt 0.3.0 calls core 0.7.0 and is an upgrade of ntt 0.2.0.
readonly NTT_V2_PACKAGE_ID="8b9c3ee1c93b7039b39421b790c283f507c2b6a01171aa839873f220dc5ff84a"
readonly NTT_V3_PACKAGE_ID="291fcf920b971d50956845c2690723fb321f0a5b141a44caa5a16910f6ffa1a6"

readonly CORE_V6_DAR="$REPO_ROOT/canton/dars/wormhole-core-0.6.0.dar"
readonly CORE_V7_DAR="$REPO_ROOT/canton/dars/wormhole-core-0.7.0.dar"
readonly NTT_V2_DAR="${NTT_REPO_DIR:-$(cd "$REPO_ROOT/.." && pwd)/ntt}/canton/dars/ntt-0.2.0.dar"
# The NTT branch builds ntt 0.3.4 on core 0.8.3. Set NTT_V3_DAR to an ntt 0.3.0 build on core 0.7.0.
readonly NTT_V3_DAR="${NTT_V3_DAR:-}"

# check_rehearsal_dar <dar> <expected "name version"> <expected-package-id>
check_rehearsal_dar() {
  local dar_path="$1" expected_name_and_version="$2" expected_package_id="$3" name_and_version package_id
  [[ -f "$dar_path" ]] || { echo "DAR not found: $dar_path" >&2; return 1; }
  name_and_version="$(dar_main_package_name_and_version "$dar_path")" || return 1
  [[ "$name_and_version" == "$expected_name_and_version" ]] \
    || { echo "$dar_path is $name_and_version, expected $expected_name_and_version" >&2; return 1; }
  package_id="$(dar_main_package_id "$dar_path")" || return 1
  [[ "$package_id" == "$expected_package_id" ]] \
    || { echo "$dar_path is $expected_name_and_version $package_id, expected $expected_package_id" >&2; return 1; }
}

# write_rehearsal_approved_core_packages <file>: the approved list of the rehearsal watchers, in the format of
# node/pkg/cantonclient/approved_core_packages.json. LocalNet only: guardiand does not approve 0.6.0 or 0.7.0.
write_rehearsal_approved_core_packages() {
  jq -n --arg v6 "$CORE_V6_PACKAGE_ID" --arg v6_version "$CORE_V6_VERSION" \
    --arg v7 "$CORE_V7_PACKAGE_ID" --arg v7_version "$CORE_V7_VERSION" \
    '[{packageId: $v6, version: $v6_version}, {packageId: $v7, version: $v7_version}]' > "$1"
}
