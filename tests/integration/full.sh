#!/usr/bin/env bash
# Runs inside the container (see run.sh, SUITE=full): syncs whole bundles like the legacy scripts
# do (sudo: full + extras, like run.sh; user: the user bundle, like run_user.sh), checks the
# result with the legacy verify.sh, then checks that a second sync has nothing to do.
set -uo pipefail

status=0
if [ "${ENVIRA_MODE:-sudo}" = user ]; then
  # What an administrator provides on a machine without sudo.
  sudo apt-get update -qq
  sudo env DEBIAN_FRONTEND=noninteractive apt-get install -y -qq \
    git build-essential unzip libatomic1 python3-pip python3-venv >/dev/null
  export ENVIRA_PRIVILEGE=none
  bundles=(-b user -b extras)
else
  bundles=(-b full -b extras)
fi

echo "=== sync ${bundles[*]} (${ENVIRA_MODE:-sudo})"
envira sync -y -q "${bundles[@]}" || status=1

echo "=== status"
envira status
envira status --json > /tmp/status.json
failed=$(python3 -c '
import json
items = json.load(open("/tmp/status.json"))
print(" ".join(i["id"] for i in items if i["label"] in ("failed", "drifted", "missing")))')
for id in $failed; do
  echo "=== log: $id"
  envira log "$id" | tail -n 40
done
[ -z "$failed" ] || status=1

echo "=== verify.sh"
bash /verify.sh || status=1

echo "=== second sync must be a no-op"
out=$(envira sync -y -q 2>&1)
echo "$out"
grep -q "Nothing to do." <<<"$out" || status=1

exit $status
