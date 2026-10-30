#!/usr/bin/env bash
# Installs envira for this machine into ~/.local/bin:
#   curl -fsSL https://boot.controlnet.space/envira | bash
# The binary is static (musl): any x86_64 or aarch64 Linux, no dependencies. Its SHA-256 is
# checked against the release's checksum file before it is installed.
#   ENVIRA_VERSION=v0.2.0    a release tag instead of the latest release
#   ENVIRA_BASE_URL=URL      where envira-<arch>-linux and its .sha256 are (a mirror, CI)
#   ENVIRA_INSTALL_DIR=DIR   instead of ~/.local/bin
set -euo pipefail

BOLD="" RED="" GREEN="" CYAN="" RESET=""
if [ -t 2 ]; then
  BOLD="\033[1m" RED="\033[31m" GREEN="\033[32m" CYAN="\033[36m" RESET="\033[0m"
fi
info() { printf "${CYAN}${BOLD}[envira]${RESET} %s\n" "$*"; }
fail() { printf "${RED}${BOLD}[envira]${RESET} %s\n" "$*" >&2; exit 1; }

[ "$(uname -s)" = Linux ] || fail "envira runs on Linux only (this is $(uname -s))."
case "$(uname -m)" in
  x86_64 | amd64) arch=x86_64 ;;
  aarch64 | arm64) arch=aarch64 ;;
  *) fail "no envira build for $(uname -m); x86_64 and aarch64 are available." ;;
esac

repo="https://github.com/ControlNet/envira/releases"
if [ -n "${ENVIRA_BASE_URL:-}" ]; then
  base="${ENVIRA_BASE_URL%/}"
elif [ -n "${ENVIRA_VERSION:-}" ]; then
  base="$repo/download/$ENVIRA_VERSION"
else
  base="$repo/latest/download"
fi
dir="${ENVIRA_INSTALL_DIR:-$HOME/.local/bin}"
asset="envira-$arch-linux"

if command -v curl >/dev/null 2>&1; then
  fetch() { curl -fsSL --retry 3 -o "$2" "$1"; }
elif command -v wget >/dev/null 2>&1; then
  fetch() { wget -q -O "$2" "$1"; }
else
  fail "neither curl nor wget is installed."
fi
if command -v sha256sum >/dev/null 2>&1; then
  sha256() { sha256sum "$1" | cut -d' ' -f1; }
elif command -v shasum >/dev/null 2>&1; then
  sha256() { shasum -a 256 "$1" | cut -d' ' -f1; }
else
  fail "neither sha256sum nor shasum is installed, so the download cannot be checked."
fi

tmp=$(mktemp -d)
trap 'rm -rf "$tmp"' EXIT
info "Downloading $asset from $base"
fetch "$base/$asset" "$tmp/$asset" || fail "could not download $base/$asset"
fetch "$base/$asset.sha256" "$tmp/$asset.sha256" || fail "could not download $base/$asset.sha256"
want=$(cut -d' ' -f1 < "$tmp/$asset.sha256")
got=$(sha256 "$tmp/$asset")
[ -n "$want" ] && [ "$want" = "$got" ] ||
  fail "checksum mismatch for $asset (expected ${want:-nothing}, got $got); nothing was installed."

chmod 755 "$tmp/$asset"
version=$("$tmp/$asset" --version) || fail "the downloaded envira does not run on this machine."
mkdir -p "$dir"
# Replace atomically: a running envira keeps its old file.
mv -f "$tmp/$asset" "$dir/.envira.new"
mv -f "$dir/.envira.new" "$dir/envira"
printf "${GREEN}${BOLD}[envira]${RESET} %s installed to %s\n" "$version" "$dir/envira"

case ":$PATH:" in
  *":$dir:"*) info "Run: envira" ;;
  *) info "Run: $dir/envira (once it has run, new shells have $dir on PATH)" ;;
esac
