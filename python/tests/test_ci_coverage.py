"""Every integration script must be run by CI.

`tests/integration/test_run_refresh.sh` (the `barca run` cache-behavior test from #125) sat in the
repo for a long time without being wired into the workflow, so a regression in it could not fail a
PR. This keeps that from happening again.
"""

from pathlib import Path

REPO = Path(__file__).resolve().parents[2]
WORKFLOW = REPO / ".depot" / "workflows" / "ci.yml"


def test_ci_runs_every_integration_script():
    if not WORKFLOW.exists():  # e.g. running from an installed sdist
        return
    scripts = sorted(p.name for p in (REPO / "tests" / "integration").glob("test_*.sh"))
    assert scripts, "expected integration scripts under tests/integration"
    ci = WORKFLOW.read_text()
    missing = [s for s in scripts if f"tests/integration/{s}" not in ci]
    assert not missing, (
        f"CI ({WORKFLOW.relative_to(REPO)}) never runs: {missing}. "
        "Add a `bash tests/integration/<script>` line to the integration-tests step."
    )
