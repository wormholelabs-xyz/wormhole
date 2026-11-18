#!/usr/bin/env bash
# Table-driven test of publish_message_scan.sh. A stub curl in a temporary PATH directory serves recorded pages,
# selected by the beginExclusive offset of the request. Needs bash and jq.
# Usage: canton/tools/publish_message_scan_test.sh
set -uo pipefail

TOOLS_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
readonly TOOLS_DIR
readonly SCAN="$TOOLS_DIR/publish_message_scan.sh"
readonly PARTY="guardianGovernance::1220aa"
readonly CORE_V6="6666666666666666666666666666666666666666666666666666666666666666"
readonly CORE_V7="7777777777777777777777777777777777777777777777777777777777777777"

work_dir="$(mktemp -d)"
trap 'rm -rf "$work_dir"' EXIT
printf 'unused-token\n' > "$work_dir/token"
mkdir -p "$work_dir/bin"

# The stub logs the beginExclusive of each update request and the arguments of each call.
cat > "$work_dir/bin/curl" <<'STUB'
#!/usr/bin/env bash
set -euo pipefail
request_body="" request_url=""
while [[ $# -gt 0 ]]; do
  case "$1" in
    -d) request_body="$2"; shift 2 ;;
    -H | -X | --max-time | --connect-timeout | -o | -w) shift 2 ;;
    -*) shift ;;
    *) request_url="$1"; shift ;;
  esac
done
printf '%s\n' "$request_url" >> "$STUB_DIR/calls.log"
if [[ -n "${STUB_CURL_EXIT_STATUS:-}" ]]; then
  echo "curl: (${STUB_CURL_EXIT_STATUS}) stub failure" >&2
  exit "$STUB_CURL_EXIT_STATUS"
fi
case "$request_url" in
  */v2/updates?limit=*)
    begin_exclusive="$(jq -r .beginExclusive <<<"$request_body")"
    printf '%s\n' "$begin_exclusive" >> "$STUB_DIR/begins.log"
    if [[ -f "$STUB_DIR/page-$begin_exclusive.json" ]]; then cat "$STUB_DIR/page-$begin_exclusive.json"; else echo '[]'; fi
    ;;
  *) echo "stub: unexpected URL $request_url" >&2; exit 99 ;;
esac
STUB
chmod +x "$work_dir/bin/curl"

# update_id <number>: a 68-character update id with the multihash prefix 1220.
update_id() {
  printf '1220%064x' "$1"
}

# publish_update <offset> <update-id> <executing-package-id> <sequence>
publish_update() {
  jq -nc --argjson offset "$1" --arg id "$2" --arg template "$3:Wormhole.Core.State:Emitter" --arg sequence "$4" \
    '{update: {Transaction: {value: {offset: $offset, recordTime: "2026-11-17T00:00:00Z", updateId: $id,
      events: [{ExercisedEvent: {choice: "PublishMessage", templateId: $template, exerciseResult: {_2: {sequence: $sequence}}}}]}}}}'
}

# other_update <offset> <update-id>: a transaction without a PublishMessage.
other_update() {
  jq -nc --argjson offset "$1" --arg id "$2" \
    '{update: {Transaction: {value: {offset: $offset, recordTime: "2026-11-17T00:00:00Z", updateId: $id, events: []}}}}'
}

# write_page <case-dir> <begin-exclusive> <update-json>...
write_page() {
  local case_dir="$1" begin_exclusive="$2"
  shift 2
  jq -sc . <(printf '%s\n' "$@") > "$case_dir/page-$begin_exclusive.json"
}

failure_count=0

# run_case <case-name> <case-dir> <expected-status> <expected-stdout-line-count or "-"> <stderr-fragment or "-">
#   <scan arguments...>. Environment of the stub: STUB_CURL_EXIT_STATUS, SCAN_PAGE_SIZE.
run_case() {
  local case_name="$1" case_dir="$2" expected_status="$3" expected_line_count="$4" stderr_fragment="$5" actual_status=0
  shift 5
  STUB_DIR="$case_dir" PATH="$work_dir/bin:$PATH" "$SCAN" "$@" > "$case_dir/stdout" 2> "$case_dir/stderr" \
    || actual_status=$?
  if [[ "$actual_status" != "$expected_status" ]]; then
    echo "FAIL: $case_name: status $actual_status, expected $expected_status ($(head -c 300 "$case_dir/stderr"))" >&2
    failure_count=$((failure_count + 1))
    return
  fi
  if [[ "$expected_line_count" != "-" && "$(grep -c . "$case_dir/stdout")" != "$expected_line_count" ]]; then
    echo "FAIL: $case_name: $(grep -c . "$case_dir/stdout") output lines, expected $expected_line_count" >&2
    failure_count=$((failure_count + 1))
    return
  fi
  if [[ "$stderr_fragment" != "-" ]] && ! grep -qF -- "$stderr_fragment" "$case_dir/stderr"; then
    echo "FAIL: $case_name: stderr '$(head -c 300 "$case_dir/stderr")' lacks '$stderr_fragment'" >&2
    failure_count=$((failure_count + 1))
    return
  fi
  echo "ok: $case_name"
}

new_case_dir() {
  mkdir -p "$work_dir/$1"
  echo "$work_dir/$1"
}

readonly URL="http://participant.invalid"
control_id="$(update_id 100)"

# S1: one short page with the control.
case_dir="$(new_case_dir s1)"
write_page "$case_dir" 10 "$(publish_update 11 "$(update_id 1)" "$CORE_V6" 1)" "$(other_update 12 "$(update_id 2)")" \
  "$(publish_update 13 "$control_id" "$CORE_V7" 2)"
SCAN_PAGE_SIZE=200 run_case "S1 one short page with the control" "$case_dir" 0 2 - \
  "$URL" "$work_dir/token" "$PARTY" 10 13 "$control_id"

# S2: a full page, then a short page with the control. The second request begins at the last offset of page 1.
case_dir="$(new_case_dir s2)"
write_page "$case_dir" 10 "$(publish_update 11 "$(update_id 1)" "$CORE_V6" 1)" "$(other_update 12 "$(update_id 2)")"
write_page "$case_dir" 12 "$(publish_update 15 "$control_id" "$CORE_V7" 2)"
SCAN_PAGE_SIZE=2 run_case "S2 two pages, control on page 2" "$case_dir" 0 2 - \
  "$URL" "$work_dir/token" "$PARTY" 10 15 "$control_id"
touch "$case_dir/begins.log"
if [[ "$(paste -sd, - < "$case_dir/begins.log")" == "10,12" ]]; then
  echo "ok: S2 requests begin at 10, then at 12"
else
  echo "FAIL: S2 requests begin at '$(paste -sd, - < "$case_dir/begins.log")', expected 10,12" >&2
  failure_count=$((failure_count + 1))
fi

# S3: the response ends early without the control.
case_dir="$(new_case_dir s3)"
write_page "$case_dir" 10 "$(publish_update 11 "$(update_id 1)" "$CORE_V6" 1)"
SCAN_PAGE_SIZE=200 run_case "S3 short page without the control" "$case_dir" 1 - "without the required update $control_id" \
  "$URL" "$work_dir/token" "$PARTY" 10 20 "$control_id"

# S4: the required update id is mandatory.
case_dir="$(new_case_dir s4)"
write_page "$case_dir" 10 "$(publish_update 11 "$(update_id 1)" "$CORE_V6" 1)"
SCAN_PAGE_SIZE=200 run_case "S4 no required id" "$case_dir" 2 0 "usage" \
  "$URL" "$work_dir/token" "$PARTY" 10 20
touch "$case_dir/calls.log"
if [[ ! -s "$case_dir/calls.log" ]]; then
  echo "ok: S4 makes no request"
else
  echo "FAIL: S4 made requests: $(paste -sd, - < "$case_dir/calls.log")" >&2
  failure_count=$((failure_count + 1))
fi

# S5: an error body instead of a page.
case_dir="$(new_case_dir s5)"
echo '{"code": "NOT_FOUND", "cause": "offset pruned"}' > "$case_dir/page-10.json"
SCAN_PAGE_SIZE=200 run_case "S5 error body" "$case_dir" 1 - "update read after offset 10" \
  "$URL" "$work_dir/token" "$PARTY" 10 20 "$control_id"

# S6: more updates than the page size.
case_dir="$(new_case_dir s6)"
write_page "$case_dir" 10 "$(other_update 11 "$(update_id 1)")" "$(other_update 12 "$(update_id 2)")" \
  "$(other_update 13 "$(update_id 3)")"
SCAN_PAGE_SIZE=2 run_case "S6 page longer than the page size" "$case_dir" 1 - "returned 3 updates for a page of 2" \
  "$URL" "$work_dir/token" "$PARTY" 10 20 "$control_id"

# S7: a full page whose last offset is after the end offset.
case_dir="$(new_case_dir s7)"
write_page "$case_dir" 10 "$(other_update 11 "$(update_id 1)")" "$(other_update 25 "$(update_id 2)")"
SCAN_PAGE_SIZE=2 run_case "S7 last offset outside the range" "$case_dir" 1 - "outside (10, 20]" \
  "$URL" "$work_dir/token" "$PARTY" 10 20 "$control_id"

# S8: curl times out.
case_dir="$(new_case_dir s8)"
STUB_CURL_EXIT_STATUS=28 SCAN_PAGE_SIZE=200 run_case "S8 curl timeout" "$case_dir" 1 - "curl status 28" \
  "$URL" "$work_dir/token" "$PARTY" 10 20 "$control_id"


# S9: argument errors.
case_dir="$(new_case_dir s9)"
run_case "S9a required id with a character outside [0-9a-f]" "$case_dir" 2 - "required update id" \
  "$URL" "$work_dir/token" "$PARTY" 10 20 "1220$(printf 'g%.0s' {1..64})"
run_case "S9b required id with uppercase hex" "$case_dir" 2 - "required update id" \
  "$URL" "$work_dir/token" "$PARTY" 10 20 "1220$(printf 'A%.0s' {1..64})"
run_case "S9c required id of 66 characters" "$case_dir" 2 - "required update id" \
  "$URL" "$work_dir/token" "$PARTY" 10 20 "${control_id:0:66}"
run_case "S9d required id without the 1220 prefix" "$case_dir" 2 - "required update id" \
  "$URL" "$work_dir/token" "$PARTY" 10 20 "1221${control_id:4}"
run_case "S9e required id with an empty end offset" "$case_dir" 2 - "end offset" \
  "$URL" "$work_dir/token" "$PARTY" 10 "" "$control_id"
run_case "S9f seven arguments" "$case_dir" 2 - "usage" \
  "$URL" "$work_dir/token" "$PARTY" 10 20 "$control_id" extra
SCAN_CURL_MAX_SECONDS=0 run_case "S9g SCAN_CURL_MAX_SECONDS zero" "$case_dir" 2 - "SCAN_CURL_MAX_SECONDS" \
  "$URL" "$work_dir/token" "$PARTY" 10 20 "$control_id"
run_case "S9h four arguments" "$case_dir" 2 - "usage" \
  "$URL" "$work_dir/token" "$PARTY" 10
run_case "S9i begin offset equal to the end offset" "$case_dir" 2 - "begin offset 13 is not before end offset 13" \
  "$URL" "$work_dir/token" "$PARTY" 13 13 "$control_id"
run_case "S9j end offset with a leading zero" "$case_dir" 2 - "end offset is not a non-negative integer" \
  "$URL" "$work_dir/token" "$PARTY" 10 013 "$control_id"

# S10: the control is in the output, but the end offset is after the control. A short page after the control would
# then pass unseen.
case_dir="$(new_case_dir s10)"
write_page "$case_dir" 10 "$(publish_update 11 "$(update_id 1)" "$CORE_V6" 1)" "$(publish_update 13 "$control_id" "$CORE_V7" 2)"
SCAN_PAGE_SIZE=200 run_case "S10 end offset after the control" "$case_dir" 1 - "required update $control_id is at offsets [13], not at the end offset 20" \
  "$URL" "$work_dir/token" "$PARTY" 10 20 "$control_id"

# S11: the end offset is before the control offset that a full page reports. The page check stops the scan.
case_dir="$(new_case_dir s11)"
write_page "$case_dir" 10 "$(publish_update 11 "$(update_id 1)" "$CORE_V6" 1)" "$(publish_update 13 "$control_id" "$CORE_V7" 2)"
SCAN_PAGE_SIZE=2 run_case "S11 end offset before the control" "$case_dir" 1 - "outside (10, 12]" \
  "$URL" "$work_dir/token" "$PARTY" 10 12 "$control_id"

if [[ "$failure_count" -ne 0 ]]; then
  echo "publish_message_scan_test.sh: $failure_count failures" >&2
  exit 1
fi
echo "publish_message_scan_test.sh: PASS"
