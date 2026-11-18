#!/usr/bin/env bash
# Copy the Canton console jar out of the canton image that images.lock pins by digest, then check its SHA-256.
# Usage: canton/localnet/console_jar_from_image.sh
# Writes CANTON_CONSOLE_JAR (env.sh; CANTON_VENDOR_DIR moves it). Needs Docker. Pulls the image by digest only.
set -euo pipefail
# shellcheck source=env.sh
source "$(dirname "${BASH_SOURCE[0]}")/env.sh"

readonly CANTON_IMAGE_REPOSITORY="ghcr.io/digital-asset/decentralized-canton-sync/docker/canton"
readonly IMAGES_LOCK="$LOCALNET_SCRIPT_DIR/images.lock"

if [[ -f "$CANTON_CONSOLE_JAR" ]]; then
  echo "$CANTON_CONSOLE_JAR exists. Remove it to copy it again." >&2
  exit 1
fi
canton_image_digests="$(awk -F'\t' -v repository="$CANTON_IMAGE_REPOSITORY" '$1 == repository {print $3}' "$IMAGES_LOCK")"
if [[ ! "$canton_image_digests" =~ ^sha256:[0-9a-f]{64}$ ]]; then
  echo "expected exactly one sha256 digest of $CANTON_IMAGE_REPOSITORY in $IMAGES_LOCK, found: $canton_image_digests" >&2
  exit 1
fi
readonly CANTON_IMAGE_REFERENCE="$CANTON_IMAGE_REPOSITORY@$canton_image_digests"

docker pull --quiet "$CANTON_IMAGE_REFERENCE" >/dev/null
container_id="$(docker create "$CANTON_IMAGE_REFERENCE")"
jar_directory="$(dirname "$CANTON_CONSOLE_JAR")"
mkdir -p "$jar_directory"
temporary_jar="$(mktemp "$jar_directory/console-jar.XXXXXX")"
trap 'docker rm -f "$container_id" >/dev/null; rm -f "$temporary_jar"' EXIT
docker cp "$container_id:/app/lib/$(basename "$CANTON_CONSOLE_JAR")" "$temporary_jar"
copied_jar_sha256="$(openssl dgst -sha256 -r "$temporary_jar" | cut -d' ' -f1)"
if [[ "$copied_jar_sha256" != "$CANTON_CONSOLE_JAR_SHA256" ]]; then
  echo "console jar SHA-256 $copied_jar_sha256, expected $CANTON_CONSOLE_JAR_SHA256" >&2
  exit 1
fi
mv "$temporary_jar" "$CANTON_CONSOLE_JAR"
echo "Copied $CANTON_CONSOLE_JAR from $CANTON_IMAGE_REFERENCE (SHA-256 $copied_jar_sha256)"
