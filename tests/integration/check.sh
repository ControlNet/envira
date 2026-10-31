#!/usr/bin/env bash
# Runs inside the container (see run.sh). ENVIRA_MODE=sudo uses passwordless sudo;
# ENVIRA_MODE=user simulates a machine without sudo where the admin installed unzip and libatomic1.
# The last part (update) installs an older codex with npm and checks adoption and update.
set -euo pipefail

# Installs packages as the administrator would, with the distro's package manager.
admin_install() {
  if command -v apt-get >/dev/null; then
    sudo apt-get update -qq && sudo env DEBIAN_FRONTEND=noninteractive apt-get install -y -qq "$@" >/dev/null
  elif command -v dnf >/dev/null; then
    sudo dnf install -y -q "$@" >/dev/null
  elif command -v pacman >/dev/null; then
    sudo pacman -Syu --noconfirm --needed "$@" >/dev/null
  elif command -v zypper >/dev/null; then
    sudo zypper --non-interactive --quiet install "$@" >/dev/null
  fi
}

items=(bat fnm nodejs bun codex-sandbox-network)
if [ "${ENVIRA_MODE:-sudo}" = user ]; then
  # unzip, and the library Node.js 25+ links against (the catalog's `libatomic` per distro).
  . /etc/os-release
  case " $ID ${ID_LIKE:-} " in
    *" debian "*|*" ubuntu "*|*" suse "*|*" opensuse "*) admin_install unzip libatomic1 ;;
    *" fedora "*) admin_install unzip libatomic ;;
    *" arch "*) admin_install unzip gcc-libs ;;
  esac
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
# With sudo the distro package, without it the release download.
via = status["bat"]["via"]
assert (via != "github-release") if mode == "sudo" else (via == "github-release"), status["bat"]
print("ok: all present and managed; bat via", status["bat"]["via"])
PY

echo "=== a closed pipe ends a read-only command quietly"
envira doctor 2>/tmp/pipe.err | head -n 1
if grep -q panicked /tmp/pipe.err; then cat /tmp/pipe.err; exit 1; fi

echo "=== second run must be a no-op"
out=$(envira install -y -q "${items[@]}")
echo "$out"
grep -q "Nothing to do." <<<"$out"

if [ "${ENVIRA_MODE:-sudo}" = sudo ] && command -v apt-get >/dev/null; then
  echo "=== apt lists: refreshed at most once a day"
  # bat's install above refreshed the image's empty lists.
  sudo test -f /var/lib/envira/apt-refreshed
  sudo envira apt-refresh | tee /tmp/apt.txt
  grep -q "not refreshing them" /tmp/apt.txt
  # Two days old: the first apt package refreshes them, the second does not.
  sudo touch -d '2 days ago' /var/lib/envira/apt-refreshed
  envira install -y -q screen tmux
  { envira log screen; envira log tmux; } > /tmp/apt.txt
  [ "$(grep -c 'refreshed 2 days ago: apt-get update$' /tmp/apt.txt)" = 1 ]
  [ "$(grep -c 'not refreshing them$' /tmp/apt.txt)" = 1 ]
  # A source that does not resolve: the refresh warns, nothing fails.
  echo 'deb http://bogus.invalid/ubuntu noble main' | sudo tee /etc/apt/sources.list.d/bogus.list >/dev/null
  sudo envira apt-refresh | tee /tmp/apt.txt
  grep -q "sources changed since the last refresh: apt-get update" /tmp/apt.txt
  sudo rm /etc/apt/sources.list.d/bogus.list
fi

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

echo "=== published catalog: download, cache, fallback"
mkdir -p /tmp/site
envira catalog show > /tmp/site/catalog.toml
python3 -m http.server -d /tmp/site -b 127.0.0.1 8765 >/dev/null 2>&1 &
web=$!
for _ in $(seq 50); do [ -n "$(curl -fsI http://127.0.0.1:8765/catalog.toml 2>/dev/null)" ] && break; sleep 0.2; done
remote() { env -u ENVIRA_NO_REMOTE ENVIRA_CACHE_DIR=/tmp/cache ENVIRA_CATALOG_URL="$1" envira catalog check; }
remote http://127.0.0.1:8765/catalog.toml | tee /tmp/remote.txt
grep -q "remote http://127.0.0.1:8765/catalog.toml (updated now)" /tmp/remote.txt
remote http://127.0.0.1:8765/catalog.toml | grep -q "(cached just now)"
# The cached file is the download, byte for byte (coreutils only: cmp is missing on some images).
[ "$(sha256sum < /tmp/site/catalog.toml)" = "$(sha256sum < /tmp/cache/catalog.toml)" ]
kill "$web"
# Unreachable and no cache: the embedded catalog, within the 3 s timeout.
rm -rf /tmp/cache
start=$(date +%s)
remote http://127.0.0.1:9/catalog.toml | tee /tmp/remote.txt
grep -q "^warning: could not fetch http://127.0.0.1:9/catalog.toml" /tmp/remote.txt
grep -q "^embedded" /tmp/remote.txt
[ $(( $(date +%s) - start )) -le 5 ]

echo "=== recovery: lock, SIGTERM, retry and step timeout"
# A throwaway catalog and state, so the managed shell files of the runs above stay untouched.
export ENVIRA_STATE_DIR=$(mktemp -d) ENVIRA_CONFIG_DIR=$(mktemp -d)
cat > /tmp/recovery.toml <<'TOML'
[tools.slow]
desc = "test only"
category = "test"
detect = { cmd = "test -f \"$HOME/slow-done\"" }
install = [{ run = "echo begin; sleep ${SLOW:-600}; touch \"$HOME/slow-done\"" }]

[tools.flaky]
desc = "test only"
category = "test"
detect = { cmd = "test -f \"$HOME/flaky-done\"" }
install = [{ run = "if [ ! -f \"$HOME/flaky-once\" ]; then touch \"$HOME/flaky-once\"; echo 'curl: (56) Recv failure: Connection reset by peer'; exit 56; fi; touch \"$HOME/flaky-done\"" }]

[tools.quick]
desc = "test only"
category = "test"
detect = { cmd = "test -f \"$HOME/quick-done\"" }
install = [{ run = "touch \"$HOME/quick-done\"" }]
TOML
r=(envira --no-overlay --catalog /tmp/recovery.toml)
# One at a time, so quick is still waiting when the run is terminated.
"${r[@]}" install -y -j 1 slow quick > /tmp/first.txt 2>&1 &
pid=$!
# The step's own output line, not the plan, which prints the command (and its "begin") first.
for _ in $(seq 50); do grep -q '│ begin$' /tmp/first.txt && break; sleep 0.2; done
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
# A network error is retried automatically and the run succeeds.
"${r[@]}" install -y flaky > /tmp/flaky.txt
grep -q "hint (network)" /tmp/flaky.txt
grep -q "retrying in 5.0s (attempt 2/3)" /tmp/flaky.txt
grep -q "1/1 succeeded" /tmp/flaky.txt
# Units run side by side: quick finishes while slow is still running.
rm "$HOME/quick-done"
SLOW=3 "${r[@]}" install -y slow quick > /tmp/parallel.txt
cat /tmp/parallel.txt
grep -q "│ slow │ begin$" /tmp/parallel.txt
[ "$(grep -n '✓ quick' /tmp/parallel.txt | cut -d: -f1)" -lt "$(grep -n '✓ slow' /tmp/parallel.txt | cut -d: -f1)" ]
"${r[@]}" log --list > /tmp/runs.txt
cat /tmp/runs.txt
[ "$(grep -c ' finished ' /tmp/runs.txt)" -ge 4 ]
unset ENVIRA_STATE_DIR ENVIRA_CONFIG_DIR

echo "=== PASS (${ENVIRA_MODE:-sudo})"
