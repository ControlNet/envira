#!/usr/bin/env bash
# Installs envira for this machine into ~/.local/bin and opens it:
#   curl -fsSL https://boot.controlnet.space/envira | bash
# Arguments after `bash -s --` are passed to envira instead, e.g. for a Dockerfile or CI:
#   curl -fsSL https://boot.controlnet.space/envira | bash -s -- install full -y
# Linux (x86_64, aarch64): a static musl binary, no dependencies. macOS: Apple Silicon. Its
# SHA-256 is checked against the release's checksum file before it is installed.
#   ENVIRA_VERSION=v0.2.0    a release tag instead of the latest release
#   ENVIRA_BASE_URL=URL      where envira-<arch>-<os> and its .sha256 are (a mirror, CI)
#   ENVIRA_INSTALL_DIR=DIR   instead of ~/.local/bin
#   ENVIRA_NO_LAUNCH=1       only install; do not open envira afterwards
set -euo pipefail

BOLD="" RED="" GREEN="" CYAN="" RESET=""
if [ -t 2 ]; then
  BOLD="\033[1m" RED="\033[31m" GREEN="\033[32m" CYAN="\033[36m" RESET="\033[0m"
fi
info() { printf "${CYAN}${BOLD}[envira]${RESET} %s\n" "$*"; }
fail() { printf "${RED}${BOLD}[envira]${RESET} %s\n" "$*" >&2; exit 1; }

# The terminal, even when stdin is this script (`curl … | bash`); none in Docker builds or CI.
has_tty() { (exec </dev/tty) 2>/dev/null; }

# Everything runs from here, called on the last line: a download cut short runs nothing.
main() {
  case "$(uname -s)" in
    Linux) os=linux ;;
    Darwin) os=macos ;;
    *) fail "envira runs on Linux and macOS (this is $(uname -s))." ;;
  esac
  case "$(uname -m)" in
    x86_64 | amd64) arch=x86_64 ;;
    aarch64 | arm64) arch=aarch64 ;;
    *) fail "no envira build for $(uname -m); x86_64 and aarch64 are available." ;;
  esac
  [ "$os-$arch" != macos-x86_64 ] || fail "no envira build for Intel Macs yet; Apple Silicon only."

  repo="https://github.com/ControlNet/envira/releases"
  if [ -n "${ENVIRA_BASE_URL:-}" ]; then
    base="${ENVIRA_BASE_URL%/}"
  elif [ -n "${ENVIRA_VERSION:-}" ]; then
    base="$repo/download/$ENVIRA_VERSION"
  else
    base="$repo/latest/download"
  fi
  dir="${ENVIRA_INSTALL_DIR:-$HOME/.local/bin}"
  asset="envira-$arch-$os"

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
  # exec skips the EXIT trap.
  rm -rf "$tmp"
  trap - EXIT

  # envira reads the keyboard from the terminal, never from this script's pipe.
  if [ $# -gt 0 ]; then
    info "Running: envira $*"
    if has_tty; then exec "$dir/envira" "$@" </dev/tty; fi
    exec "$dir/envira" "$@" </dev/null
  fi
  if [ -z "${ENVIRA_NO_LAUNCH:-}" ] && [ -t 1 ] && has_tty; then
    info "Opening envira (q quits; run it again with $dir/envira)"
    exec "$dir/envira" </dev/tty
  fi
  case ":$PATH:" in
    *":$dir:"*) info "Run: envira" ;;
    *) info "Run: $dir/envira (once it has run, new shells have $dir on PATH)" ;;
  esac
}

main "$@"
