#!/usr/bin/env bash
# Runs on a GitHub macOS runner itself: macOS has no containers, but every job gets a fresh VM.
# The image is not a clean Mac (Homebrew and many tools are preinstalled), so the tools to
# install are picked from those it lacks, and preinstalled ones test adoption. envira runs with a
# bare PATH, as on a new Mac where /opt/homebrew/bin is not on PATH yet.
# ENVIRA_BIN: the binary to test (default target/release/envira).
set -euo pipefail
cd "$(dirname "$0")/../.."

envira=$(cd "$(dirname "${ENVIRA_BIN:-target/release/envira}")" && pwd)/$(basename "${ENVIRA_BIN:-envira}")
tmp=$(mktemp -d)
export ENVIRA_STATE_DIR=$tmp/state ENVIRA_CONFIG_DIR=$tmp/config ENVIRA_CACHE_DIR=$tmp/cache
export ENVIRA_NO_REMOTE=1
bare=/usr/bin:/bin:/usr/sbin:/sbin
e() { env PATH="$bare" "$envira" "$@"; }

echo "=== machine"
{ e doctor || true; } | head -n 12
echo "preinstalled formulae: $(brew list --formula | wc -l | tr -d ' ')"

echo "=== status with a bare PATH"
start=$(date +%s)
e status --json > "$tmp/status.json"
echo "status took $(($(date +%s) - start))s"
# Tools from the catalog that the image lacks (installed through Homebrew), and one tool that
# Homebrew installed outside envira (adoptable).
read -r -a picked < <(python3 - "$tmp/status.json" <<'PY'
import json, sys
st = {i["id"]: i for i in json.load(open(sys.argv[1]))}
assert st["homebrew"]["present"], st["homebrew"]
for i in ("docker", "libatomic", "default-shell"):
    assert st[i]["label"] == "not_applicable", (i, st[i]["label"], st[i]["reason"])
want = ["lazygit", "dust", "duf", "zoxide", "gping", "procs", "xh", "viu", "scc", "lsd", "fd", "bat"]
missing = [i for i in want if not st[i]["present"]][:4]
assert len(missing) >= 2, f"the image has almost everything: {missing}"
print(" ".join(missing))
PY
)
echo "picked: ${picked[*]}"
adopt=$(python3 - "$tmp/status.json" <<'PY'
import json, sys
st = json.load(open(sys.argv[1]))
brew = [i["id"] for i in st if i["ownership"] == "adoptable" and i["via"] == "brew"]
print(brew[0] if brew else "")
PY
)
echo "adoptable through brew: ${adopt:-none}"

echo "=== install"
e install -y "${picked[@]}" codex-sandbox-network

echo "=== status"
e status --json > "$tmp/status.json"
python3 - "$tmp/status.json" "${picked[@]}" <<'PY'
import json, sys
st = {i["id"]: i for i in json.load(open(sys.argv[1]))}
for i in sys.argv[2:]:
    assert st[i]["present"] and st[i]["ownership"] == "managed", st[i]
    assert st[i]["via"] == "brew", (i, st[i]["via"])
assert st["codex-sandbox-network"]["present"], st["codex-sandbox-network"]
print("ok: installed through Homebrew and managed")
PY

echo "=== second run must be a no-op"
out=$(e install -y "${picked[@]}" codex-sandbox-network)
echo "$out"
grep -q "Nothing to do." <<<"$out"

echo "=== Linux-only items are not planned"
e plan docker 2>&1 | tee "$tmp/plan.txt"
grep -q "os:linux" "$tmp/plan.txt"

echo "=== shell integration (zsh and the bash login profile)"
grep -q "# >>> envira >>>" "$HOME/.zshrc"
grep -q "# >>> envira >>>" "$HOME/.bash_profile"
bin=$(e status --json | python3 -c 'import json,sys; print({i["id"]: i for i in json.load(sys.stdin)}[sys.argv[1]]["path"])' "${picked[0]}")
found=$(env -i HOME="$HOME" PATH="$bare" zsh -c ". \"$ENVIRA_CONFIG_DIR/env.sh\" && command -v $(basename "$bin")")
echo "a new shell finds ${picked[0]} at $found"

if [ -n "$adopt" ]; then
  echo "=== adopt $adopt (installed by Homebrew outside envira)"
  e adopt "$adopt"
  e status --json | python3 -c '
import json, sys
st = {i["id"]: i for i in json.load(sys.stdin)}
i = st[sys.argv[1]]
assert i["ownership"] == "managed" and i["via"] == "brew", i
print("ok:", sys.argv[1], i["version"], "managed via brew")' "$adopt"
fi

echo "=== revert and re-apply a config"
e config revert -y codex-sandbox-network
e config apply -y codex-sandbox-network
echo "=== PASS (macOS)"
