# NTT functions that read only their arguments and local files. ntt_lib.sh and test/ntt_lib_test.sh source this file; do not run it.
# shellcheck shell=bash

ntt_contract() {
  jq -er --arg name "$1" '.[$name]' "$NTT_CONTRACTS"
}

# write_ntt_contracts_atomically <json>: write to a temporary file in the same directory, then rename.
write_ntt_contracts_atomically() {
  local temporary_file
  jq -e . <<<"$1" > /dev/null || { echo "not JSON; $NTT_CONTRACTS stays unchanged" >&2; return 1; }
  temporary_file="$(mktemp "$NTT_CONTRACTS.XXXXXX")" || return 1
  if ! printf '%s\n' "$1" > "$temporary_file"; then
    rm -f "$temporary_file"
    return 1
  fi
  mv -f "$temporary_file" "$NTT_CONTRACTS"
}

record_ntt_contract() {
  local name="$1" contract_value="$2" updated
  if [[ -f "$NTT_CONTRACTS" ]]; then
    updated="$(jq --arg name "$name" --arg value "$contract_value" '.[$name] = $value' "$NTT_CONTRACTS")" || return 1
  else
    updated="$(jq -n --arg name "$name" --arg value "$contract_value" '{($name): $value}')" || return 1
  fi
  write_ntt_contracts_atomically "$updated"
}

record_ntt_json() {
  local name="$1" json_value="$2" updated
  if [[ ! -f "$NTT_CONTRACTS" ]]; then
    echo "$NTT_CONTRACTS does not exist; run ntt_governance.sh first" >&2
    return 1
  fi
  updated="$(jq --arg name "$name" --argjson value "$json_value" '.[$name] = $value' "$NTT_CONTRACTS")" || return 1
  write_ntt_contracts_atomically "$updated"
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

# subject_auth_header_file <subject>: path of a 0600 header file in $gg_work_dir with a token for that subject.
subject_auth_header_file() {
  local subject="$1" header_file
  if [[ ! "$subject" =~ ^[A-Za-z0-9._-]+$ || "$subject" == .* ]]; then
    echo "subject has characters outside [A-Za-z0-9._-] or starts with a dot: $subject" >&2
    return 1
  fi
  # shellcheck disable=SC2154 # gg_lib.sh or the test sets gg_work_dir
  header_file="$gg_work_dir/subject-$subject.auth"
  if [[ ! -f "$header_file" ]]; then
    write_auth_header_file "$header_file" "$subject" || return 1
  fi
  echo "$header_file"
}

readonly MAX_RELEASE_AFTER_ROUNDS=24

# check_release_after_rounds <text>: a decimal integer from 0 to MAX_RELEASE_AFTER_ROUNDS.
check_release_after_rounds() {
  [[ "$1" =~ ^[0-9]{1,3}$ ]] && (( 10#$1 <= MAX_RELEASE_AFTER_ROUNDS ))
}

# classify_submit_response <response-body-and-status> <expected-error-text>: succeeds only for a non-200
# response whose body has the expected text. The last line of the response is the HTTP status.
classify_submit_response() {
  local response="$1" expected_error_text="$2" http_status response_body
  if [[ -z "$expected_error_text" ]]; then
    echo "classify_submit_response: the expected text is empty" >&2
    return 1
  fi
  http_status="$(tail -n 1 <<<"$response")"
  if [[ ! "$http_status" =~ ^[0-9]{3}$ ]]; then
    echo "the last line of the response is not an HTTP status: $http_status" >&2
    return 1
  fi
  response_body="$(sed '$d' <<<"$response")"
  if [[ "$http_status" == "200" ]]; then
    echo "the command was accepted; expected a rejection with '$expected_error_text'" >&2
    return 1
  fi
  if ! grep -qF -- "$expected_error_text" <<<"$response_body"; then
    echo "HTTP $http_status rejection lacks '$expected_error_text': $response_body" >&2
    return 1
  fi
}

# ntt_pinned_package_id <ntt-version>: the package id that ntt_upload_dars.sh requires for that version.
# Both on wormhole-core 0.8.3. 0.3.4: NTT branch canton/overnight-ntt, commit 8976e29. 0.4.2: branch canton/migrate-ntt-layer, commit eb42001.
ntt_pinned_package_id() {
  case "$1" in
    0.3.4) echo "1c555ab22b096aa7bc780213eafbf95231bfe03bc4d669d9fe152a8eeff8293f" ;;
    0.4.2) echo "1a3e3b0e78f9f08adac111bc44110469dc271f61a9c0e5ed2f7b0b20a569feb8" ;;
    *)
      echo "no ntt package pin for version '$1'; pinned versions: 0.3.4, 0.4.2" >&2
      return 1
      ;;
  esac
}

# ntt_requires_custody_party <ntt-version>: from 0.4.2, NttGovernance and NttManager have the signatory nttCustody.
ntt_requires_custody_party() {
  [[ "$1" == "0.4.2" ]]
}
