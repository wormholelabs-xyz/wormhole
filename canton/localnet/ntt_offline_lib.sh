# NTT functions that read only their arguments and local files. ntt_lib.sh and test/ntt_lib_test.sh source this file; do not run it.
# shellcheck shell=bash

ntt_contract() {
  jq -er --arg name "$1" '.[$name]' "$NTT_CONTRACTS"
}

record_ntt_contract() {
  local name="$1" contract_value="$2" updated
  if [[ -f "$NTT_CONTRACTS" ]]; then
    updated="$(jq --arg name "$name" --arg value "$contract_value" '.[$name] = $value' "$NTT_CONTRACTS")"
  else
    updated="$(jq -n --arg name "$name" --arg value "$contract_value" '{($name): $value}')"
  fi
  printf '%s\n' "$updated" > "$NTT_CONTRACTS"
}

record_ntt_json() {
  local name="$1" json_value="$2" updated
  updated="$(jq --arg name "$name" --argjson value "$json_value" '.[$name] = $value' "$NTT_CONTRACTS")"
  printf '%s\n' "$updated" > "$NTT_CONTRACTS"
}

# created_contract_argument <transaction-json> <qualified-template-suffix>: create arguments of the one such created event.
created_contract_argument() {
  jq -ec --arg suffix "$2" \
    '[.events[] | .CreatedEvent? // empty | select(.templateId | endswith($suffix)) | .createArgument]
     | if length == 1 then .[0] else error("expected exactly one created \($suffix), found \(length)") end' <<<"$1"
}

# external_party_signers_json <party> <key-file> <fingerprint>
external_party_signers_json() {
  jq -n --arg party "$1" --arg keyFile "$2" --arg fingerprint "$3" '{($party): [{keyFile: $keyFile, fingerprint: $fingerprint}]}'
}

create_command() {
  jq -n --arg templateId "$1" --argjson createArguments "$2" \
    '{CreateCommand: {templateId: $templateId, createArguments: $createArguments}}'
}

# exercise_result <transaction-json> <qualified-template-suffix> <choice>: the result of the one exercise of
# that choice on that template.
exercise_result() {
  jq -ec --arg suffix "$2" --arg choice "$3" '[.events[] | .ExercisedEvent? // empty
      | select(.choice == $choice and (.templateId | endswith($suffix))) | .exerciseResult]
    | if length == 1 then .[0] else error("expected exactly one \($suffix) \($choice) exercise, found \(length)") end' <<<"$1"
}

# message_summary <wormhole-message-json>: the WormholeMessage fields the watcher reads.
message_summary() {
  jq -c '{registrar: .registrar[0:24], owner: .owner[0:24], emitterId, sequence, nonce, consistencyLevel,
          payloadPrefix: .payload[0:8], payloadBytes: ((.payload | length) / 2)}' <<<"$1"
}

# json_array <json>...: one JSON array of the arguments.
json_array() {
  jq -sc '.' <<<"$(printf '%s\n' "$@")"
}

# dar_entries <dar>: the file names in the DAR archive.
dar_entries() {
  local entries
  entries="$(unzip -Z1 "$1" 2>/dev/null)" || { echo "cannot list $1 as a DAR" >&2; return 1; }
  printf '%s\n' "$entries"
}

# dar_main_package_directory <dar>: <name>-<version>-<package-id> of the main DALF, which is the only
# DALF named after its directory.
dar_main_package_directory() {
  local entries
  entries="$(dar_entries "$1")" || return 1
  jq -Rsr --arg dar "$1" '[split("\n")[] | select(test("^[^/]+/[^/]+\\.dalf$")) | split("/")
      | select(.[1] == .[0] + ".dalf") | .[0] | select(test("-[0-9a-f]{64}$"))]
    | if length == 1 then .[0] else error("\($dar): expected one main DALF, found \(length)") end' <<<"$entries"
}

# dar_main_package_id <dar>
dar_main_package_id() {
  local main_directory
  main_directory="$(dar_main_package_directory "$1")" || return 1
  printf '%s\n' "${main_directory: -64}"
}

# dar_main_package_name_and_version <dar>: "<name> <version>" of the main package.
dar_main_package_name_and_version() {
  local main_directory
  main_directory="$(dar_main_package_directory "$1")" || return 1
  jq -Rr 'capture("^(?<name>.+)-(?<version>[0-9]+(\\.[0-9]+)*)-[0-9a-f]{64}$") | "\(.name) \(.version)"' <<<"$main_directory"
}

# dar_package_ids <dar>: the package id of each DALF in the DAR, one per line.
dar_package_ids() {
  local entries
  entries="$(dar_entries "$1")" || return 1
  jq -Rr 'select(test("^[^/]+/[^/]+-[0-9a-f]{64}\\.dalf$")) | capture("-(?<id>[0-9a-f]{64})\\.dalf$").id' <<<"$entries" | sort -u
}

# dar_dependency_package_ids <dar> <package-name>: ids of the DALFs <package-name>-<version>-<id>.dalf in the DAR.
dar_dependency_package_ids() {
  local entries
  entries="$(dar_entries "$1")" || return 1
  jq -Rr --arg name "$2" 'select(test("^[^/]+/[^/]+\\.dalf$")) | split("/")[1]
      | select(startswith($name + "-")) | ltrimstr($name + "-")
      | select(test("^[0-9]+(\\.[0-9]+)*-[0-9a-f]{64}\\.dalf$")) | capture("-(?<id>[0-9a-f]{64})\\.dalf$").id' <<<"$entries"
}

# check_core_package_id_of_dar <dar> <expected-package-id>: the DAR carries exactly one wormhole-core DALF,
# with the expected id.
check_core_package_id_of_dar() {
  local dar="$1" expected_package_id="$2" core_package_ids core_package_count
  core_package_ids="$(dar_dependency_package_ids "$dar" wormhole-core)" || return 1
  core_package_count="$(grep -c . <<<"$core_package_ids")"
  if [[ "$core_package_count" -ne 1 ]]; then
    echo "$dar: expected one wormhole-core DALF, found $core_package_count" >&2
    return 1
  fi
  if [[ "$core_package_ids" != "$expected_package_id" ]]; then
    echo "$dar: wormhole-core $core_package_ids, expected $expected_package_id" >&2
    return 1
  fi
}

# Amounts in units stay below 10^18, so a sum of two fits in a signed 64-bit bash integer.
readonly MAX_UNITS_DIGITS=18

# decimal_to_units <decimal-text> <decimals>: the amount in the smallest unit, as an integer string.
decimal_to_units() {
  local decimal_text="$1" decimals="$2" integer_digits fraction_digits units_text
  if [[ ! "$decimals" =~ ^[0-9]{1,2}$ ]]; then
    echo "decimals must be 0 to 99, got '$decimals'" >&2
    return 1
  fi
  if [[ ! "$decimal_text" =~ ^([0-9]+)(\.([0-9]+))?$ ]]; then
    echo "'$decimal_text' is not a non-negative decimal number" >&2
    return 1
  fi
  integer_digits="${BASH_REMATCH[1]}"
  fraction_digits="${BASH_REMATCH[3]}"
  if (( ${#fraction_digits} > 10#$decimals )); then
    echo "'$decimal_text' has more than $((10#$decimals)) fraction digits" >&2
    return 1
  fi
  while (( ${#fraction_digits} < 10#$decimals )); do
    fraction_digits="${fraction_digits}0"
  done
  units_text="$integer_digits$fraction_digits"
  while [[ "${#units_text}" -gt 1 && "${units_text:0:1}" == "0" ]]; do
    units_text="${units_text:1}"
  done
  if (( ${#units_text} > MAX_UNITS_DIGITS )); then
    echo "'$decimal_text' has more than $MAX_UNITS_DIGITS digits in units" >&2
    return 1
  fi
  echo "$units_text"
}

# decimal_texts_total_units <decimals> [<decimal-text>...]: the sum in the smallest unit.
decimal_texts_total_units() {
  local decimals="$1" decimal_text amount_units total_units=0
  shift
  for decimal_text in "$@"; do
    amount_units="$(decimal_to_units "$decimal_text" "$decimals")" || return 1
    total_units=$((total_units + amount_units))
    if (( ${#total_units} > MAX_UNITS_DIGITS )); then
      echo "the sum has more than $MAX_UNITS_DIGITS digits in units" >&2
      return 1
    fi
  done
  echo "$total_units"
}

# json_decimal_array_total_units <decimals> <json-array-of-decimal-texts>: the sum in the smallest unit.
# The array must be non-empty.
json_decimal_array_total_units() {
  local decimals="$1" decimal_texts
  decimal_texts="$(jq -er 'if type == "array" and length > 0 and all(.[]; type == "string" and test("^[0-9]+(\\.[0-9]+)?$"))
      then join(" ") else error("expected a non-empty array of decimal strings") end' <<<"$2")" || return 1
  # shellcheck disable=SC2086 # jq checked each word against [0-9.]
  decimal_texts_total_units "$decimals" $decimal_texts
}
