"""Materialize the modeling demo, check caching, and run all validation tasks."""

import json
import os
import subprocess
from pathlib import Path

ROOT = Path(__file__).resolve().parents[2]
DEMO = Path(__file__).resolve().parent
BARCA = os.environ.get("BARCA_BIN", str(ROOT / "target/debug/barca"))
ENV = {**os.environ, "PYTHONPATH": str(ROOT / "python")}


def command(*args: str) -> dict:
    result = subprocess.run(
        [BARCA, *args, "--json"], cwd=DEMO, env=ENV, capture_output=True, text=True, check=False
    )
    if result.returncode:
        raise RuntimeError(f"{' '.join(args)} failed:\n{result.stdout}\n{result.stderr}")
    return json.loads(result.stdout)


if __name__ == "__main__":
    discovered = command("list", "modeling.py", "--all")
    nodes = discovered["nodes"]
    assets = {node["id"]: node for node in nodes if node["kind"] == "asset"}
    tasks = {node["id"]: node for node in nodes if node["kind"] == "task"}
    assert len(nodes) == 152 and len(assets) == len(tasks) == 76
    for node_id in assets:
        name = node_id.split(":", 1)[1]
        validator = tasks[f"modeling.py:validate__{name}"]
        assert node_id in validator["inputs"], f"Missing corresponding validator for {name}"

    materialized = command("get", "modeling.py")
    cached = command("get", "modeling.py")
    assert cached["steps_executed"] == 0, "Identical second get should reuse every cached asset"
    print(
        f"152 nodes: 76 assets + 76 validation tasks; {materialized['steps_executed']} assets materialized"
    )
    print("Second identical get: 0 executed steps (all assets cached)")

    # Give each hierarchy branch its own run in the UI's history.
    groups = [
        "data__",
        *(f"cv__fold_{fold:02d}__" for fold in range(1, 6)),
        "cv__summary__",
        "train__",
        "test__",
        "release__",
    ]
    checked = 0
    for group in groups:
        names = sorted(
            node_id.split(":", 1)[1]
            for node_id in tasks
            if node_id.split(":", 1)[1].startswith(f"validate__{group}")
        )
        result = command("run", ",".join(names), "modeling.py")
        assert result["status"] == "success", result
        assert result["steps_executed"] == len(names), (
            "Only validators should execute; their inputs are cached"
        )
        checked += len(names)
        print(f"{group}: {len(names)} validation tasks passed")
    assert checked == 76
    print("Modeling demo ready in the UI: select modeling.py in the pipeline sidebar.")
