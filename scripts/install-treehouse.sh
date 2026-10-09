#!/usr/bin/env bash
# Installs the pinned treehouse release into $1 (default: $RUNNER_TEMP/treehouse-bin or a temp dir), and
# puts it on PATH for later GitHub Actions steps. Issue 259: the cowfs-treehouse sandbox tests need a real
# treehouse with `get --lease --json` (v3.1 or newer; the docs were checked against v3.1.0) and
# refuse to skip in CI, so the runner must have one.
#
# The version and every archive's sha256 are pinned here, from the release's own checksums.txt
# (https://github.com/kunchenguid/treehouse/releases/tag/v3.1.2). Never fetched unpinned, never piped to a shell.
# To bump: change VERSION and the hashes together, from that release's checksums.txt.
set -euo pipefail

VERSION=v3.1.2
case "$(uname -s)" in Linux) os=linux ;; Darwin) os=darwin ;; *) echo "unsupported OS $(uname -s)" >&2; exit 1 ;; esac
case "$(uname -m)" in x86_64 | amd64) arch=amd64 ;; arm64 | aarch64) arch=arm64 ;; *) echo "unsupported arch $(uname -m)" >&2; exit 1 ;; esac
key="$os-$arch"
# No associative array: macOS ships bash 3.2.
case "$key" in
  linux-amd64) want=bc059c6dbbcf6b11a741e92aed1d845d5b4a96f1d9c5e6f78c1aa2502669962d ;;
  linux-arm64) want=999e6f4888cfd4e6c0b97d49a4857355208bcba080aa2c59902f7fa77b940c0a ;;
  darwin-amd64) want=19a3a79c792139e36c1f5c3e9e0f9fdebe9127f1168b1debb40b4513ffed1319 ;;
  darwin-arm64) want=24666ddec346b5fd1f0467f475c647d8e2ce45e80e8a84548eade97b353cbe4b ;;
  *) echo "no pinned treehouse hash for $key" >&2; exit 1 ;;
esac

dest="${1:-${RUNNER_TEMP:-$(mktemp -d)}/treehouse-bin}"
work="$(mktemp -d)"
trap 'rm -rf "$work"' EXIT
archive="treehouse-$VERSION-$key.tar.gz"
curl --fail --silent --show-error --location --retry 3 \
  -o "$work/$archive" "https://github.com/kunchenguid/treehouse/releases/download/$VERSION/$archive"

if command -v sha256sum >/dev/null; then got=$(sha256sum "$work/$archive" | cut -d' ' -f1); else got=$(shasum -a 256 "$work/$archive" | cut -d' ' -f1); fi
[ "$got" = "$want" ] || { echo "sha256 mismatch for $archive: got $got, pinned $want" >&2; exit 1; }

mkdir -p "$dest"
tar -xzf "$work/$archive" -C "$dest" treehouse
chmod 0755 "$dest/treehouse"
[ "$("$dest/treehouse" --version)" = "$VERSION" ] || { echo "installed treehouse reports '$("$dest/treehouse" --version)', expected $VERSION" >&2; exit 1; }
echo "installed treehouse $VERSION ($key) in $dest"
if [ -n "${GITHUB_PATH:-}" ]; then echo "$dest" >> "$GITHUB_PATH"; fi
