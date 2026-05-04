#!/usr/bin/env bash
set -euo pipefail

workspace="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
evidence_dir="$workspace/.sisyphus/evidence"

mkdir -p "$evidence_dir"

cargo test --test launch_parity_matrix
bash "$workspace/scripts/run-container-parity-check.sh"

ENVIRA_WORKSPACE="$workspace" python3 - <<'PY'
import json
import os
from pathlib import Path

workspace = Path(os.environ["ENVIRA_WORKSPACE"])
evidence_dir = workspace / ".sisyphus" / "evidence"
required = [
    evidence_dir / "task-11-launch-parity-catalog.json",
    evidence_dir / "task-11-launch-parity-commands.json",
    evidence_dir / "task-11-launch-parity-container-matrix.json",
]

missing = [str(path.relative_to(workspace)) for path in required if not path.exists()]
if missing:
    raise SystemExit(f"missing evidence artifacts: {', '.join(missing)}")

catalog_summary = json.loads((evidence_dir / "task-11-launch-parity-catalog.json").read_text())
command_summary = json.loads((evidence_dir / "task-11-launch-parity-commands.json").read_text())
container_summary = json.loads((evidence_dir / "task-11-launch-parity-container-matrix.json").read_text())
artifacts = [str(path.relative_to(workspace)) for path in required]
artifacts.extend(
    str(path.relative_to(workspace))
    for path in sorted((evidence_dir / "task-11-launch-parity-containers").rglob("*"))
    if path.is_file()
)
artifacts.append(str((evidence_dir / "task-11-launch-parity-summary.json").relative_to(workspace)))

summary = {
    "task": 11,
    "kind": "launch_parity_matrix_contract",
    "cargo_test": {
        "command": "cargo test --test launch_parity_matrix",
        "status": "passed",
    },
    "catalog_contract": catalog_summary,
    "command_contract": command_summary,
    "container_matrix": container_summary,
    "artifacts": artifacts,
}

(evidence_dir / "task-11-launch-parity-summary.json").write_text(
    json.dumps(summary, indent=2) + "\n"
)
PY
