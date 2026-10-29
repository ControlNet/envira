#!/usr/bin/env bash
# Runs inside the container (see run.sh). ENVIRA_MODE=sudo uses passwordless sudo;
# ENVIRA_MODE=user simulates a machine without sudo where the admin installed unzip and libatomic1.
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

echo "=== PASS (${ENVIRA_MODE:-sudo})"
