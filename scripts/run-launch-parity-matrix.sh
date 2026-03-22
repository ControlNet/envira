#!/usr/bin/env bash
set -euo pipefail

workspace="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
evidence_dir="$workspace/.sisyphus/evidence"

mkdir -p "$evidence_dir"

cargo test --test launch_parity_matrix
"$workspace/scripts/run-container-parity-check.sh"

ENVIRA_WORKSPACE="$workspace" python3 - <<'PY'
import json
import os
from pathlib import Path

workspace = Path(os.environ["ENVIRA_WORKSPACE"])
evidence_dir = workspace / ".sisyphus" / "evidence"
required = [
    evidence_dir / "task-15-planner-verifier-install.json",
    evidence_dir / "task-15-matrix-error.txt",
    evidence_dir / "task-15-container-matrix.json",
]

missing = [str(path.relative_to(workspace)) for path in required if not path.exists()]
if missing:
    raise SystemExit(f"missing evidence artifacts: {', '.join(missing)}")

container_summary = json.loads((evidence_dir / "task-15-container-matrix.json").read_text())
success_summary = json.loads((evidence_dir / "task-15-planner-verifier-install.json").read_text())
artifacts = [
    str(path.relative_to(workspace))
    for path in sorted(evidence_dir.rglob("*"))
    if path.is_file()
]

summary = {
    "task": 15,
    "kind": "launch_catalog_parity_matrix",
    "cargo_test": {
        "command": "cargo test --test launch_parity_matrix",
        "status": "passed",
    },
    "embedded_catalog": success_summary["catalog"],
    "coverage": {
        "matrix_test": "tests/launch_parity_matrix.rs",
        "success_evidence": str((evidence_dir / "task-15-planner-verifier-install.json").relative_to(workspace)),
        "failure_regression_evidence": str((evidence_dir / "task-15-matrix-error.txt").relative_to(workspace)),
        "container_matrix": str((evidence_dir / "task-15-container-matrix.json").relative_to(workspace)),
    },
    "container_cases": container_summary["cases"],
    "artifacts": artifacts,
}

(evidence_dir / "task-15-matrix.json").write_text(json.dumps(summary, indent=2) + "\n")
PY
