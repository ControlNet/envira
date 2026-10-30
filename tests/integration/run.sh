#!/usr/bin/env bash
# Integration test inside throwaway containers, once with sudo and once without.
# SUITE=check (default): a few items, status, idempotence, shell integration, update and adopt.
# SUITE=full: whole bundles as the legacy scripts install them, checked with verify.sh.
# DISTRO=ubuntu (default) | fedora | arch | opensuse picks tests/integration/Dockerfile.$DISTRO.
# ENVIRA_BIN uses an existing binary (CI builds it once) instead of `cargo build --release`.
# Needs Docker and network access. Set GITHUB_TOKEN to avoid GitHub API rate limits.
set -euo pipefail
cd "$(dirname "$0")/../.."

bin=${ENVIRA_BIN:-target/release/envira}
if [ -z "${ENVIRA_BIN:-}" ]; then cargo build --release; fi
bin=$(realpath "$bin")
distro=${DISTRO:-ubuntu}
docker build -q -t "envira-it-$distro" -f "tests/integration/Dockerfile.$distro" tests/integration >/dev/null

token=()
if [ -n "${GITHUB_TOKEN:-}" ]; then token=(-e GITHUB_TOKEN); fi
for mode in ${MODES:-sudo user}; do
  docker run --rm "${token[@]}" -e ENVIRA_MODE="$mode" \
    -v "$bin:/usr/local/bin/envira:ro" \
    -v "$PWD/tests/integration/${SUITE:-check}.sh:/suite.sh:ro" \
    -v "$PWD/verify.sh:/verify.sh:ro" \
    "envira-it-$distro" bash /suite.sh
done
