"""Populate the UI demo through Barca's CLI; rerun to add fresh history."""

import os
import subprocess
from pathlib import Path

ROOT = Path(__file__).resolve().parents[2]
DEMO = Path(__file__).resolve().parent
BARCA = os.environ.get("BARCA_BIN", str(ROOT / "target/debug/barca"))
ENV = {**os.environ, "PYTHONPATH": str(ROOT / "python")}


def run(command: str, target: str, *, fails: bool = False) -> None:
    result = subprocess.run([BARCA, command, target, "commerce.py", "--json"],
                            cwd=DEMO, env=ENV, capture_output=True, text=True, check=False)
    expected = 1 if fails else 0
    if result.returncode != expected:
        raise RuntimeError(f"{command} {target}: {result.stdout}\n{result.stderr}")
    print(f"{command} {target}: {'demo failure recorded' if fails else 'ok'}")


if __name__ == "__main__":
    run("get", "revenue_by_product,stock_alerts")
    for _ in range(8):
        run("run", "publish_dashboard")
    for _ in range(3):
        run("run", "validate_orders")
        run("run", "notify_stock_team")
    for _ in range(2):
        run("run", "export_partner_feed", fails=True)
    run("get", "inventory_quality_check", fails=True)
    # Materialize a different assumption, then restore the source so the UI
    # shows the cached forecast as stale. Never edit the metadata database.
    source = DEMO / "commerce.py"
    original = source.read_text()
    try:
        source.write_text(original.replace('"growth_rate": 0.08', '"growth_rate": 0.12'))
        run("get", "revenue_forecast")
    finally:
        source.write_text(original)
    print("Demo ready: cached, stale, failed, never-run and unknown states.")
