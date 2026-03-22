#!/usr/bin/env bash
set -euo pipefail

if [[ -t 2 ]]; then
  RESET="\e[0m"
  BOLD="\e[1m"
  RED="\e[31m"
  GREEN="\e[32m"
  CYAN="\e[36m"
else
  RESET=""
  BOLD=""
  RED=""
  GREEN=""
  CYAN=""
fi

info() {
  printf '%b\n' "${CYAN}${BOLD}[INFO]${RESET} $*" >&2
}

success() {
  printf '%b\n' "${GREEN}${BOLD}[OK]${RESET} $*" >&2
}

fail() {
  printf '%b\n' "${RED}${BOLD}[ERROR]${RESET} $*" >&2
  exit 1
}

usage() {
  cat >&2 <<'EOF'
envira.sh - thin bootstrap wrapper for the envira binary

Usage:
  bash envira.sh
  bash envira.sh --run -- <envira-args...>

Trust contract:
  - download exactly two release-surface assets from the published files endpoint:
      * envira
      * envira.sha256
  - envira.sha256 must contain a single sha256 entry for the filename "envira"
  - the downloaded binary must match that checksum before install or exec handoff
EOF
}

download_to_file() {
  local url=$1
  local destination=$2

  if command -v curl >/dev/null 2>&1; then
    curl --fail --location --silent --show-error "$url" --output "$destination"
    return
  fi

  if command -v wget >/dev/null 2>&1; then
    wget --quiet --output-document="$destination" "$url"
    return
  fi

  fail "Neither curl nor wget is installed; cannot download envira."
}

sha256_file() {
  local path=$1

  if command -v sha256sum >/dev/null 2>&1; then
    local output
    output=$(sha256sum "$path")
    printf '%s\n' "${output%% *}"
    return
  fi

  if command -v shasum >/dev/null 2>&1; then
    local output
    output=$(shasum -a 256 "$path")
    printf '%s\n' "${output%% *}"
    return
  fi

  if command -v openssl >/dev/null 2>&1; then
    local output
    output=$(openssl dgst -sha256 "$path")
    printf '%s\n' "${output##*= }"
    return
  fi

  fail "No sha256 tool found; install sha256sum, shasum, or openssl."
}

expected_checksum_from_manifest() {
  local manifest=$1
  local line
  local checksum=""
  local filename=""

  while IFS= read -r line || [[ -n "$line" ]]; do
    line=${line%$'\r'}
    [[ -z "$line" ]] && continue
    [[ "$line" == \#* ]] && continue

    if [[ ! "$line" =~ ^([[:xdigit:]]{64})[[:space:]]+\*?([^[:space:]]+)$ ]]; then
      fail "Checksum manifest at $CHECKSUM_URL is malformed."
    fi

    if [[ -n "$checksum" ]]; then
      fail "Checksum manifest at $CHECKSUM_URL must contain exactly one asset entry."
    fi

    checksum=${BASH_REMATCH[1],,}
    filename=${BASH_REMATCH[2]}
  done < "$manifest"

  [[ -n "$checksum" ]] || fail "Checksum manifest at $CHECKSUM_URL did not contain a checksum entry."
  [[ "$filename" == "$ASSET_NAME" ]] || fail "Checksum manifest expected asset '$ASSET_NAME' but found '$filename'."

  printf '%s\n' "$checksum"
}

ASSET_NAME="envira"
DEFAULT_BASE_URL="https://boot.controlnet.space/files"
BASE_URL="${ENVIRA_BOOTSTRAP_BASE_URL:-$DEFAULT_BASE_URL}"
BASE_URL="${BASE_URL%/}"
BINARY_URL="${ENVIRA_BOOTSTRAP_BINARY_URL:-$BASE_URL/$ASSET_NAME}"
CHECKSUM_URL="${ENVIRA_BOOTSTRAP_CHECKSUM_URL:-$BASE_URL/$ASSET_NAME.sha256}"
BIN_DIR="${ENVIRA_BOOTSTRAP_INSTALL_DIR:-$HOME/.local/bin}"
OUTPUT="$BIN_DIR/$ASSET_NAME"
RUN_AFTER_DOWNLOAD=false

while (($# > 0)); do
  case "$1" in
    --help|-h)
      usage
      exit 0
      ;;
    --run)
      RUN_AFTER_DOWNLOAD=true
      shift
      if (($# > 0)) && [[ "$1" == "--" ]]; then
        shift
      fi
      break
      ;;
    --)
      shift
      break
      ;;
    *)
      fail "Unknown option: $1"
      ;;
  esac
done

RUN_ARGS=("$@")

if [[ "$RUN_AFTER_DOWNLOAD" == false && ${#RUN_ARGS[@]} -gt 0 ]]; then
  fail "Command arguments require --run, for example: bash envira.sh --run -- catalog --format json"
fi

TMP_DIR=$(mktemp -d)
TMP_BINARY="$TMP_DIR/$ASSET_NAME"
TMP_CHECKSUM="$TMP_DIR/$ASSET_NAME.sha256"

cleanup() {
  rm -rf "$TMP_DIR"
}

trap cleanup EXIT

info "Bootstrapping envira into $OUTPUT"
mkdir -p "$BIN_DIR"

info "Downloading $BINARY_URL"
download_to_file "$BINARY_URL" "$TMP_BINARY"

info "Downloading checksum manifest $CHECKSUM_URL"
download_to_file "$CHECKSUM_URL" "$TMP_CHECKSUM"

info "Verifying release checksum"
EXPECTED_CHECKSUM=$(expected_checksum_from_manifest "$TMP_CHECKSUM")
ACTUAL_CHECKSUM=$(sha256_file "$TMP_BINARY")

if [[ "$ACTUAL_CHECKSUM" != "$EXPECTED_CHECKSUM" ]]; then
  fail "Integrity check failed for $ASSET_NAME: expected $EXPECTED_CHECKSUM but downloaded $ACTUAL_CHECKSUM."
fi

chmod 0755 "$TMP_BINARY"
mv "$TMP_BINARY" "$OUTPUT"
success "Installed $ASSET_NAME to $OUTPUT"

if [[ "$RUN_AFTER_DOWNLOAD" == true ]]; then
  info "Handing off to $OUTPUT"
  trap - EXIT
  cleanup
  exec "$OUTPUT" "${RUN_ARGS[@]}"
fi

info "Run $OUTPUT --help to get started."
