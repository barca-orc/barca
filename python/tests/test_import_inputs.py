"""Cross-file inputs through ordinary imports (#202): `inputs={"m": ibp_model}` after
`from pipelines.sources import ibp_model` wires the input to that file's node, with no
`asset_ref`. A name defined in several files is never guessed.
"""

import json
import subprocess
import textwrap
from pathlib import Path

import pytest

from barca.api import _find_binary

FILES = {
    "pipelines/__init__.py": "",
    "pipelines/common.py": """
        TOLERANCE = 0.01


        def close(a, b):
            return abs(a - b) <= TOLERANCE
    """,
    "pipelines/dims.py": """
        from barca import asset


        @asset()
        def weeks() -> list:
            return [1, 2, 3]
    """,
    "pipelines/sources.py": """
        from barca import asset

        from pipelines.dims import weeks


        @asset(inputs={"w": weeks})
        def ibp_model(w: list) -> dict:
            return {str(k): 10.0 * k for k in w}
    """,
    "pipelines/reconcile.py": """
        from barca import asset

        from .sources import ibp_model


        @asset(inputs={"m": ibp_model})
        def reconciled(m: dict) -> dict:
            return {k: v + 0.001 for k, v in m.items()}
    """,
    "pipelines/validate.py": """
        from barca import task

        import pipelines.sources as src
        from pipelines.common import close
        from pipelines.reconcile import reconciled


        @task(inputs={"r": reconciled, "m": src.ibp_model})
        def validate_planning_projection(r: dict, m: dict) -> dict:
            bad = [k for k in m if not close(r[k], m[k])]
            assert not bad, f"{len(bad)} keys off"
            return {"status": "PASS", "keys": len(m)}
    """,
    # Same function name in an unrelated file: imports must pick the right one.
    "scratch/sources.py": """
        from barca import asset


        @asset()
        def ibp_model() -> dict:
            return {"decoy": -1.0}
    """,
}


def barca(cwd: Path, *args: str) -> subprocess.CompletedProcess:
    return subprocess.run(
        [_find_binary(), *args, "--json"], cwd=cwd, capture_output=True, text=True, check=False
    )


def ok(proc: subprocess.CompletedProcess) -> dict:
    assert proc.returncode == 0, proc.stderr
    out = proc.stdout.strip()
    try:
        return json.loads(out)
    except json.JSONDecodeError:
        return json.loads(out.splitlines()[-1])


@pytest.fixture
def proj(tmp_path: Path) -> Path:
    root = tmp_path / "proj"
    for rel, code in FILES.items():
        p = root / rel
        p.parent.mkdir(parents=True, exist_ok=True)
        p.write_text(textwrap.dedent(code))
    (root / "barca.toml").write_text("")
    return root


def inputs_of(listing: dict, node: str) -> set:
    (n,) = [n for n in listing["nodes"] if n["id"] == node]
    return set(n["inputs"])


def test_imports_wire_inputs_across_files(proj):
    listing = ok(barca(proj, "list"))
    assert inputs_of(listing, "pipelines/validate.py:validate_planning_projection") == {
        "pipelines/reconcile.py:reconciled",
        "pipelines/sources.py:ibp_model",
    }
    assert inputs_of(listing, "pipelines/reconcile.py:reconciled") == {
        "pipelines/sources.py:ibp_model"
    }


def test_the_task_runs_with_no_files_and_no_asset_ref(proj):
    r = ok(barca(proj, "run", "validate_planning_projection"))
    assert r["final_output"] == {"status": "PASS", "keys": 3}
    assert r["steps_executed"] == 4
    again = ok(barca(proj / "pipelines", "run", "validate_planning_projection"))
    assert again["steps_executed"] == 1


def test_editing_one_module_reruns_only_its_dependents(proj):
    ok(barca(proj, "run", "validate_planning_projection"))
    p = proj / "pipelines" / "reconcile.py"
    p.write_text(p.read_text().replace("v + 0.001", "v + 0.002"))
    dry = ok(barca(proj, "run", "validate_planning_projection", "--dry-run"))
    will_run = {s["id"] for s in dry["steps"] if s["action"] == "run"}
    assert will_run == {
        "pipelines/reconcile.py:reconciled",
        "pipelines/validate.py:validate_planning_projection",
    }


def test_a_bare_name_defined_in_two_files_is_an_error_not_a_guess(proj):
    (proj / "pipelines" / "report.py").write_text(
        textwrap.dedent(
            """
            from barca import asset


            @asset(inputs={"m": ibp_model})  # no import: which ibp_model?
            def report(m: dict) -> int:
                return len(m)
            """
        )
    )
    proc = barca(proj, "list")
    assert proc.returncode == 2
    assert "pipelines/sources.py:ibp_model" in proc.stderr
    assert "scratch/sources.py:ibp_model" in proc.stderr


def test_importing_a_helper_as_an_input_is_an_error(proj):
    (proj / "pipelines" / "bad.py").write_text(
        textwrap.dedent(
            """
            from barca import asset
            from pipelines.common import close


            @asset(inputs={"c": close})
            def bad(c) -> int:
                return 1
            """
        )
    )
    proc = barca(proj, "list")
    assert proc.returncode == 2
    assert "'close' is imported from pipelines.common" in proc.stderr
