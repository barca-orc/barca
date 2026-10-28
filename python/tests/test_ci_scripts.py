"""The CI helper scripts: the shard split covers every test file once, and the change filter
turns the optional backends parts on for the paths it lists and off for others."""

import importlib.util
import json
from pathlib import Path

import pytest

REPO = Path(__file__).resolve().parents[2]
SCRIPTS = REPO / "scripts"


def load(name: str):
    path = SCRIPTS / f"{name}.py"
    if not path.exists():  # e.g. running from an installed sdist
        pytest.skip("not a source checkout")
    spec = importlib.util.spec_from_file_location(name.replace("-", "_"), path)
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module


@pytest.mark.parametrize("emulators", [False, True])
@pytest.mark.parametrize("shards", [1, 2, 3, 4, 6])
def test_every_test_file_is_in_exactly_one_shard(shards, emulators):
    ci_shards = load("ci-shards")
    files = ci_shards.test_files()
    assigned = ci_shards.split(shards, emulators)
    flat = [f for part in assigned for f in part]
    assert len(assigned) == shards and all(assigned)
    assert sorted(flat) == files


def test_the_check_passes():
    assert load("ci-shards").check(3) == 0


def test_this_file_is_in_a_shard():
    ci_shards = load("ci-shards")
    assert any("python/tests/test_ci_scripts.py" in part for part in ci_shards.split(3))


def test_the_split_is_deterministic():
    ci_shards = load("ci-shards")
    assert ci_shards.split(3) == ci_shards.split(3)
    assert ci_shards.split(3, True) == ci_shards.split(3, True)


def test_the_duration_maps_name_only_test_files():
    ci_shards = load("ci-shards")
    data = json.loads(ci_shards.DURATIONS.read_text())
    assert set(data) == set(ci_shards.MAPS)
    for name, durations in data.items():
        assert durations, f"{name} in scripts/ci-test-durations.json is empty"
        stale = sorted(set(durations) - set(ci_shards.test_files()))
        assert not stale, f"refresh {name} in scripts/ci-test-durations.json: {stale}"


def test_unrelated_changes_turn_nothing_on(monkeypatch):
    ci_changes = load("ci-changes")
    monkeypatch.setattr(
        ci_changes, "changed_lines_mention", lambda base, path, pattern, head="HEAD": False
    )
    why = ci_changes.decide("base", ["README.md", "crates/barca-core/src/dag.rs", "ui/src/x.ts"])
    assert why == {"emulators": [], "duckdb1": []}


def test_store_and_duckdb_paths_turn_their_part_on(monkeypatch):
    ci_changes = load("ci-changes")
    monkeypatch.setattr(
        ci_changes, "changed_lines_mention", lambda base, path, pattern, head="HEAD": False
    )
    why = ci_changes.decide(
        "base",
        [
            "python/barca/_storage.py",
            "crates/barca-core/src/transfer.rs",
            "python/barca/_duckdb.py",
            "crates/barca-core/src/sql/tables.rs",
            "python/tests/test_state_backends.py",  # uses the emulators
            "python/tests/test_sql.py",  # mentions duckdb
        ],
    )
    assert why["emulators"] == [
        "python/barca/_storage.py",
        "crates/barca-core/src/transfer.rs",
        "python/tests/test_state_backends.py",
    ]
    assert why["duckdb1"] == [
        "python/barca/_duckdb.py",
        "crates/barca-core/src/sql/tables.rs",
        "python/tests/test_sql.py",
    ]


def test_dependencies_and_the_workflow_turn_everything_on(monkeypatch):
    ci_changes = load("ci-changes")
    monkeypatch.setattr(
        ci_changes, "changed_lines_mention", lambda base, path, pattern, head="HEAD": False
    )
    for f in ["pyproject.toml", "uv.lock", ".depot/workflows/ci.yml"]:
        why = ci_changes.decide("base", [f])
        assert why == {"emulators": [f], "duckdb1": [f]}


def test_a_changed_line_mentioning_duckdb_turns_the_rerun_on(monkeypatch):
    ci_changes = load("ci-changes")
    monkeypatch.setattr(
        ci_changes,
        "changed_lines_mention",
        lambda base, path, pattern, head="HEAD": path.endswith("_worker.py"),
    )
    why = ci_changes.decide("base", ["python/barca/_worker.py", "python/barca/_runtime.py"])
    assert why == {"emulators": [], "duckdb1": ["python/barca/_worker.py"]}
