#!/usr/bin/env bash
set -euo pipefail

workspace="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
binary_path="${ENVIRA_PARITY_BINARY:-$workspace/target/debug/envira}"
fixture_path="/workspace/tests/fixtures/launch_parity_container_catalog.toml"
evidence_root="$workspace/.sisyphus/evidence/task-11-launch-parity-containers"
verified_tool="envira-launch-parity-tool"

mkdir -p "$evidence_root"

if [[ ! -x "$binary_path" ]]; then
  cargo build --bin envira
fi

cases=(
  "ubuntu-user|ubuntu:24.04|1000:1000|alice|/tmp/envira-home"
  "fedora-user|fedora:41|1000:1000|alice|/tmp/envira-home"
)

for entry in "${cases[@]}"; do
  IFS='|' read -r case_name image user_spec username home_dir <<<"$entry"
  case_dir="$evidence_root/$case_name"
  bin_dir="$case_dir/bin"
  relative_bin_dir=".sisyphus/evidence/task-11-launch-parity-containers/$case_name/bin"
  path_env="/workspace/$relative_bin_dir:/usr/local/sbin:/usr/local/bin:/usr/sbin:/usr/bin:/sbin:/bin"

  mkdir -p "$case_dir" "$bin_dir"
  install -m 755 /bin/sh "$bin_dir/$verified_tool"

  docker pull "$image" >/dev/null

  docker run --rm \
    --user "$user_spec" \
    -e ENVIRA_CATALOG_PATH="$fixture_path" \
    -e HOME="$home_dir" \
    -e PATH="$path_env" \
    -e USER="$username" \
    -v "$workspace:/workspace" \
    -w /workspace \
    "$image" \
    /bin/sh -c "mkdir -p '$home_dir' && /workspace/target/debug/envira catalog --format json" \
    > "$case_dir/catalog.json"

  docker run --rm \
    --user "$user_spec" \
    -e ENVIRA_CATALOG_PATH="$fixture_path" \
    -e HOME="$home_dir" \
    -e PATH="$path_env" \
    -e USER="$username" \
    -v "$workspace:/workspace" \
    -w /workspace \
    "$image" \
    /bin/sh -c "mkdir -p '$home_dir' && /workspace/target/debug/envira plan --format json" \
    > "$case_dir/plan.json"

  docker run --rm \
    --user "$user_spec" \
    -e ENVIRA_CATALOG_PATH="$fixture_path" \
    -e HOME="$home_dir" \
    -e PATH="$path_env" \
    -e USER="$username" \
    -v "$workspace:/workspace" \
    -w /workspace \
    "$image" \
    /bin/sh -c "mkdir -p '$home_dir' && /workspace/target/debug/envira verify --format json" \
    > "$case_dir/verify.json"

  docker run --rm \
    --user "$user_spec" \
    -e ENVIRA_CATALOG_PATH="$fixture_path" \
    -e HOME="$home_dir" \
    -e PATH="$path_env" \
    -e USER="$username" \
    -v "$workspace:/workspace" \
    -w /workspace \
    "$image" \
    /bin/sh -c "mkdir -p '$home_dir' && /workspace/target/debug/envira install --dry-run --format json" \
    > "$case_dir/install.json"
done

ENVIRA_WORKSPACE="$workspace" python3 - <<'PY'
import json
import os
from pathlib import Path

workspace = Path(os.environ["ENVIRA_WORKSPACE"])
evidence_root = workspace / ".sisyphus" / "evidence" / "task-11-launch-parity-containers"
cases = []

for case_dir in sorted(path for path in evidence_root.iterdir() if path.is_dir()):
    catalog = json.loads((case_dir / "catalog.json").read_text())
    plan = json.loads((case_dir / "plan.json").read_text())
    verify = json.loads((case_dir / "verify.json").read_text())
    install = json.loads((case_dir / "install.json").read_text())
    default_bundles = catalog["payload"]["catalog"]["default_bundles"]
    default_bundle_id = default_bundles[0]
    default_bundle = next(
        bundle
        for bundle in catalog["payload"]["catalog"]["bundles"]
        if bundle["id"] == default_bundle_id
    )
    cases.append(
        {
            "case": case_dir.name,
            "catalog_source": "envira_catalog_path",
            "fixture": "tests/fixtures/launch_parity_container_catalog.toml",
            "catalog": {
                "required_version": catalog["payload"]["catalog"]["required_version"],
                "shell": catalog["payload"]["catalog"]["shell"],
                "default_bundles": default_bundles,
            },
            "default_bundle": {
                "id": default_bundle_id,
                "items": default_bundle["items"],
            },
            "plan": {
                "requested_selection_count": len(plan["payload"]["request"]["selections"]),
                "item_ids": [item["item_id"] for item in plan["payload"]["items"]],
                "actions": [item["action"] for item in plan["payload"]["items"]],
            },
            "verify": {
                "item_ids": [item["item_id"] for item in verify["payload"]["items"]],
                "thresholds_met": [item["threshold_met"] for item in verify["payload"]["items"]],
            },
            "install": {
                "mode": install["payload"]["install_mode"],
                "status": install["payload"]["outcome"]["status"],
                "item_ids": [item["item_id"] for item in install["payload"]["items"]],
                "actions": [item["action"] for item in install["payload"]["items"]],
                "execution_messages": [
                    step["message"]
                    for step in install["payload"]["execution"]["steps"]
                ],
            },
            "artifacts": {
                "catalog": str((case_dir / "catalog.json").relative_to(workspace)),
                "plan": str((case_dir / "plan.json").relative_to(workspace)),
                "verify": str((case_dir / "verify.json").relative_to(workspace)),
                "install": str((case_dir / "install.json").relative_to(workspace)),
            },
        }
    )

summary = {
    "task": 11,
    "kind": "launch_parity_container_contract_matrix",
    "catalog_source": "envira_catalog_path",
    "fixture": "tests/fixtures/launch_parity_container_catalog.toml",
    "cases": cases,
}

(workspace / ".sisyphus" / "evidence" / "task-11-launch-parity-container-matrix.json").write_text(
    json.dumps(summary, indent=2) + "\n"
)
PY
