"""A directory where an artifact file belongs (#249), end to end through the binary.

The rule (`barca docs cache`, "A directory at an artifact's path"):

- An artifact is one file. A directory at its path is not an artifact, so the cached result
  counts as missing, exactly like a deleted file: it is fetched from the store, or computed
  again, when something needs to read it, and not looked at otherwise.
- Whoever then writes the file moves the directory out of the way. Nothing in it is deleted:
  an empty directory is removed, any other is renamed to `<name>.moved-aside` beside it.
- This applies only inside barca's own artifact directory. A `@sink` path is the user's: a
  directory there fails that sink and is left alone. So is a directory in the artifact store.
"""

import json
import os
import subprocess
from pathlib import Path

import pytest

from barca.api import _find_binary

PIPELINE = """
from barca import asset


@asset()
def numbers() -> list:
    return [3, 4]


@asset(inputs={"numbers": numbers})
def total(numbers: list) -> dict:
    return {"sum": sum(numbers)}
"""


def cli(root: Path, *args: str, store: Path | None = None) -> subprocess.CompletedProcess:
    env = {k: v for k, v in os.environ.items() if not k.startswith("BARCA_")}
    if store is not None:
        env["BARCA_REMOTE_URI"] = str(store)
    return subprocess.run(
        [_find_binary(), *args],
        cwd=root,
        env=env,
        capture_output=True,
        text=True,
        check=False,
        timeout=300,
    )


def ok(proc: subprocess.CompletedProcess) -> dict:
    assert proc.returncode == 0, proc.stderr
    return json.loads(proc.stdout)


def steps(doc: dict, key: str = "status") -> dict:
    return {s["id"].rsplit(":", 1)[1]: (s.get(key), s.get("reason")) for s in doc["steps"]}


def artifact(root: Path, node: str) -> Path:
    """The path of `node`'s one artifact (a file, or whatever sits there)."""
    (path,) = [
        p
        for p in (root / ".barca" / "artifacts").glob(f"*--{node}/*.json")
        if not p.name.startswith(".")
    ]
    return path


def put_directory(path: Path, *, contents: bool = True) -> None:
    """Replace the artifact at `path` with a directory."""
    path.unlink()
    path.mkdir()
    if contents:
        (path / "inner").mkdir()
        (path / "inner" / "x").write_text("mine")


def siblings(path: Path) -> list[str]:
    return sorted(p.name for p in path.parent.iterdir())


def moved_aside(path: Path) -> Path:
    return path.with_name(path.name + ".moved-aside")


@pytest.fixture
def project(tmp_path):
    root = tmp_path / "project"
    root.mkdir()
    (root / "pipeline.py").write_text(PIPELINE)
    assert ok(cli(root, "get", "total", "--json"))["steps_executed"] == 2
    return root


# ─── local artifacts (no store) ──────────────────────────────────────────────


def test_a_step_that_reads_it_recomputes_the_input_and_the_directory_is_moved_aside(project):
    path = artifact(project, "numbers")
    put_directory(path)

    proc = cli(project, "get", "total", "--refresh", "total", "--json")

    doc = ok(proc)
    assert doc["final_output"] == {"sum": 7}
    assert steps(doc) == {"numbers": ("ran", "artifact_missing"), "total": ("ran", "refresh")}
    assert path.is_file() and json.loads(path.read_text()) == [3, 4]
    assert siblings(path) == [path.name, moved_aside(path).name]
    assert (moved_aside(path) / "inner" / "x").read_text() == "mine"
    assert "is a directory, not an artifact. Moved it" in proc.stderr, proc.stderr
    assert str(moved_aside(path).name) in proc.stderr, proc.stderr
    assert "Traceback" not in proc.stderr and "IsADirectoryError" not in proc.stderr


def test_returned_as_the_commands_output_it_is_computed_again_not_pointed_at(project):
    path = artifact(project, "numbers")
    put_directory(path)

    dry = ok(cli(project, "get", "numbers", "--dry-run", "--json"))
    assert steps(dry, "action") == {"numbers": ("run", "artifact_missing")}
    status = ok(cli(project, "status", "numbers", "--json"))
    (node,) = status["nodes"]
    assert (node["cache"]["state"], node["cache"]["reason"]) == ("stale", "artifact_missing")

    doc = ok(cli(project, "get", "numbers", "--json"))
    assert doc["final_output"] == [3, 4]
    assert steps(doc) == {"numbers": ("ran", "artifact_missing")}
    assert (moved_aside(path) / "inner" / "x").read_text() == "mine"

    again = ok(cli(project, "get", "numbers", "--json"))
    assert again["steps_executed"] == 0 and steps(again) == {"numbers": ("cached", None)}


def test_a_refresh_writes_over_it_after_moving_it_aside(project):
    path = artifact(project, "numbers")
    put_directory(path)
    doc = ok(cli(project, "get", "total", "--refresh", "numbers", "--json"))
    assert steps(doc) == {"numbers": ("ran", "refresh"), "total": ("ran", "refresh_cascade")}
    assert json.loads(path.read_text()) == [3, 4]
    assert (moved_aside(path) / "inner" / "x").read_text() == "mine"


def test_an_empty_directory_is_removed_without_a_warning(project):
    path = artifact(project, "numbers")
    put_directory(path, contents=False)
    proc = cli(project, "get", "numbers", "--json")
    assert ok(proc)["final_output"] == [3, 4]
    assert siblings(path) == [path.name] and path.is_file()
    assert "not an artifact" not in proc.stderr, proc.stderr


def test_a_directory_nothing_reads_is_not_touched(project):
    path = artifact(project, "numbers")
    put_directory(path)
    doc = ok(cli(project, "get", "total", "--json"))
    assert doc["steps_executed"] == 0
    assert steps(doc) == {"numbers": ("cached", None), "total": ("cached", None)}
    assert path.is_dir() and (path / "inner" / "x").read_text() == "mine"
    assert siblings(path) == [path.name]


def test_a_symlink_to_a_directory_is_replaced_and_its_target_is_untouched(project, tmp_path):
    path = artifact(project, "numbers")
    elsewhere = tmp_path / "elsewhere"
    elsewhere.mkdir()
    (elsewhere / "precious").write_text("keep")
    path.unlink()
    path.symlink_to(elsewhere, target_is_directory=True)

    doc = ok(cli(project, "get", "numbers", "--json"))

    assert doc["final_output"] == [3, 4]
    assert steps(doc) == {"numbers": ("ran", "artifact_missing")}
    assert path.is_file() and not path.is_symlink()
    assert siblings(path) == [path.name]
    assert sorted(p.name for p in elsewhere.iterdir()) == ["precious"]
    assert (elsewhere / "precious").read_text() == "keep"


SINK_PIPELINE = """
from barca import asset, sink


@asset()
@sink("./exports/out.json")
def exported() -> dict:
    return {"v": 1}
"""


def test_a_directory_at_a_sink_path_fails_the_sink_and_is_never_moved(tmp_path):
    root = tmp_path / "sinks"
    out = root / "exports" / "out.json"
    (out / "inner").mkdir(parents=True)
    (out / "inner" / "x").write_text("mine")
    (root / "pipeline.py").write_text(SINK_PIPELINE)

    proc = cli(root, "get", "exported", "--json")

    assert ok(proc)["final_output"] == {"v": 1}  # the asset itself succeeds
    assert "[barca] SINK FAILED: pipeline.py:exported" in proc.stderr, proc.stderr
    assert "IsADirectoryError" in proc.stderr, proc.stderr
    assert sorted(p.name for p in out.parent.iterdir()) == ["out.json"]
    assert (out / "inner" / "x").read_text() == "mine"


# ─── with an artifact store ──────────────────────────────────────────────────


@pytest.fixture
def shared(tmp_path):
    """(store, reader): `numbers` and `total` are in the store; the reader has local copies."""
    store = tmp_path / "store"
    producer = tmp_path / "producer"
    reader = tmp_path / "reader"
    for root in (producer, reader):
        root.mkdir()
        (root / "pipeline.py").write_text(PIPELINE)
    ok(cli(producer, "get", "total", "--json", store=store))
    # `get numbers` makes the reader hold a local copy of it.
    ok(cli(reader, "get", "numbers", "--json", store=store))
    return store, reader


def test_a_directory_at_the_local_copy_is_moved_aside_and_the_store_copy_fetched(shared):
    store, reader = shared
    path = artifact(reader, "numbers")
    put_directory(path)

    proc = cli(reader, "get", "numbers", "--json", store=store)

    doc = ok(proc)
    assert doc["final_output"] == [3, 4]
    assert steps(doc) == {"numbers": ("cached", None)}  # served by the store, not recomputed
    assert "fetched 1 cached artifact" in proc.stderr, proc.stderr
    assert "could not fetch" not in proc.stderr, proc.stderr
    assert path.is_file() and json.loads(path.read_text()) == [3, 4]
    assert (moved_aside(path) / "inner" / "x").read_text() == "mine"
    assert "is a directory, not an artifact. Moved it" in proc.stderr, proc.stderr


def test_a_directory_at_the_local_copy_and_no_store_copy_recomputes(shared):
    store, reader = shared
    path = artifact(reader, "numbers")
    put_directory(path)
    (stored,) = store.glob("default/artifacts/*--numbers/*.json")
    stored.unlink()

    doc = ok(cli(reader, "get", "numbers", "--json", store=store))

    assert doc["final_output"] == [3, 4]
    assert steps(doc) == {"numbers": ("ran", "artifact_missing")}
    assert path.is_file() and stored.is_file()
    assert (moved_aside(path) / "inner" / "x").read_text() == "mine"


def test_a_directory_in_the_store_is_an_infra_error_and_is_left_alone(shared):
    store, reader = shared
    (stored,) = store.glob("default/artifacts/*--numbers/*.json")
    stored.unlink()
    (stored / "theirs").mkdir(parents=True)
    artifact(reader, "numbers").unlink()  # the reader has to ask the store

    proc = cli(reader, "get", "numbers", "--json", store=store)

    assert proc.returncode == 3, proc.stderr
    envelope = json.loads(proc.stderr.strip().splitlines()[-1])
    assert envelope["kind"] == "infra"
    assert "could not fetch 1 cached artifact" in envelope["error"], envelope
    assert str(stored) in json.dumps(envelope), envelope
    # The remedy that works: recomputing would meet the same directory on upload.
    assert "Remove or rename it there" in envelope["remediation"], envelope
    assert "--refresh-all" not in json.dumps(envelope), envelope
    assert (stored / "theirs").is_dir()
    assert sorted(p.name for p in stored.parent.iterdir()) == [stored.name]


def test_a_directory_in_the_store_fails_the_upload_the_same_way(shared, tmp_path):
    """The upload side: the object of a result that is about to be stored is a directory."""
    store, reader = shared
    (stored,) = store.glob("default/artifacts/*--numbers/*.json")
    stored.unlink()
    (stored / "theirs").mkdir(parents=True)

    proc = cli(reader, "get", "numbers", "--refresh", "numbers", "--json", store=store)

    assert proc.returncode == 3, proc.stderr
    envelope = json.loads(proc.stderr.strip().splitlines()[-1])
    assert envelope["kind"] == "infra"
    text = json.dumps(envelope)
    assert "1 artifact upload(s) failed" in text and str(stored) in text, envelope
    assert "IsADirectoryError" in text and "Remove or rename it there" in text, envelope
    assert (stored / "theirs").is_dir()
    assert sorted(p.name for p in stored.parent.iterdir()) == [stored.name]
    # Removed there, the same command stores the result.
    (stored / "theirs").rmdir()
    stored.rmdir()
    ok(cli(reader, "get", "numbers", "--refresh", "numbers", "--json", store=store))
    assert json.loads(stored.read_text()) == [3, 4]


def test_a_directory_where_the_shared_history_belongs_says_so(tmp_path):
    """Not "fix the connection or credentials": the object is a directory."""
    store = tmp_path / "store"
    state = store / "default" / "state" / "metadata.db"
    (state / "theirs").mkdir(parents=True)
    root = tmp_path / "project"
    root.mkdir()
    (root / "pipeline.py").write_text(PIPELINE)

    proc = cli(root, "get", "numbers", "--json", store=store)

    assert proc.returncode == 3, proc.stderr
    envelope = json.loads(proc.stderr.strip().splitlines()[-1])
    assert envelope["kind"] == "infra"
    assert f"{state} is a directory, not the shared history file" in envelope["error"], envelope
    assert "Remove or rename that directory in the store" in envelope["remediation"], envelope
    assert "credentials" not in json.dumps(envelope), envelope
    assert (state / "theirs").is_dir()
