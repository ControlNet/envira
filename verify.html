#!/usr/bin/env bash
set -u

# verify_envira_user_tools.sh
# Verify tools installed by envira/run_user.sh (user-level installer)

RED=$'\e[31m'
GRN=$'\e[32m'
YLW=$'\e[33m'
RST=$'\e[0m'

fail=0
missing=()

add_path() {
  local p="$1"
  [[ -d "$p" ]] || return 0
  case ":$PATH:" in
    *":$p:"*) ;;
    *) PATH="$p:$PATH" ;;
  esac
}

# Add common paths used by run_user.sh installs
add_path "$HOME/.local/bin"
add_path "$HOME/.cargo/bin"
add_path "$HOME/go/bin"
add_path "$HOME/.go/bin"
add_path "$HOME/.fzf/bin"
add_path "$HOME/.bun/bin"
add_path "$HOME/.opencode/bin"
add_path "$HOME/.nvim/bin"
export PATH

ok()   { printf "%sOK%s  %s\n"   "$GRN" "$RST" "$1"; }
warn() { printf "%sWARN%s %s\n"  "$YLW" "$RST" "$1"; }
bad()  { printf "%sFAIL%s %s\n"  "$RED" "$RST" "$1"; fail=$((fail+1)); missing+=("$1"); }

have_cmd() { command -v "$1" >/dev/null 2>&1; }

check_cmd() {
  local c="$1"
  if have_cmd "$c"; then
    ok "cmd: $c ($(command -v "$c"))"
  else
    bad "cmd: $c"
  fi
}

check_file() {
  local f="$1"
  if [[ -f "$f" ]]; then ok "file: $f"
  else bad "file: $f"
  fi
}

check_dir() {
  local d="$1"
  if [[ -d "$d" ]]; then ok "dir:  $d"
  else bad "dir:  $d"
  fi
}

check_contains() {
  local f="$1" pat="$2" label="$3"
  if [[ -f "$f" ]] && grep -qE "$pat" "$f"; then
    ok "$label"
  else
    bad "$label"
  fi
}

section() {
  echo
  echo "== $1 =="
}

section "Base folders"
check_dir "$HOME/.local"
check_dir "$HOME/.local/bin"

section "Shell / zsh setup"
check_dir "$HOME/.oh-my-zsh"
check_file "$HOME/.zshrc"
check_dir "$HOME/.oh-my-zsh/custom/plugins/zsh-autosuggestions"
check_dir "$HOME/.oh-my-zsh/custom/plugins/zsh-syntax-highlighting"
check_file "$HOME/.oh-my-zsh/themes/mzz-ys.zsh-theme"
check_contains "$HOME/.zshrc" 'plugins=\([^)]*zsh-autosuggestions[^)]*\)' "zshrc: plugin zsh-autosuggestions enabled"
check_contains "$HOME/.zshrc" 'plugins=\([^)]*zsh-syntax-highlighting[^)]*\)' "zshrc: plugin zsh-syntax-highlighting enabled"

section "Core dev tools"
check_cmd "git"
check_cmd "curl"
check_cmd "wget"

section "bat / neofetch / ncdu / gitkraken"
check_cmd "bat"
check_cmd "neofetch"
check_cmd "ncdu"
# gitkraken is an app; envira symlinks a launcher
check_cmd "gitkraken"

section "Conda (miniconda3)"
check_dir "$HOME/miniconda3"
check_file "$HOME/miniconda3/bin/conda"
if [[ -x "$HOME/miniconda3/bin/conda" ]]; then
  "$HOME/miniconda3/bin/conda" --version >/dev/null 2>&1 && ok "conda runs: conda --version" || bad "conda runs: conda --version"
  "$HOME/miniconda3/bin/python" --version >/dev/null 2>&1 && ok "conda python runs: python --version" || bad "conda python runs: python --version"
else
  bad "conda executable: $HOME/miniconda3/bin/conda"
fi

section "Rust toolchain"
check_cmd "rustup"
check_cmd "cargo"
check_cmd "cargo-binstall"      # installed by curl script
# cargo-cache is installed as binary "cargo-cache" providing subcommand "cargo cache"
if have_cmd "cargo-cache"; then ok "cmd: cargo-cache ($(command -v cargo-cache))"
else warn "cmd: cargo-cache not found (but 'cargo cache' might still work via subcommand)"
fi
if have_cmd "cargo"; then
  cargo cache -V >/dev/null 2>&1 && ok "subcmd: cargo cache" || warn "subcmd: cargo cache (not runnable)"
fi

section "Go toolchain"
check_file "$HOME/.go/bin/go"
if [[ -x "$HOME/.go/bin/go" ]]; then
  "$HOME/.go/bin/go" version >/dev/null 2>&1 && ok "go runs: go version" || bad "go runs: go version"
else
  bad "go executable: $HOME/.go/bin/go"
fi

section "Neovim / LunarVim / fzf"
check_dir "$HOME/.nvim"
check_cmd "nvim"
check_dir "$HOME/.fzf"
check_cmd "fzf"
check_cmd "lvim"

section "Git TUIs"
check_cmd "lazygit"
check_cmd "lazydocker"

section "Remote clipboard helper"
check_cmd "lemonade"

section "CLI utilities installed by cargo/pipx/go"
# cargo binstall / cargo install targets
check_cmd "zellij"
check_cmd "lsd"
check_cmd "delta"
check_cmd "duf"
check_cmd "dust"
check_cmd "fd"
check_cmd "rg"
check_cmd "gping"
check_cmd "procs"
check_cmd "xh"
check_cmd "genact"
check_cmd "viu"
check_cmd "rustscan"
check_cmd "btm"
check_cmd "nviwatch"

# pipx targets (run_user.sh uses pipx install ...)
check_cmd "pipx"
check_cmd "uv"
check_cmd "speedtest"
check_cmd "gdown"
check_cmd "archey"
check_cmd "tldr"
# huggingface-hub[cli,...] provides huggingface-cli and/or hf
if have_cmd "huggingface-cli"; then ok "cmd: huggingface-cli ($(command -v huggingface-cli))"
elif have_cmd "hf"; then ok "cmd: hf ($(command -v hf))"
else bad "cmd: huggingface-cli (or hf)"
fi
check_cmd "nvitop"
check_cmd "rich"

# go install targets
check_cmd "scc"
check_cmd "dive"
check_cmd "gotify"   # renamed from ~/go/bin/cli -> ~/go/bin/gotify in run_user.sh

section "pixi"
check_cmd "pixi"
check_file "$HOME/.config/pixi/config.toml"

section "superfile"
# superfile installs the 'spf' binary and writes config
check_cmd "spf"
check_file "$HOME/.config/superfile/config.toml"
check_contains "$HOME/.config/superfile/config.toml" 'auto_check_update\s*=\s*false' "superfile: auto_check_update=false"

section "yazi"
check_cmd "yazi"
check_cmd "ya"
check_file "$HOME/.config/yazi/theme.toml"
check_contains "$HOME/.config/yazi/theme.toml" '^\[flavor\]' "yazi theme: [flavor] present"
check_contains "$HOME/.config/yazi/theme.toml" '^use\s*=\s*"onedark"' "yazi theme: use=\"onedark\""

section "Node / npm / pm2 / agent CLIs"
check_cmd "fnm"
check_cmd "node"
check_cmd "npm"

# NOTE: run_user.sh sets npm prefix to '~/.local/' (tilde may not expand in npm config).
# We verify via command presence first; if missing, you likely hit the prefix issue.
check_cmd "pm2"

# @openai/codex -> usually `codex`
check_cmd "codex"

# @google/gemini-cli -> usually `gemini`
check_cmd "gemini"

# Cursor / Claude / OpenCode / Bun
# These installers may vary by distro; we only check presence.
check_cmd "agent"
check_cmd "claude"
check_cmd "opencode"
check_file "$HOME/.bun/bin/bun"
check_cmd "bun"

section "OpenCode path & Codex config"
check_dir "$HOME/.config/opencode"
check_dir "$HOME/.codex"
check_file "$HOME/.codex/config.toml"
check_contains "$HOME/.codex/config.toml" 'network_access\s*=\s*true' "codex config: network_access=true"

section "GitHub CLI"
check_cmd "gh"

echo
if [[ "$fail" -eq 0 ]]; then
  echo "${GRN}All checks passed.${RST}"
  exit 0
else
  echo "${RED}Some checks failed ($fail).${RST}"
  echo "Missing items:"
  printf ' - %s\n' "${missing[@]}"
  exit 1
fi
