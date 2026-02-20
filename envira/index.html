#!/usr/bin/env bash
set -euo pipefail

###─── COLORS & STYLES ─────────────────────────────────────────────────────────
RESET="\e[0m"
BOLD="\e[1m"
UNDERLINE="\e[4m"

RED="\e[31m"
GREEN="\e[32m"
YELLOW="\e[33m"
BLUE="\e[34m"
MAGENTA="\e[35m"
CYAN="\e[36m"

info()    { echo -e "${CYAN}${BOLD}[INFO]${RESET}    $*"; }
success() { echo -e "${GREEN}${BOLD}[SUCCESS]${RESET} $*"; }
warn()    { echo -e "${YELLOW}${BOLD}[WARN]${RESET}    $*"; }
error()   { echo -e "${RED}${BOLD}[ERROR]${RESET}   $*"; exit 1; }

###─── SPINNER ─────────────────────────────────────────────────────────────────
spinner() {
  local pid=$1
  local delay=0.1
  local spinstr='|/-\'
  while kill -0 "$pid" 2>/dev/null; do
    for c in $spinstr; do
      printf "\r${MAGENTA}%s${RESET} Downloading... %c" "${BOLD}" "$c"
      sleep $delay
    done
  done
  printf "\r"
}

###─── COLORFUL ENVIRA HEADER ───────────────────────────────────────────────────
cat << EOF

███████╗███╗   ██╗██╗   ██╗██╗██████╗  █████╗ 
██╔════╝████╗  ██║██║   ██║██║██╔══██╗██╔══██╗
█████╗  ██╔██╗ ██║██║   ██║██║██████╔╝███████║
██╔══╝  ██║╚██╗██║╚██╗ ██╔╝██║██╔══██╗██╔══██║
███████╗██║ ╚████║ ╚████╔╝ ██║██║  ██║██║  ██║
╚══════╝╚═╝  ╚═══╝  ╚═══╝  ╚═╝╚═╝  ╚═╝╚═╝  ╚═╝

EOF
info "Welcome to the envira Auto-Installer!"

###─── SETTINGS ────────────────────────────────────────────────────────────────
URL="https://boot.controlnet.space/files/envira"
BIN_DIR="$HOME/.local/bin"
OUTPUT="$BIN_DIR/envira"

###─── PREPARE ─────────────────────────────────────────────────────────────────
info "Ensuring target directory exists: ${UNDERLINE}$BIN_DIR${RESET}"
mkdir -p "$BIN_DIR"

###─── FIND DOWNLOADER ─────────────────────────────────────────────────────────
if command -v curl &>/dev/null; then
  DOWNLOADER="curl -fsSL"
  PROGRESS_ARGS="-#"
elif command -v wget &>/dev/null; then
  DOWNLOADER="wget -qO-"
  PROGRESS_ARGS="--show-progress --progress=bar:force:noscroll"
else
  error "Neither curl nor wget found. Please install one and retry."
fi

###─── DOWNLOAD ────────────────────────────────────────────────────────────────
info "Fetching Envira from $URL"
(
  if [[ $DOWNLOADER == curl* ]]; then
    curl $PROGRESS_ARGS -L "$URL" -o "$OUTPUT"
  else
    wget $PROGRESS_ARGS -O "$OUTPUT" "$URL"
  fi
) & spinner $!

chmod +x "$OUTPUT"
success "Downloaded and installed to ${UNDERLINE}$OUTPUT${RESET}"
info "Run ${BOLD}envira --help${RESET} to get started."
