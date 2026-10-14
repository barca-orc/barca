"""Groups are discoverable metadata, without changing materialization or caching."""
import json
import subprocess

from barca import group
from barca.api import _find_binary

BASE = '''from barca import asset, task, group
@asset
def rows():
    return [1, 2, 3]
@asset(inputs={"group": rows})
def model(group):
    return {"count": len(group)}
@task(inputs={"result": model})
def check(result):
    assert result["count"] == 3
'''
GROUPS = '''
preparation = group("Preparation", members=[rows], output=rows)
training = group("Training", members=[preparation, model, check], output=model)
experiment = group("Experiment", members=[training], output=training)
'''


def test_python_group_keeps_references():
    def node():
        return 1
    inner = group("inner", members=[node], output=node)
    outer = group("outer", members=[inner], output=inner)
    assert outer.members[0] is inner
    assert inner.output is node
    assert node() == 1


def test_groups_do_not_change_steps_or_invalidate_cache(tmp_path):
    binary = _find_binary()
    pipeline = tmp_path / "pipeline.py"
    pipeline.write_text(BASE.replace(", group", ""))

    def run(*args):
        proc = subprocess.run([binary, *args], cwd=tmp_path, text=True, capture_output=True, check=False)
        assert proc.returncode == 0, proc.stderr
        return json.loads(proc.stdout)

    flat = run("list", "pipeline.py", "--json")
    first = run("get", "model", "pipeline.py", "--json")
    assert first["steps_executed"] == 2
    pipeline.write_text(BASE + GROUPS)
    assert run("list", "pipeline.py", "--json") == flat
    metadata = run("list", "pipeline.py", "--groups", "--json")["groups"]
    assert len(metadata) == 3
    assert metadata[2]["output"] == "group:pipeline.py:training"
    assert run("get", "model", "pipeline.py", "--json")["steps_executed"] == 0
    # Editing labels and hierarchy still uses the same cached artifacts.
    pipeline.write_text(BASE + GROUPS.replace('"Experiment"', '"Renamed experiment"'))
    assert run("get", "model", "pipeline.py", "--json")["steps_executed"] == 0


def test_modeling_demo_metadata_matches_browser_fixture():
    from pathlib import Path

    root = Path(__file__).resolve().parents[2]
    proc = subprocess.run(
        [_find_binary(), "list", "modeling.py", "--groups", "--json"],
        cwd=root / "ui" / "demo", text=True, capture_output=True, check=False,
    )
    assert proc.returncode == 0, proc.stderr
    assert json.loads(proc.stdout)["groups"] == json.loads(
        (root / "ui" / "e2e" / "group-metadata.json").read_text()
    )
