#!/usr/bin/env bash
# Integration test inside throwaway Ubuntu containers, once with sudo and once without.
# SUITE=check (default): a few items, status, idempotence, shell integration, update and adopt.
# SUITE=full: whole bundles as the legacy scripts install them, checked with verify.sh.
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
    -v "$PWD/tests/integration/${SUITE:-check}.sh:/suite.sh:ro" \
    -v "$PWD/verify.sh:/verify.sh:ro" \
    envira-it bash /suite.sh
done
