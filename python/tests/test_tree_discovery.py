"""Tree discovery: with no file arguments barca finds every @asset/@task/@sensor under the
project root. File and directory arguments narrow the scope. Node ids are root-relative.
"""

import json
import subprocess
import textwrap
from pathlib import Path

import pytest

from barca.api import _find_binary

SOURCES = """
from barca import asset


@asset()
def ibp_model() -> dict:
    return {"rows": 3}
"""

RECONCILE = """
from barca import asset, asset_ref


@asset(inputs={"m": asset_ref("pipelines/sources.py:ibp_model")})
def reconciled(m: dict) -> dict:
    return {"rows": m["rows"], "ok": True}
"""

VALIDATE = """
from barca import task, asset_ref


@task(inputs={"r": asset_ref("pipelines/reconcile.py:reconciled")})
def validate(r: dict) -> dict:
    assert r["ok"]
    return {"status": "PASS"}
"""

DECOY = """
from barca import asset


@asset()
def decoy() -> int:
    return 1
"""


def barca(cwd: Path, *args: str) -> subprocess.CompletedProcess:
    return subprocess.run(
        [_find_binary(), *args, "--json"], cwd=cwd, capture_output=True, text=True, check=False
    )


def ok(proc: subprocess.CompletedProcess) -> dict:
    assert proc.returncode == 0, proc.stderr
    out = proc.stdout.strip()
    try:
        return json.loads(out)  # list/status print one indented document
    except json.JSONDecodeError:
        return json.loads(out.splitlines()[-1])  # get/run: the result is the last line


def write(root: Path, rel: str, code: str) -> None:
    p = root / rel
    p.parent.mkdir(parents=True, exist_ok=True)
    p.write_text(textwrap.dedent(code))


@pytest.fixture
def proj(tmp_path: Path) -> Path:
    root = tmp_path / "proj"
    root.mkdir()
    (root / "barca.toml").write_text("")
    write(root, "pipelines/sources.py", SOURCES)
    write(root, "pipelines/reconcile.py", RECONCILE)
    write(root, "pipelines/validate.py", VALIDATE)
    write(root, "pipelines/helpers.py", "def double(x):\n    return 2 * x\n")
    # Never discovered by default:
    write(root, "tests/test_pipeline.py", DECOY)
    write(root, "test_top.py", DECOY)
    write(root, "conftest.py", DECOY)
    write(root, ".venv/lib/site.py", DECOY)
    write(root, "build/gen.py", DECOY)
    write(root, "node_modules/x/y.py", DECOY)
    write(root, "pkg/__pycache__/z.py", DECOY)
    return root


def ids(listing: dict) -> list[str]:
    return sorted(n["id"] for n in listing["nodes"])


ALL = [
    "pipelines/reconcile.py:reconciled",
    "pipelines/sources.py:ibp_model",
    "pipelines/validate.py:validate",
]


def test_list_with_no_files_discovers_the_whole_tree(proj):
    assert ids(ok(barca(proj, "list"))) == ALL


def test_discovery_from_a_subdirectory_sees_the_whole_project(proj):
    assert ids(ok(barca(proj / "pipelines", "list"))) == ALL


def test_list_reports_the_root(proj):
    listing = ok(barca(proj / "pipelines", "list"))
    assert Path(listing["root"]).resolve() == proj.resolve()


def test_run_a_task_with_no_files(proj):
    r = ok(barca(proj, "run", "validate"))
    assert r["final_output"] == {"status": "PASS"}
    assert r["steps_executed"] == 3
    again = ok(barca(proj / "pipelines", "run", "validate"))
    assert again["steps_executed"] == 1  # the task always runs; its upstream is cached


def test_get_with_no_arguments_materializes_every_asset(proj):
    r = ok(barca(proj, "get"))
    assert {s["id"] for s in r["steps"]} == {
        "pipelines/sources.py:ibp_model",
        "pipelines/reconcile.py:reconciled",
    }


def test_explicit_files_still_scope_discovery(proj):
    listing = ok(barca(proj, "list", "pipelines/sources.py"))
    assert ids(listing) == ["pipelines/sources.py:ibp_model"]


def test_absolute_file_arguments_get_root_relative_ids(proj):
    listing = ok(barca(proj, "list", str(proj / "pipelines" / "sources.py")))
    assert ids(listing) == ["pipelines/sources.py:ibp_model"]


def test_a_directory_argument_means_every_file_under_it(proj):
    write(proj, "other/extra.py", DECOY.replace("decoy", "extra"))
    assert ids(ok(barca(proj, "list", "pipelines/"))) == ALL
    assert ids(ok(barca(proj, "list", "other"))) == ["other/extra.py:extra"]
    r = ok(barca(proj, "run", "validate", "pipelines/"))
    assert r["final_output"] == {"status": "PASS"}


def test_a_directory_named_like_a_target_needs_a_trailing_slash(proj):
    # `reconciled` is a target, not the directory `reconciled/`, unless written `reconciled/`.
    write(proj, "reconciled/x.py", DECOY.replace("decoy", "x"))
    r = ok(barca(proj, "get", "reconciled"))
    assert r["final_output"] == {"rows": 3, "ok": True}
    assert ids(ok(barca(proj, "list", "reconciled/"))) == ["reconciled/x.py:x"]


def test_dot_means_the_current_directory(proj):
    assert ids(ok(barca(proj / "pipelines", "list", "."))) == ALL


def test_discovery_exclude_and_include(proj):
    write(proj, "scratch/probe.py", DECOY.replace("decoy", "probe"))
    assert "scratch/probe.py:probe" in ids(ok(barca(proj, "list")))
    (proj / "barca.toml").write_text('[discovery]\nexclude = ["scratch/**"]\n')
    assert ids(ok(barca(proj, "list"))) == ALL
    (proj / "barca.toml").write_text('[discovery]\ninclude = ["pipelines/sources.py"]\n')
    assert ids(ok(barca(proj, "list"))) == ["pipelines/sources.py:ibp_model"]


def test_unknown_discovery_key_is_a_usage_error(proj):
    (proj / "barca.toml").write_text('[discovery]\nexclud = ["x"]\n')
    proc = barca(proj, "list")
    assert proc.returncode == 2
    assert "exclud" in proc.stderr


def test_files_without_barca_are_not_nodes_and_syntax_errors_in_them_are_ignored(proj):
    write(proj, "notes/broken.py", "def (:\n")
    assert ids(ok(barca(proj, "list"))) == ALL


def test_a_syntax_error_in_a_barca_file_names_the_file(proj):
    write(proj, "pipelines/bad.py", "from barca import asset\n\n@asset()\ndef (:\n")
    proc = barca(proj, "list")
    assert proc.returncode == 2
    assert "pipelines/bad.py" in proc.stderr


def test_same_function_name_in_two_files_needs_the_full_id(proj):
    write(proj, "a/dup.py", DECOY.replace("decoy", "twin"))
    write(proj, "b/dup.py", DECOY.replace("decoy", "twin").replace("return 1", "return 2"))
    proc = barca(proj, "get", "twin")
    assert proc.returncode == 2
    assert "a/dup.py:twin" in proc.stderr and "b/dup.py:twin" in proc.stderr
    assert ok(barca(proj, "get", "b/dup.py:twin"))["final_output"] == 2
    assert ok(barca(proj, "get", "a/dup.py:twin"))["final_output"] == 1


SAME_STEM = """
from barca import asset
from {helper} import factor


@asset()
def {name}() -> int:
    return factor()
"""


def test_same_file_name_in_two_directories_hashes_and_runs_each_separately(proj):
    # Two `assets.py` in different directories are different modules. (Helper modules still
    # need distinct names: every file's directory is on one sys.path, as in any Python process.)
    write(proj, "east/assets.py", SAME_STEM.format(name="east_total", helper="east_helpers"))
    write(proj, "east/east_helpers.py", "def factor():\n    return 10\n")
    write(proj, "west/assets.py", SAME_STEM.format(name="west_total", helper="west_helpers"))
    write(proj, "west/west_helpers.py", "def factor():\n    return 100\n")
    assert ok(barca(proj, "get", "east_total"))["final_output"] == 10
    assert ok(barca(proj, "get", "west_total"))["final_output"] == 100

    # Editing west's helper invalidates west only.
    write(proj, "west/west_helpers.py", "def factor():\n    return 1000\n")
    assert ok(barca(proj, "get", "east_total"))["steps_executed"] == 0
    west = ok(barca(proj, "get", "west_total"))
    assert (west["steps_executed"], west["final_output"]) == (1, 1000)


def test_two_same_named_files_run_in_one_worker(proj):
    write(proj, "east/assets.py", SAME_STEM.format(name="east_total", helper="east_helpers"))
    write(proj, "east/east_helpers.py", "def factor():\n    return 10\n")
    write(proj, "west/assets.py", SAME_STEM.format(name="west_total", helper="west_helpers"))
    write(proj, "west/west_helpers.py", "def factor():\n    return 100\n")
    write(
        proj,
        "both.py",
        """
        from barca import asset, asset_ref


        @asset(inputs={"e": asset_ref("east/assets.py:east_total"),
                       "w": asset_ref("west/assets.py:west_total")})
        def both(e: int, w: int) -> list:
            return [e, w]
        """,
    )
    assert ok(barca(proj, "get", "both"))["final_output"] == [10, 100]


def test_status_with_no_files(proj):
    ok(barca(proj, "get"))
    st = ok(barca(proj, "status"))
    assert Path(st["root"]).resolve() == proj.resolve()
    states = {n["id"]: n["cache"]["state"] for n in st["nodes"]}
    assert states["pipelines/sources.py:ibp_model"] == "cached"


def test_without_barca_toml_the_cwd_is_walked(tmp_path):
    write(tmp_path, "p/a.py", DECOY)
    assert ids(ok(barca(tmp_path / "p", "list"))) == ["a.py:decoy"]


def test_an_empty_project_is_a_usage_error_that_says_so(tmp_path):
    (tmp_path / "barca.toml").write_text("")
    proc = barca(tmp_path, "list")
    assert proc.returncode == 2
    assert "no @asset" in proc.stderr
