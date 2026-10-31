#!/usr/bin/env bash
# Usage: catalog-compat.sh <released envira binary> <catalog.toml>
# Every released envira reads the catalog published from main. Passes when that envira loads the
# catalog, or when the catalog's min_envira is newer than it (older envira then keep their cached or
# built-in catalog, with a warning). Fails when it would reject the catalog without that: a release
# and a raised min_envira have to come first.
set -euo pipefail

envira=$1
catalog=$2

released=$("$envira" --version | awk '{print $2}')
min=$(sed -n 's/^min_envira = "\(.*\)"$/\1/p' "$catalog" | head -n 1)
if [ -n "$min" ] && [ "$min" != "$released" ] &&
  [ "$(printf '%s\n%s\n' "$min" "$released" | sort -V | tail -n 1)" = "$min" ]; then
  echo "catalog needs envira >= $min; envira $released keeps its cached or built-in catalog"
  exit 0
fi

if ! "$envira" --no-remote --no-overlay --catalog "$catalog" catalog check; then
  echo "error: envira $released cannot load this catalog. Release a new envira and raise" \
    "min_envira in catalog/templates.toml to its version first." >&2
  exit 1
fi
echo "envira $released loads this catalog"
