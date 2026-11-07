#!/usr/bin/env bash
# Run in the release checkout before invoking makepkg. Never package an older
# archive under the current release name, or silently use a different Go tree.
set -euo pipefail
source pack/PKGBUILD
if [[ "${RELEASE_TAG:?release tag required}" != "$pkgver" ]]; then
    echo "PKGBUILD pkgver must match the release tag; update source hashes before release." >&2
    exit 1
fi
expected_wg=$(git rev-parse HEAD:libwg/wireguard-go)
if [[ "$_wgcommit" != "$expected_wg" ]]; then
    echo "PKGBUILD wireguard-go pin must match the release gitlink." >&2
    exit 1
fi
for checksum in "${sha256sums[@]}"; do
    if [[ ! "$checksum" =~ ^[0-9a-f]{64}$ ]]; then
        echo "Every package source requires a SHA-256 checksum." >&2
        exit 1
    fi
done
