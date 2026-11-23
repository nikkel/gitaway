#!/usr/bin/env bash
# Signs and notarizes the program inside a macOS release archive, then rebuilds
# the archive and everything that records its hash.
#
#   sign-macos-archive.sh <folder> <target>
#
# <folder> holds what the release's build job for <target> uploaded: the
# archive, its .sha256 file and dist's manifest for the target. dist makes the
# installers and sha256.sum from that manifest.
#
# Environment:
#   SIGNING_IDENTITY   the certificate's SHA-1 hash, or "-" to sign without a
#                      certificate and skip notarization (local tests only)
#   SIGNING_KEYCHAIN   the keychain holding it (not needed with "-")
#   NOTARY_KEY_FILE, NOTARY_KEY_ID, NOTARY_ISSUER_ID   App Store Connect API key
#
# It never changes the default keychain or the keychain search list.
set -euo pipefail

folder=$1
target=$2
name="gitaway-$target"
archive="$name.tar.xz"
manifest="$target-dist-manifest.json"

work=$(mktemp -d)
trap 'rm -rf "$work"' EXIT

cd "$folder"
shasum -a 256 -c "$archive.sha256"
# -p: keep the files' permissions whatever the caller's umask is.
tar -xpJf "$archive" -C "$work"
program="$work/$name/gitaway"
[[ -f $program ]] || { echo "no gitaway in $archive" >&2; exit 1; }

# Permissions users give the program, such as reading its Keychain item, belong
# to this identifier and the certificate's team: changing it makes macOS ask again.
options=(--force --identifier io.github.nikkel.gitaway --options runtime)
if [[ $SIGNING_IDENTITY == - ]]; then
  codesign "${options[@]}" --sign - "$program"
else
  codesign "${options[@]}" --timestamp \
    --sign "$SIGNING_IDENTITY" --keychain "$SIGNING_KEYCHAIN" "$program"

  # A program on its own can't carry its notarization ticket, so nothing is
  # added to it: Apple records its hash, and macOS looks that up.
  ditto -c -k "$program" "$work/notarize.zip"
  notary=(--key "$NOTARY_KEY_FILE" --key-id "$NOTARY_KEY_ID" --issuer "$NOTARY_ISSUER_ID")
  xcrun notarytool submit "$work/notarize.zip" "${notary[@]}" \
    --wait --timeout 30m --output-format json > "$work/notary.json" || true
  status=$(jq -r .status "$work/notary.json")
  echo "notarization of $name: $status"
  if [[ $status != Accepted ]]; then
    xcrun notarytool log "$(jq -r .id "$work/notary.json")" "${notary[@]}" \
      | jq -r '.issues[]?.message' >&2 || true
    exit 1
  fi
fi
codesign --verify --strict "$program"

# Keep macOS file attributes and the runner's user out of the archive.
COPYFILE_DISABLE=1 tar --no-xattrs --uid 0 --gid 0 --uname "" --gname "" \
  -cJf "$work/$archive" -C "$work" "$name"
mv "$work/$archive" "$archive"

hash=$(shasum -a 256 "$archive" | cut -d ' ' -f 1)
echo "$hash *$archive" > "$archive.sha256"
jq --arg archive "$archive" --arg hash "$hash" '
  if .artifacts[$archive].checksums.sha256 then
    .artifacts[$archive].checksums.sha256 = $hash
  else
    error("no checksum for \($archive) in the manifest")
  end' "$manifest" > "$work/$manifest"
mv "$work/$manifest" "$manifest"
