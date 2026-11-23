#!/usr/bin/env bash
# Puts the signed archives' hashes into one target's dist manifest.
#
#   use-signed-hashes.sh <manifest> <hashes>
#
# <hashes> is a JSON object of archive names and SHA-256 hashes. A build that
# starts after another has finished copies that one's hashes into its own
# manifest, and dist merges all the manifests in no fixed order. So an unsigned
# archive's hash left in any of them could end up in the installers.
set -euo pipefail

manifest=$1
hashes=$2

updated=$(jq --argjson hashes "$hashes" '
  reduce ($hashes | to_entries[]) as $signed (.;
    if .artifacts[$signed.key].checksums.sha256 then
      .artifacts[$signed.key].checksums.sha256 = $signed.value
    else . end)' "$manifest")
printf '%s\n' "$updated" > "$manifest"
