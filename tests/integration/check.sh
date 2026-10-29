#!/usr/bin/env bash
# Runs inside the container (see run.sh). ENVIRA_MODE=sudo uses passwordless sudo;
# ENVIRA_MODE=user simulates a machine without sudo where the admin installed unzip and libatomic1.
# The last part (update) installs an older codex with npm and checks adoption and update.
set -euo pipefail

items=(bat fnm nodejs bun codex-sandbox-network)
if [ "${ENVIRA_MODE:-sudo}" = user ]; then
  sudo apt-get update -qq && sudo env DEBIAN_FRONTEND=noninteractive apt-get install -y -qq unzip libatomic1 >/dev/null
  export ENVIRA_PRIVILEGE=none
fi

echo "=== first run (${ENVIRA_MODE:-sudo})"
envira install -y -q "${items[@]}"

echo "=== status"
envira status --json > /tmp/status.json
python3 - "${ENVIRA_MODE:-sudo}" "${items[@]}" <<'PY'
import json, sys
mode, items = sys.argv[1], sys.argv[2:]
status = {i["id"]: i for i in json.load(open("/tmp/status.json"))}
bad = [i for i in items + ["codex"] if not status[i]["present"]]
assert not bad, f"not present: {bad}"
for i in items + ["codex"]:
    assert status[i]["ownership"] in (None, "managed"), (i, status[i]["ownership"])
want_bat = "apt" if mode == "sudo" else "github-release"
assert status["bat"]["via"] == want_bat, status["bat"]
print("ok: all present and managed; bat via", status["bat"]["via"])
PY

echo "=== second run must be a no-op"
out=$(envira install -y -q "${items[@]}")
echo "$out"
grep -q "Nothing to do." <<<"$out"

echo "=== shell integration"
envira env >/dev/null
envira env | grep -q "up to date"
[ "$(grep -c '^# >>> envira >>>$' ~/.zshrc)" = 1 ]
[ "$(grep -c '^# >>> envira >>>$' ~/.bashrc)" = 1 ]
zsh -ic 'command -v bun >/dev/null && command -v fnm >/dev/null && node --version && codex --version' </dev/null
grep -q '^network_access = true$' ~/.codex/config.toml

echo "=== revert the codex config and re-apply it"
envira config revert -y -q codex-sandbox-network
if grep -q network_access ~/.codex/config.toml; then echo 'revert left network_access behind'; exit 1; fi
envira config apply -y -q codex-sandbox-network
grep -q '^network_access = true$' ~/.codex/config.toml

echo "=== update: an older codex installed outside envira is adopted and updated"
# A legacy install: an older codex from npm, and a state dir that has never seen it.
export PATH="$HOME/.local/share/fnm/aliases/default/bin:$PATH"
old=$(npm view @openai/codex versions --json | python3 -c '
import json, sys
stable = [v for v in json.load(sys.stdin) if "-" not in v]
print(stable[-5])')
npm install -g --silent "@openai/codex@$old" >/dev/null
export ENVIRA_STATE_DIR=$(mktemp -d)
envira status --latest --json > /tmp/before.json
envira update --check | tee /tmp/check.txt
grep -q "codex .* → " /tmp/check.txt
envira update -y -q codex
envira status --json > /tmp/after.json
python3 - "$old" <<'PY'
import json, sys
old = sys.argv[1]
before = {i["id"]: i for i in json.load(open("/tmp/before.json"))}
after = {i["id"]: i for i in json.load(open("/tmp/after.json"))}
b, a = before["codex"], after["codex"]
assert (b["label"], b["ownership"], b["via"]) == ("outdated", "adoptable", "npm"), b
assert b["method_version"] == old, (b["method_version"], old)
assert (a["label"], a["ownership"]) == ("installed", "managed"), a
assert a["version"] != old and a["version"] == b["latest"], (a["version"], b["latest"])
print(f"ok: codex {old} adopted and updated to {a['version']}")
PY
envira update codex > /tmp/again.txt
grep -q "skip codex: up to date" /tmp/again.txt
envira adopt bun > /tmp/adopt.txt
grep -q "1 adopted" /tmp/adopt.txt
envira status --json | python3 -c '
import json, sys
s = {i["id"]: i for i in json.load(sys.stdin)}
assert s["bun"]["ownership"] == "managed", s["bun"]'

echo "=== recovery: lock, SIGTERM, retry and step timeout"
# A throwaway catalog and state, so the managed shell files of the runs above stay untouched.
export ENVIRA_STATE_DIR=$(mktemp -d) ENVIRA_CONFIG_DIR=$(mktemp -d)
cat > /tmp/recovery.toml <<'TOML'
[tools.slow]
desc = "test only"
category = "test"
detect = { cmd = "test -f \"$HOME/slow-done\"" }
install = [{ run = "echo begin; sleep ${SLOW:-600}; touch \"$HOME/slow-done\"" }]

[tools.quick]
desc = "test only"
category = "test"
detect = { cmd = "test -f \"$HOME/quick-done\"" }
install = [{ run = "touch \"$HOME/quick-done\"" }]
TOML
r=(envira --no-overlay --catalog /tmp/recovery.toml)
"${r[@]}" install -y slow quick > /tmp/first.txt 2>&1 &
pid=$!
for _ in $(seq 50); do grep -q begin /tmp/first.txt && break; sleep 0.2; done
if "${r[@]}" install -y quick > /tmp/second.txt 2>&1; then echo 'a second run got past the lock'; exit 1; fi
grep -q "another envira run is in progress" /tmp/second.txt
kill -TERM "$pid"
if wait "$pid"; then echo 'a terminated run exited 0'; exit 1; fi
running() { # a process with exactly this command line exists (the image has no procps)
  local c
  for c in /proc/[0-9]*/cmdline; do
    [ "$(tr '\0' ' ' < "$c" 2>/dev/null || true)" = "$1 " ] && return 0
  done
  return 1
}
if running 'sleep 600'; then echo 'the step outlived envira'; exit 1; fi
"${r[@]}" log | tee /tmp/log.txt
grep -Eq '^  cancelled +slow' /tmp/log.txt
grep -Eq '^  skipped +quick' /tmp/log.txt
"${r[@]}" log slow > /tmp/slow.log
grep -q '^# finished cancelled' /tmp/slow.log
SLOW=0 "${r[@]}" retry -y -q | tee /tmp/retry.txt
grep -q "2/2 succeeded" /tmp/retry.txt
rm "$HOME/slow-done"
out=$(ENVIRA_STEP_TIMEOUT=1 "${r[@]}" install -y -q slow) && { echo 'a hung step succeeded'; exit 1; }
grep -q "timed out after" <<<"$out"
unset ENVIRA_STATE_DIR ENVIRA_CONFIG_DIR

echo "=== PASS (${ENVIRA_MODE:-sudo})"
