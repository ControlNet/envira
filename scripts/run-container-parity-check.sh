#!/usr/bin/env bash
set -euo pipefail

workspace="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
binary_path="${ENVIRA_PARITY_BINARY:-$workspace/target/debug/envira}"
evidence_root="$workspace/.sisyphus/evidence/task-15-containers"
selection_bundle="terminal-tools"

mkdir -p "$evidence_root"

if [[ ! -x "$binary_path" ]]; then
  cargo build --bin envira
fi

cases=(
  "ubuntu-root|ubuntu:24.04|0:0|root|/root"
  "ubuntu-user|ubuntu:24.04|1000:1000|alice|/tmp/envira-home"
  "fedora-root|fedora:41|0:0|root|/root"
  "fedora-user|fedora:41|1000:1000|alice|/tmp/envira-home"
)

for entry in "${cases[@]}"; do
  IFS='|' read -r case_name image user_spec username home_dir <<<"$entry"
  case_dir="$evidence_root/$case_name"
  mkdir -p "$case_dir"

  docker pull "$image" >/dev/null

  docker run --rm \
    --user "$user_spec" \
    -e HOME="$home_dir" \
    -e USER="$username" \
    -v "$workspace:/workspace" \
    -w /workspace \
    "$image" \
    /bin/sh -c "mkdir -p '$home_dir' && /workspace/target/debug/envira catalog --format json" \
    > "$case_dir/catalog.json"

  docker run --rm \
    --user "$user_spec" \
    -e HOME="$home_dir" \
    -e USER="$username" \
    -v "$workspace:/workspace" \
    -w /workspace \
    "$image" \
    /bin/sh -c "mkdir -p '$home_dir' && /workspace/target/debug/envira plan --bundle $selection_bundle --format json" \
    > "$case_dir/plan.json"

  docker run --rm \
    --user "$user_spec" \
    -e HOME="$home_dir" \
    -e USER="$username" \
    -v "$workspace:/workspace" \
    -w /workspace \
    "$image" \
    /bin/sh -c "mkdir -p '$home_dir' && /workspace/target/debug/envira verify --bundle $selection_bundle --format json || true" \
    > "$case_dir/verify.json"

  docker run --rm \
    --user "$user_spec" \
    -e HOME="$home_dir" \
    -e USER="$username" \
    -v "$workspace:/workspace" \
    -w /workspace \
    "$image" \
    /bin/sh -c "mkdir -p '$home_dir' && /workspace/target/debug/envira install --bundle $selection_bundle --dry-run --format json" \
    > "$case_dir/install.json"
done

ENVIRA_WORKSPACE="$workspace" python3 - <<'PY'
import json
import os
from pathlib import Path

workspace = Path(os.environ["ENVIRA_WORKSPACE"])
evidence_root = workspace / ".sisyphus" / "evidence" / "task-15-containers"
cases = []

for case_dir in sorted(path for path in evidence_root.iterdir() if path.is_dir()):
    catalog = json.loads((case_dir / "catalog.json").read_text())
    plan = json.loads((case_dir / "plan.json").read_text())
    verify = json.loads((case_dir / "verify.json").read_text())
    install = json.loads((case_dir / "install.json").read_text())
    selected_bundle = next(
        bundle for bundle in catalog["payload"]["catalog"]["bundles"] if bundle["id"] == "terminal-tools"
    )
    cases.append(
        {
            "case": case_dir.name,
            "distro": verify["payload"]["verification"]["platform"]["distro"],
            "runtime_scope": verify["payload"]["verification"]["platform"]["runtime_scope"],
            "manifest_source": "embedded",
            "catalog_items": len(catalog["payload"]["catalog"]["items"]),
            "catalog_bundles": len(catalog["payload"]["catalog"]["bundles"]),
            "default_bundles": catalog["payload"]["catalog"]["default_bundles"],
            "selection": {
                "bundle": "terminal-tools",
                "items": selected_bundle["items"],
            },
            "plan_actions": [
                step["action"] for step in plan["payload"]["action_plan"]["steps"]
            ],
            "verification_summary": verify["payload"]["verification"]["summary"],
            "failing_items": [
                result["step"]["item_id"]
                for result in verify["payload"]["verification"]["results"]
                if not result["result"]["threshold_met"]
            ],
            "install_status": install["payload"]["install"]["outcome"]["status"],
            "artifacts": {
                "catalog": str((case_dir / "catalog.json").relative_to(workspace)),
                "plan": str((case_dir / "plan.json").relative_to(workspace)),
                "verify": str((case_dir / "verify.json").relative_to(workspace)),
                "install": str((case_dir / "install.json").relative_to(workspace)),
            },
        }
    )

summary = {
    "task": 15,
    "kind": "launch_catalog_container_matrix",
    "manifest_source": "embedded",
    "selection": {
        "bundle": "terminal-tools",
        "install_mode": "dry_run",
    },
    "cases": cases,
}

(workspace / ".sisyphus" / "evidence" / "task-15-container-matrix.json").write_text(
    json.dumps(summary, indent=2) + "\n"
)
PY
