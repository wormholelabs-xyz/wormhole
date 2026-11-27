# check_compose_images_are_locked for start.sh. Source this file; do not run it.
# shellcheck shell=bash

readonly LOCKED_IMAGE_PATTERN='^([a-z0-9][a-z0-9._/-]*):([A-Za-z0-9_][A-Za-z0-9._-]*)@(sha256:[0-9a-f]{64})$'

# check_compose_images_are_locked <resolved-images-file> <images.lock>: each image that compose resolves has the
# form <repository>:<tag>@sha256:<64 hex> and its "repository tag digest" line is in images.lock, and images.lock
# lists no other image. Prints each violation and returns 1 on any.
check_compose_images_are_locked() {
  local resolved_images_file="$1" images_lock_file="$2" resolved_image lock_line violation_count=0
  local resolved_image_count locked_image_count
  local locked_images=""
  local -a lock_fields
  while IFS= read -r lock_line || [[ -n "$lock_line" ]]; do
    [[ -z "$lock_line" || "$lock_line" == \#* ]] && continue
    IFS=$'\t' read -ra lock_fields <<<"$lock_line"
    if [[ "${#lock_fields[@]}" -ne 3 || ! "${lock_fields[2]}" =~ ^sha256:[0-9a-f]{64}$ ]]; then
      echo "malformed images.lock line (expected repository<TAB>tag<TAB>sha256:<64 hex>): $lock_line" >&2
      violation_count=$((violation_count + 1))
      continue
    fi
    locked_images+="${lock_fields[0]}:${lock_fields[1]}@${lock_fields[2]}"$'\n'
  done < "$images_lock_file"
  locked_image_count="$(printf '%s' "$locked_images" | sort -u | grep -c . || true)"

  resolved_image_count="$(sort -u "$resolved_images_file" | grep -c . || true)"
  if [[ "$resolved_image_count" -eq 0 ]]; then
    echo "compose resolves no images" >&2
    return 1
  fi
  while IFS= read -r resolved_image; do
    [[ -z "$resolved_image" ]] && continue
    if [[ "$resolved_image" != *@sha256:* ]]; then
      echo "$resolved_image has no sha256 digest" >&2
      violation_count=$((violation_count + 1))
    elif [[ ! "$resolved_image" =~ $LOCKED_IMAGE_PATTERN ]]; then
      echo "$resolved_image is not <repository>:<tag>@sha256:<64 hex>" >&2
      violation_count=$((violation_count + 1))
    elif ! grep -qxF -- "$resolved_image" <<<"$locked_images"; then
      echo "$resolved_image is not in images.lock" >&2
      violation_count=$((violation_count + 1))
    fi
  done < <(sort -u "$resolved_images_file")
  if [[ "$resolved_image_count" -ne "$locked_image_count" ]]; then
    echo "images.lock lists $locked_image_count images, compose resolves $resolved_image_count" >&2
    violation_count=$((violation_count + 1))
  fi
  ((violation_count == 0))
}
