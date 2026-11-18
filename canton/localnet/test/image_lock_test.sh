#!/usr/bin/env bash
# Table-driven test of check_compose_images_are_locked (image_lock_lib.sh). Needs no Docker.
# Usage: canton/localnet/test/image_lock_test.sh
set -uo pipefail
LOCALNET_TEST_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
readonly LOCALNET_TEST_DIR
# shellcheck source=SCRIPTDIR/../image_lock_lib.sh
source "$LOCALNET_TEST_DIR/../image_lock_lib.sh"

readonly IMAGES_LOCK="$LOCALNET_TEST_DIR/../images.lock"
readonly OTHER_DIGEST="sha256:0000000000000000000000000000000000000000000000000000000000000000"
failure_count=0
work_dir="$(mktemp -d)"
trap 'rm -rf "$work_dir"' EXIT

# The images that compose resolves when every image is locked: one per images.lock line.
awk '!/^#/ && NF {print $1 ":" $2 "@" $3}' "$IMAGES_LOCK" | sort -u > "$work_dir/locked-images"
locked_image_count="$(wc -l < "$work_dir/locked-images" | tr -d ' ')"
if [[ "$locked_image_count" != 8 ]]; then
  echo "FAIL: images.lock lists $locked_image_count images; this test expects the 8 LocalNet images" >&2
  exit 1
fi
locked_postgres_image="$(grep '^postgres:' "$work_dir/locked-images")"

# write_fixture <name> <transform>: the locked images with one transform applied, as a resolved-images file.
write_fixture() {
  case "$2" in
    unchanged) cat "$work_dir/locked-images" ;;
    postgres-tag-only) grep -v '^postgres:' "$work_dir/locked-images"; echo "postgres:17" ;;
    postgres-other-digest) grep -v '^postgres:' "$work_dir/locked-images"; echo "${locked_postgres_image%@*}@$OTHER_DIGEST" ;;
    postgres-short-digest) grep -v '^postgres:' "$work_dir/locked-images"; echo "${locked_postgres_image%@*}@sha256:abc" ;;
    without-postgres) grep -v '^postgres:' "$work_dir/locked-images" ;;
    empty) : ;;
  esac > "$work_dir/$1"
}

# Columns: case | resolved-images transform | images.lock variant | expected status | output fragment ("-" for none)
readonly IMAGE_LOCK_CASES=(
  "all locked|unchanged|current|0|-"
  "tag only, no digest|postgres-tag-only|current|1|postgres:17 has no sha256 digest"
  "digest not in images.lock|postgres-other-digest|current|1|postgres:18.6-trixie@$OTHER_DIGEST is not in images.lock"
  "lock line not used|without-postgres|current|1|images.lock lists 8 images, compose resolves 7"
  "bad digest length|postgres-short-digest|current|1|postgres:18.6-trixie@sha256:abc is not <repository>:<tag>@sha256:<64 hex>"
  "empty resolved list|empty|current|1|compose resolves no images"
  "malformed images.lock line|unchanged|two-columns|1|malformed images.lock line"
)

for image_lock_case in "${IMAGE_LOCK_CASES[@]}"; do
  IFS='|' read -r case_name resolved_transform lock_variant expected_status output_fragment <<<"$image_lock_case"
  write_fixture resolved-images "$resolved_transform"
  case "$lock_variant" in
    current) cp "$IMAGES_LOCK" "$work_dir/images.lock" ;;
    two-columns) { cat "$IMAGES_LOCK"; printf 'postgres\t17\n'; } > "$work_dir/images.lock" ;;
  esac
  actual_status=0
  check_compose_images_are_locked "$work_dir/resolved-images" "$work_dir/images.lock" > "$work_dir/output" 2>&1 \
    || actual_status=$?
  case_failed=false
  if [[ "$actual_status" != "$expected_status" ]]; then
    echo "FAIL: $case_name: status $actual_status, expected $expected_status" >&2
    case_failed=true
  fi
  if [[ "$output_fragment" != "-" ]] && ! grep -qF -- "$output_fragment" "$work_dir/output"; then
    echo "FAIL: $case_name: output does not contain: $output_fragment" >&2
    cat "$work_dir/output" >&2
    case_failed=true
  fi
  if [[ "$case_failed" == true ]]; then
    failure_count=$((failure_count + 1))
  else
    echo "PASS: $case_name (status $actual_status)"
  fi
done

if ((failure_count > 0)); then
  echo "$failure_count case(s) failed" >&2
  exit 1
fi
echo "All ${#IMAGE_LOCK_CASES[@]} cases passed"
