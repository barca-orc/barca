"""The CLI contract (`barca docs contract`): JSON output schemas, the error envelope and the
`--agent` line formats, snapshotted from the real binary.

Each case runs one command on the fixture pipeline below and reduces its output to a schema:
every key path with its JSON type, never values. Paths, run ids, hashes and timings therefore
never reach a snapshot. The schemas are compared against `snapshots/cli_contract/<case>.txt`
and against the generated tables in `crates/barca-cli/docs/contract.md`, so a change to any
command's output fails CI until both are updated in the same PR:

    scripts/update-cli-snapshots.sh            # everything (help, flags, schemas), then review
    BARCA_UPDATE_SNAPSHOTS=1 pytest python/tests/test_cli_contract.py   # just this half

The `--help` snapshots and the flag tables live in `crates/barca-cli/src/contract.rs`
(`cargo test -p barca`).
"""

import json
import os
import re
import subprocess
from pathlib import Path

import pytest

from barca.api import _find_binary

REPO = Path(__file__).resolve().parents[2]
SNAPSHOTS = Path(__file__).resolve().parent / "snapshots" / "cli_contract"
CONTRACT_MD = REPO / "crates" / "barca-cli" / "docs" / "contract.md"
UPDATE = os.environ.get("BARCA_UPDATE_SNAPSHOTS", "") not in ("", "0")
UPDATE_HINT = (
    "If this change to the CLI surface is deliberate, update the snapshots and the contract in "
    "the same PR: scripts/update-cli-snapshots.sh (or BARCA_UPDATE_SNAPSHOTS=1 pytest "
    "python/tests/test_cli_contract.py), then review the diff."
)

PIPELINE = """
import pandas as pd
from barca import asset, collect, partitions, task


@asset()
def numbers() -> list:
    return [{"n": 1}, {"n": 2}, {"n": 3}]


@asset(inputs={"rows": numbers}, env=["CONTRACT_REGION", "CONTRACT_API_TOKEN"])
def total(rows: list) -> dict:
    return {"total": sum(r["n"] for r in rows)}


@asset(inputs={"rows": numbers})
def frame(rows: list) -> pd.DataFrame:
    return pd.DataFrame(rows)


@asset(partitions={"k": partitions(["a", "b"])})
def per_key(k: str) -> dict:
    return {"key": k}


@asset(inputs={"parts": collect(per_key)})
def keys(parts: list) -> list:
    return sorted(p["key"] for p in parts)


# `rows` is never used: the one plan warning of the fixture (`unused_input`), so the commands
# that plan `report` show a filled `warnings` array and the others an empty one.
@task(inputs={"t": total, "rows": numbers})
def report(t: dict, rows: list) -> dict:
    return {"reported": t["total"]}


@task(inputs={"t": total})
def broken(t: dict) -> None:
    raise ValueError("contract fixture failure")
"""

# (case, argv, which stream holds the JSON, expected exit code). Run in this order on one
# project, so the state each command sees (what is cached, the run history) is fixed.
CASES: list[tuple[str, list[str], str, int]] = [
    ("plan", ["plan", "pipeline.py"], "stdout", 0),
    ("list", ["list", "pipeline.py", "--json"], "stdout", 0),
    ("list_truncated", ["list", "pipeline.py", "--json", "--limit", "1"], "stdout", 0),
    ("get_dry_run", ["get", "total", "pipeline.py", "--dry-run", "--json"], "stdout", 0),
    ("get", ["get", "total", "pipeline.py", "--json"], "stdout", 0),
    ("get_artifact_pointer", ["get", "frame", "pipeline.py", "--json"], "stdout", 0),
    ("sql", ["sql", "select * from frame order by n", "pipeline.py", "--json"], "stdout", 0),
    ("get_partitioned", ["get", "keys", "pipeline.py", "--json"], "stdout", 0),
    ("get_multi_target", ["get", "total,frame", "pipeline.py", "--json"], "stdout", 0),
    ("run", ["run", "report", "pipeline.py", "--json"], "stdout", 0),
    (
        "run_dry_run_multi_target",
        ["run", "report,broken", "pipeline.py", "--dry-run", "--json"],
        "stdout",
        0,
    ),
    ("run_failed", ["run", "broken", "pipeline.py", "--json"], "stdout", 1),
    ("error_step_failed", ["run", "broken", "pipeline.py", "--json"], "stderr", 1),
    (
        "run_multi_target_failed",
        ["run", "report,broken", "pipeline.py", "--json"],
        "stdout",
        1,
    ),
    ("status", ["status", "pipeline.py", "--json", "--sample", "1"], "stdout", 0),
    ("history", ["history", "--json", "--limit", "1"], "stdout", 0),
    ("stats", ["stats", "total", "pipeline.py", "--json"], "stdout", 0),
    ("docs_index", ["docs", "--json"], "stdout", 0),
    ("docs_topic", ["docs", "cache", "--json"], "stdout", 0),
    ("error_usage", ["get", "nope", "pipeline.py", "--json"], "stderr", 2),
    ("error_usage_parse", ["list", "pipeline.py", "--jsn", "--json"], "stderr", 2),
]

# Cases that need a setup of their own, run after CASES: (case, stream, expected exit code).
# `get_artifact_mismatch` is `MISMATCH_ARGV` on a second machine of a project with an artifact
# store, after one stored object was overwritten (see `_artifact_mismatch_run`).
STORE_PIPELINE = """
from barca import asset


@asset()
def numbers() -> list:
    return [1, 2, 3]


@asset()
def side() -> int:
    return 10


@asset(inputs={"numbers": numbers})
def total(numbers: list) -> dict:
    return {"total": sum(numbers)}


@asset(inputs={"t": total, "side": side})
def summary(t: dict, side: int) -> dict:
    return {"summary": t["total"] + side}
"""
MISMATCH_ARGV = ["get", "summary", "pipeline.py", "--refresh", "total", "--json"]
SETUP_CASES: list[tuple[str, list[str], str, int]] = [
    ("get_artifact_mismatch", MISMATCH_ARGV, "stdout", 0),
]
ALL_CASES = CASES + SETUP_CASES

# Values that are user data, not barca's schema: recorded as `<user value>`.
USER_VALUES = {"final_output", "targets.<name>.final_output", "nodes[].shape.sample", "rows"}
# Objects keyed by user-chosen names (targets, environment variables): keys become `<name>`.
NAME_MAPS = {"steps[].env", "targets"}

# `--agent` stderr lines, normalized: timings and counters become placeholders.
AGENT_RUNS = [
    ["get", "keys", "pipeline.py", "--agent", "--refresh-all"],
    ["get", "total", "pipeline.py", "--agent"],
    ["run", "broken", "pipeline.py", "--agent"],
    ["run", "report", "pipeline.py", "--agent"],
]


def _type(v) -> str:
    if v is None:
        return "null"
    if isinstance(v, bool):
        return "boolean"
    if isinstance(v, int):
        return "integer"
    if isinstance(v, float):
        return "number"
    if isinstance(v, str):
        return "string"
    if isinstance(v, list):
        return "array"
    return "object"


def schema(doc) -> dict[str, tuple[str, bool]]:
    """Every key path in `doc` -> (type union, present on every instance of its parent)."""
    types: dict[str, set[str]] = {}
    seen: dict[str, int] = {}  # path -> how many values were seen there
    objects: dict[str, int] = {}  # path -> how many of those were objects
    parents: dict[str, str] = {}  # path -> parent object path

    def walk(path: str, v) -> None:
        seen[path] = seen.get(path, 0) + 1
        if isinstance(v, dict):
            objects[path] = objects.get(path, 0) + 1
        if path in USER_VALUES and not (isinstance(v, dict) and "_barca_artifact" in v):
            types.setdefault(path, set()).add("<user value>")
            return
        types.setdefault(path, set()).add(_type(v))
        if isinstance(v, dict):
            for k, child in v.items():
                key = "<name>" if path in NAME_MAPS else k
                child_path = f"{path}.{key}" if path else key
                parents[child_path] = path
                walk(child_path, child)
        elif isinstance(v, list):
            for child in v:
                parents[f"{path}[]"] = path
                walk(f"{path}[]", child)

    walk("", doc)
    out = {}
    for path, ts in types.items():
        if not path:
            continue
        parent = parents[path]
        # A key is optional when its parent appeared as an object more often than the key did
        # (a `null` parent does not count). Array items and `<name>` entries are never optional.
        optional = (
            not path.endswith("[]")
            and not path.endswith("<name>")
            and seen[path] < objects.get(parent, 1)
        )
        out[path] = ("|".join(sorted(ts)), not optional)
    return dict(sorted(out.items()))


def render_snapshot(argv: list[str], stream: str, code: int, s: dict) -> str:
    lines = [f"# barca {' '.join(argv)}  ({stream}, exit {code})"]
    for path, (ty, always) in s.items():
        lines.append(f"{path} : {ty}{'' if always else '  (optional)'}")
    return "\n".join(lines) + "\n"


def render_table(s: dict) -> str:
    rows = ["| Key | Type | Present |", "|---|---|---|"]
    for path, (ty, always) in s.items():
        ty = ty.replace("|", " \\| ").replace("<user value>", "`<user value>`")
        rows.append(f"| `{path}` | {ty} | {'always' if always else 'sometimes'} |")
    return "\n".join(rows)


def normalize_agent_line(line: str) -> str:
    line = re.sub(r"\d+(\.\d+)?s\b", "<secs>s", line)
    line = re.sub(r"\(\d+/\d+\)", "(<n>/<total>)", line)
    line = re.sub(r"\] \d+/\d+ steps", "] <n>/<total> steps", line)
    return line


# ─── generated blocks in contract.md ─────────────────────────────────────────


def block_bounds(doc: str, name: str) -> tuple[int, int]:
    begin = f"<!-- BEGIN GENERATED {name} -->"
    end = f"<!-- END GENERATED {name} -->"
    start = doc.find(begin)
    assert start >= 0, f"contract.md has no `{name}` block markers"
    start += len(begin)
    stop = doc.find(end, start)
    assert stop >= 0, f"contract.md has no end marker for `{name}`"
    return start, stop


def get_block(doc: str, name: str) -> str:
    start, stop = block_bounds(doc, name)
    return doc[start:stop].strip("\n")


def set_block(doc: str, name: str, content: str) -> str:
    start, stop = block_bounds(doc, name)
    return doc[:start] + "\n" + content.strip("\n") + "\n" + doc[stop:]


def check(path: Path, actual: str) -> None:
    if UPDATE:
        path.parent.mkdir(parents=True, exist_ok=True)
        path.write_text(actual)
        return
    assert path.exists(), f"missing snapshot {path.relative_to(REPO)}\n\n{UPDATE_HINT}"
    expected = path.read_text()
    assert expected == actual, f"{path.relative_to(REPO)} is out of date.\n\n{UPDATE_HINT}"


# ─── running the fixture ─────────────────────────────────────────────────────


def _env() -> dict[str, str]:
    env = {k: v for k, v in os.environ.items() if not k.startswith("BARCA_")}
    env.pop("CONTRACT_API_TOKEN", None)
    env["CONTRACT_REGION"] = "eu"
    env["BARCA_PROGRESS_SECS"] = "0"  # no "still running" lines from a slow CI machine
    return env


@pytest.fixture(scope="module")
def runs(tmp_path_factory) -> dict:
    """Run every case once, in order, on one project. Returns case -> (schema, rendered)."""
    binary = _find_binary()
    cwd = tmp_path_factory.mktemp("contract")
    (cwd / "pipeline.py").write_text(PIPELINE)
    env = _env()

    def barca(argv: list[str]) -> subprocess.CompletedProcess:
        return subprocess.run(
            [binary, *argv], cwd=cwd, env=env, capture_output=True, text=True, timeout=300
        )

    out: dict = {}
    for case, argv, stream, code in CASES:
        # error_step_failed reads the same invocation's stderr as run_failed.
        if case == "error_step_failed":
            proc = out["run_failed"][2]
        else:
            proc = barca(argv)
        assert proc.returncode == code, (
            f"barca {' '.join(argv)}: exit {proc.returncode}, expected {code}\n"
            f"stdout: {proc.stdout}\nstderr: {proc.stderr}"
        )
        text = proc.stdout if stream == "stdout" else proc.stderr.strip().splitlines()[-1]
        doc = json.loads(text)
        s = schema(doc)
        out[case] = (s, render_snapshot(argv, stream, code, s), proc)

    store_case = tmp_path_factory.mktemp("contract_store")
    proc = _artifact_mismatch_run(binary, store_case, env)
    s = schema(json.loads(proc.stdout))
    out["get_artifact_mismatch"] = (s, render_snapshot(MISMATCH_ARGV, "stdout", 0, s), proc)

    agent: set[str] = set()
    for line in proc.stderr.splitlines():
        if line.startswith(("[barca] checking artifact store ", "[barca] uploading ")):
            line = line.replace(str(store_case / "store"), "<store>")
            line = re.sub(r"uploading \d+ artifacts", "uploading <n> artifacts", line)
            agent.add(normalize_agent_line(line))
    for argv in AGENT_RUNS:
        proc = barca(argv)
        for line in proc.stderr.splitlines():
            if line.startswith("[barca] "):
                agent.add(normalize_agent_line(line))
    out["agent_lines"] = sorted(agent)
    return out


def _artifact_mismatch_run(binary: str, tmp: Path, env: dict) -> subprocess.CompletedProcess:
    """`MISMATCH_ARGV` where the store's copy of `numbers` is not the one that was recorded.

    One machine fills a plain-directory store, the object of `numbers` is then overwritten
    (as a refresh on another machine would), and a second machine recomputes `total`, which
    reads it. `numbers` (the owner) and `total` (which read it) carry the marker; `side` and
    `summary` do not, so the key is seen to be optional.
    """
    store = tmp / "store"
    env = {**env, "BARCA_REMOTE_URI": str(store)}

    def barca(machine: str, argv: list[str]) -> subprocess.CompletedProcess:
        cwd = tmp / machine
        cwd.mkdir(exist_ok=True)
        (cwd / "pipeline.py").write_text(STORE_PIPELINE)
        proc = subprocess.run(
            [binary, *argv], cwd=cwd, env=env, capture_output=True, text=True, timeout=300
        )
        assert proc.returncode == 0, f"{machine}: barca {' '.join(argv)}\n{proc.stderr}"
        return proc

    barca("producer", ["get", "summary", "pipeline.py", "--json"])
    (stored,) = store.glob("default/artifacts/*--numbers/*.json")
    stored.write_text("[5, 5]")
    return barca("reader", MISMATCH_ARGV)


@pytest.mark.parametrize("case", [c[0] for c in ALL_CASES])
def test_json_schema_matches_snapshot(runs, case):
    _, rendered, _ = runs[case]
    check(SNAPSHOTS / f"{case}.txt", rendered)


def test_agent_lines_match_snapshot(runs):
    header = "# barca --agent stderr lines, normalized (<secs>, <n>/<total>), sorted\n"
    check(SNAPSHOTS / "agent_lines.txt", header + "\n".join(runs["agent_lines"]) + "\n")


def test_contract_doc_tables_match_the_schemas(runs):
    """Every schema table in contract.md is generated from these runs, so it cannot drift."""
    doc = CONTRACT_MD.read_text()
    tables = {f"schema {case}": render_table(runs[case][0]) for case, *_ in ALL_CASES}
    tables["agent-lines"] = "```\n" + "\n".join(runs["agent_lines"]) + "\n```"
    if UPDATE:
        for name, table in tables.items():
            doc = set_block(doc, name, table)
        CONTRACT_MD.write_text(doc)
        return
    for name, table in tables.items():
        assert get_block(doc, name) == table, (
            f"the `{name}` block of crates/barca-cli/docs/contract.md is out of date.\n\n"
            f"{UPDATE_HINT}"
        )


def test_no_stale_snapshots():
    known = {f"{c[0]}.txt" for c in ALL_CASES} | {"agent_lines.txt"}
    stale = sorted(p.name for p in SNAPSHOTS.glob("*.txt") if p.name not in known)
    if UPDATE:
        for name in stale:
            (SNAPSHOTS / name).unlink()
        return
    assert not stale, f"snapshots with no case (remove them): {stale}\n\n{UPDATE_HINT}"


def test_error_envelope_shape(runs):
    """The envelope is the same object for every kind; step_failed adds three keys."""
    base = {"error": "string", "code": "integer", "kind": "string", "remediation": "string"}
    for case in ("error_usage", "error_usage_parse"):
        s = runs[case][0]
        assert {k: v[0] for k, v in s.items()} == base, (case, s)
    failed = {k: v[0] for k, v in runs["error_step_failed"][0].items()}
    assert failed == {
        **base,
        "node": "string",
        "traceback": "string",
        "artifact_dir": "string",
    }, failed


def test_schema_reduction_ignores_values():
    a = {"run_id": "abc", "steps": [{"id": "x", "env": {"A": "1"}}, {"id": "y"}], "n": 1.5}
    b = {"run_id": "zzz", "steps": [{"id": "q", "env": {"B": None}}, {"id": "r"}], "n": 0.25}
    sa, sb = schema(a), schema(b)
    assert sa["steps[].env"] == ("object", False)
    assert sa["steps[].env.<name>"] == ("string", True)
    assert sb["steps[].env.<name>"] == ("null", True)
    assert {k: v for k, v in sa.items() if "env" not in k} == {
        k: v for k, v in sb.items() if "env" not in k
    }


def test_warnings_is_always_an_array_filled_only_where_the_plan_has_an_unused_input(runs):
    """`barca docs contract`, "Plan warnings": the key is on every plan/get/run document; the
    fixture's one unused input (`report`'s `rows`) fills it exactly where `report` is planned."""
    with_warning = {"plan", "run", "run_dry_run_multi_target", "run_multi_target_failed"}
    without = {
        "get_dry_run",
        "get",
        "get_artifact_pointer",
        "get_partitioned",
        "get_multi_target",
        "run_failed",
    }
    item = {
        "warnings[]": ("object", True),
        "warnings[].kind": ("string", True),
        "warnings[].message": ("string", True),
        "warnings[].node": ("string", True),
        "warnings[].param": ("string", True),
    }
    for case in with_warning | without:
        s, _, proc = runs[case]
        assert s["warnings"] == ("array", True), case
        assert {k: v for k, v in s.items() if k.startswith("warnings[")} == (
            item if case in with_warning else {}
        ), case
        doc = json.loads(proc.stdout)
        expected = [("pipeline.py:report", "rows")] if case in with_warning else []
        assert [(w["node"], w["param"]) for w in doc["warnings"]] == expected, case
        assert all(w["kind"] == "unused_input" for w in doc["warnings"]), case
    for case, *_ in CASES:
        if case not in with_warning | without:
            assert "warnings" not in runs[case][0], case


def test_artifact_mismatch_is_a_boolean_on_the_steps_concerned_and_absent_elsewhere(runs):
    """`barca docs contract`, "A store copy that differs from its recorded hash"."""
    s, _, proc = runs["get_artifact_mismatch"]
    # Present on some steps only, and always `true` where present: never `false` or `null`.
    assert s["steps[].artifact_mismatch"] == ("boolean", False)
    assert s["steps[].warning"] == ("string", False)
    doc = json.loads(proc.stdout)
    marked = {st["id"]: st for st in doc["steps"] if "artifact_mismatch" in st}
    assert sorted(marked) == ["pipeline.py:numbers", "pipeline.py:total"]
    assert all(st["artifact_mismatch"] is True and st["warning"] for st in marked.values())
    assert doc["final_output"] == {"summary": 20}  # 5 + 5 + 10: the store's copy was used
    # It is a step-level finding of the run: the plan-warnings array does not carry it.
    assert doc["warnings"] == []
    assert "[barca] warning: pipeline.py:numbers: the copy at " in proc.stderr
    # No other fixture run has a mismatch, so no other schema has the key.
    for case, *_ in CASES:
        assert "steps[].artifact_mismatch" not in runs[case][0], case
