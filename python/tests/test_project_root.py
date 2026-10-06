"""Project root: the directory holding barca.toml, found by walking up from the cwd.

Barca runs from the project root wherever it is invoked: `.barca/` lives there, file arguments
are read relative to the directory they were typed in, and node ids are the same from any
directory, so the cache is shared. With no barca.toml anywhere above, the cwd is the root (the
pre-0.13 behavior).
"""

import json
import subprocess
import textwrap
from pathlib import Path

from barca.api import _find_binary

PIPELINE = """
from pathlib import Path
from barca import asset


@asset()
def src() -> dict:
    return {"text": Path("data.txt").read_text().strip()}


@asset(inputs={"s": src})
def out(s: dict) -> dict:
    return {"upper": s["text"].upper()}
"""


def barca(cwd: Path, *args: str) -> subprocess.CompletedProcess:
    return subprocess.run(
        [_find_binary(), *args, "--json"], cwd=cwd, capture_output=True, text=True, check=False
    )


def result(proc: subprocess.CompletedProcess) -> dict:
    assert proc.returncode == 0, proc.stderr
    return json.loads(proc.stdout.strip().splitlines()[-1])


def project(tmp_path: Path, pipeline_at: str = "pipeline.py", toml: bool = True) -> Path:
    root = tmp_path / "proj"
    (root / "sub" / "deeper").mkdir(parents=True)
    if toml:
        (root / "barca.toml").write_text("")
    (root / "data.txt").write_text("from the root\n")
    p = root / pipeline_at
    p.parent.mkdir(parents=True, exist_ok=True)
    p.write_text(textwrap.dedent(PIPELINE))
    return root


def test_barca_dir_lands_at_the_root_when_run_from_a_subdirectory(tmp_path):
    root = project(tmp_path)
    r = result(barca(root / "sub", "get", "out", "../pipeline.py"))
    assert r["steps_executed"] == 2
    assert (root / ".barca" / "metadata.db").exists()
    assert not (root / "sub" / ".barca").exists()


def test_cache_is_shared_between_root_and_subdirectories(tmp_path):
    root = project(tmp_path)
    assert result(barca(root, "get", "out", "pipeline.py"))["steps_executed"] == 2
    assert result(barca(root / "sub", "get", "out", "../pipeline.py"))["steps_executed"] == 0
    deeper = barca(root / "sub" / "deeper", "get", "out", "../../pipeline.py")
    assert result(deeper)["steps_executed"] == 0


def test_a_pipeline_in_a_subdirectory_is_the_same_node_from_anywhere(tmp_path):
    root = project(tmp_path, pipeline_at="pipelines/p.py")
    assert result(barca(root / "pipelines", "get", "out", "p.py"))["steps_executed"] == 2
    assert result(barca(root, "get", "out", "pipelines/p.py"))["steps_executed"] == 0
    listed = json.loads(barca(root / "sub", "list", "../pipelines/p.py").stdout)
    ids = sorted(n["id"] for n in listed["nodes"])
    assert ids == ["pipelines/p.py:out", "pipelines/p.py:src"]


def test_steps_run_with_the_root_as_their_working_directory(tmp_path):
    root = project(tmp_path)
    (root / "sub" / "data.txt").write_text("from sub\n")
    r = result(barca(root / "sub", "get", "out", "../pipeline.py"))
    assert r["final_output"] == {"upper": "FROM THE ROOT"}


def test_stderr_names_the_root_only_when_it_is_not_the_cwd(tmp_path):
    root = project(tmp_path)
    from_sub = barca(root / "sub", "get", "out", "../pipeline.py")
    assert f"project root: {root.resolve()}" in from_sub.stderr
    from_root = barca(root, "get", "out", "pipeline.py")
    assert "project root" not in from_root.stderr


def test_history_from_a_subdirectory_reads_the_root_database(tmp_path):
    root = project(tmp_path)
    result(barca(root, "get", "out", "pipeline.py"))
    runs = json.loads(barca(root / "sub", "history").stdout)
    assert len(runs["runs"]) == 1


def test_without_barca_toml_the_cwd_is_the_root(tmp_path):
    root = project(tmp_path, toml=False)
    (root / "sub" / "data.txt").write_text("from sub\n")
    r = result(barca(root / "sub", "get", "out", "../pipeline.py"))
    assert r["final_output"] == {"upper": "FROM SUB"}
    assert (root / "sub" / ".barca").exists()
    assert not (root / ".barca").exists()


def test_the_nearest_barca_toml_wins(tmp_path):
    root = project(tmp_path)
    inner = root / "sub"
    (inner / "barca.toml").write_text("")
    (inner / "data.txt").write_text("from inner\n")
    r = result(barca(inner / "deeper", "get", "out", "../../pipeline.py"))
    assert r["final_output"] == {"upper": "FROM INNER"}
    assert (inner / ".barca").exists()
    assert not (root / ".barca").exists()


def test_an_invalid_barca_toml_above_the_cwd_is_a_usage_error(tmp_path):
    root = project(tmp_path)
    (root / "barca.toml").write_text("not = [valid\n")
    proc = barca(root / "sub", "get", "out", "../pipeline.py")
    assert proc.returncode == 2
    assert "barca.toml" in proc.stderr
