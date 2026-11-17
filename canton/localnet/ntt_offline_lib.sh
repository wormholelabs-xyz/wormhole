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

# exercise_result <transaction-json> <choice>: the exercise result of the one exercised event of that choice.
exercise_result() {
  jq -ec --arg choice "$2" '[.events[] | .ExercisedEvent? // empty | select(.choice == $choice) | .exerciseResult]
    | if length >= 1 then .[0] else error("no \($choice) exercise") end' <<<"$1"
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
