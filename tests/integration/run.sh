#!/usr/bin/env bash
# Integration test: installs a few catalog items inside throwaway Ubuntu containers, once with
# sudo and once without, then checks status, idempotence and shell integration.
# Needs Docker and network access. Set GITHUB_TOKEN to avoid GitHub API rate limits.
set -euo pipefail
cd "$(dirname "$0")/../.."

cargo build --release
docker build -q -t envira-it -f tests/integration/Dockerfile.ubuntu tests/integration >/dev/null

token=()
if [ -n "${GITHUB_TOKEN:-}" ]; then token=(-e GITHUB_TOKEN); fi
for mode in ${MODES:-sudo user}; do
  docker run --rm "${token[@]}" -e ENVIRA_MODE="$mode" \
    -v "$PWD/target/release/envira:/usr/local/bin/envira:ro" \
    -v "$PWD/tests/integration/check.sh:/check.sh:ro" \
    envira-it bash /check.sh
done
