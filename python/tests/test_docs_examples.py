"""The built-in manual (`barca docs`) must stay accurate.

Every Python example in a topic parses, every pipeline-shaped example is discovered by
`barca list`, and the runnable examples execute and produce what the text claims. If you change
CLI flags, decorators or output behavior, update crates/barca-cli/docs/ and these tests fail
until the manual matches again.
"""

import ast
import json
import re
import subprocess
from pathlib import Path

import pytest

from barca.api import _find_binary

FENCE = re.compile(r"^```(\w*)\n(.*?)^```", re.S | re.M)


def blocks(body: str, lang: str) -> list[str]:
    return [m.group(2) for m in FENCE.finditer(body) if m.group(1) == lang]


def is_pipeline(code: str) -> bool:
    return "from barca import" in code and any(d in code for d in ("@asset", "@task", "@sensor"))


@pytest.fixture(scope="module")
def binary() -> str:
    return _find_binary()


@pytest.fixture(scope="module")
def topics(binary) -> dict[str, str]:
    out = subprocess.run(
        [binary, "docs", "--all", "--json"], capture_output=True, text=True, check=True
    )
    return {t["name"]: t["content"] for t in json.loads(out.stdout)["topics"]}


def barca(binary: str, cwd: Path, *args: str) -> subprocess.CompletedProcess:
    return subprocess.run([binary, *args], cwd=cwd, capture_output=True, text=True)


def result(proc: subprocess.CompletedProcess) -> dict:
    assert proc.returncode == 0, f"exit {proc.returncode}\nstderr:\n{proc.stderr}"
    out = proc.stdout.strip()
    try:
        return json.loads(out)  # pretty-printed (plan, list, docs, history, stats)
    except json.JSONDecodeError:
        return json.loads(out.splitlines()[-1])  # get/run: user prints may precede the JSON line


def write_example(topics: dict[str, str], name: str, cwd: Path) -> Path:
    code = blocks(topics[name], "python")[0]
    path = cwd / "pipeline.py"
    path.write_text(code)
    return path


# ─── Structure ────────────────────────────────────────────────────────────────


def test_every_python_block_parses(topics):
    for name, body in topics.items():
        for i, code in enumerate(blocks(body, "python")):
            try:
                ast.parse(code)
            except SyntaxError as e:
                pytest.fail(f"docs topic '{name}', python block {i}: {e}")


FILE_MARKER = "# file: "


def project_files(code: str) -> str | None:
    """A block that starts with `# file: <path>` is one file of a multi-file example."""
    first = code.lstrip().splitlines()[0] if code.strip() else ""
    return first[len(FILE_MARKER) :].strip() if first.startswith(FILE_MARKER) else None


def test_every_pipeline_example_is_discovered_by_list(binary, topics, tmp_path):
    checked = 0
    for name, body in topics.items():
        for i, code in enumerate(blocks(body, "python")):
            if not is_pipeline(code) or project_files(code):
                continue
            f = tmp_path / f"{name.replace('/', '_')}_{i}.py"
            f.write_text(code)
            proc = barca(binary, tmp_path, "list", str(f), "--json")
            nodes = result(proc)["nodes"]
            assert nodes, f"docs topic '{name}', block {i}: `barca list` found no nodes"
            checked += 1
    assert checked >= 8, "expected the manual to contain many pipeline examples"


def test_multi_file_examples_form_one_project(binary, topics, tmp_path):
    """Blocks marked `# file: <path>` in a topic are written into one project, which plain
    `barca list` (tree discovery) must read as one DAG, with every node in it."""
    checked = 0
    for name, body in topics.items():
        files = [(project_files(c), c) for c in blocks(body, "python") if project_files(c)]
        if not files:
            continue
        root = tmp_path / name.replace("/", "_")
        root.mkdir()
        (root / "barca.toml").write_text("")
        for rel, code in files:
            (root / rel).parent.mkdir(parents=True, exist_ok=True)
            (root / rel).write_text(code)
        nodes = result(barca(binary, root, "list", "--json"))["nodes"]
        ids = {n["id"] for n in nodes}
        for rel, code in files:
            if is_pipeline(code):
                assert any(i.startswith(f"{rel}:") for i in ids), (name, rel, ids)
        checked += 1
    assert checked >= 1, "expected at least one multi-file example (barca docs discovery)"


def write_project(topics: dict[str, str], name: str, root: Path) -> Path:
    """Write a topic's `# file: <path>` blocks into `root` (with a barca.toml)."""
    root.mkdir(parents=True, exist_ok=True)
    (root / "barca.toml").write_text("")
    for code in blocks(topics[name], "python"):
        rel = project_files(code)
        if rel:
            (root / rel).parent.mkdir(parents=True, exist_ok=True)
            (root / rel).write_text(code)
    return root


def test_discovery_topic_cross_file_example_runs(binary, topics, tmp_path):
    root = write_project(topics, "discovery", tmp_path / "proj")
    run = result(barca(binary, root, "run", "validate"))
    assert run["final_output"] == {"status": "PASS"}
    assert run["steps_executed"] == 3
    # "from the root or any directory below it"
    again = result(barca(binary, root / "pipelines", "run", "validate"))
    assert again["steps_executed"] == 1


def test_docs_command_surface(binary, tmp_path):
    index = result(barca(binary, tmp_path, "docs", "--json"))
    names = [t["name"] for t in index["topics"]]
    assert {"overview", "types", "cache", "agents", "examples/duckdb"} <= set(names)
    one = result(barca(binary, tmp_path, "docs", "types", "--json"))
    assert one["name"] == "types" and one["content"].startswith("# ")
    bad = barca(binary, tmp_path, "docs", "typs")
    # No fuzzy guess (#180): the error lists every valid topic instead.
    assert bad.returncode == 2 and "did you mean" not in bad.stderr.lower()
    assert all(f"\n  {n}\n" in bad.stderr + "\n" for n in names), bad.stderr
    assert bad.stdout == ""


# ─── Runnable examples ────────────────────────────────────────────────────────


def test_example_duckdb_dag(binary, topics, tmp_path):
    pytest.importorskip("duckdb")
    write_example(topics, "examples/duckdb", tmp_path)
    out = result(barca(binary, tmp_path, "get", "top_region", "pipeline.py"))
    assert out["final_output"] == {"region": "EMEA", "orders": 3, "revenue": 465.5}
    arts = tmp_path / ".barca" / "artifacts"
    for node in ("orders", "customers", "orders_enriched", "revenue_by_region"):
        assert list((arts / f"pipeline.py--{node}").glob("*.parquet")), f"{node} not parquet"
    assert list((arts / "pipeline.py--top_region").glob("*.json"))
    # Parquet steps return a pointer on stdout, as the manual says.
    ptr = result(barca(binary, tmp_path, "get", "revenue_by_region", "pipeline.py"))
    assert ptr["final_output"]["_barca_artifact"]["format"] == "parquet"


def test_example_partitions(binary, topics, tmp_path):
    write_example(topics, "examples/partitions", tmp_path)
    plan = result(barca(binary, tmp_path, "plan", "pipeline.py"))
    steps = [s for p in plan["phases"] for st in p["streams"] for s in st["steps"]]
    assert steps.count("pipeline.py:sales") == 3
    first = result(barca(binary, tmp_path, "get", "summary", "pipeline.py"))
    assert first["steps_executed"] == 4
    assert first["final_output"] == {"regions": 3, "total": 400}
    second = result(barca(binary, tmp_path, "get", "summary", "pipeline.py"))
    assert second["steps_executed"] == 0  # every partition and the fan-in come from cache
    assert (tmp_path / ".barca" / "artifacts" / "pipeline.py--sales_region_emea").is_dir()


def test_partitions_topic_example(binary, topics, tmp_path):
    """The partitions topic's main example, as written (#189): `partitions_from(sales)` gives
    `margin` the keys of `sales`, and each key receives the key and that key's `sales` output."""
    write_example(topics, "partitions", tmp_path)
    plan = result(barca(binary, tmp_path, "plan", "pipeline.py"))
    steps = [s for p in plan["phases"] for st in p["streams"] for s in st["steps"]]
    assert steps.count("pipeline.py:sales") == 3
    assert steps.count("pipeline.py:margin") == 3
    assert steps.count("pipeline.py:summary") == 1

    margin = result(barca(binary, tmp_path, "get", "margin", "pipeline.py"))
    assert margin["steps_executed"] == 6  # three sales keys, then three margin keys
    arts = tmp_path / ".barca" / "artifacts"
    for region in ("emea", "amer", "apac"):
        (art,) = (arts / f"pipeline.py--margin_region_{region}").glob("*.json")
        assert json.loads(art.read_text()) == {"region": region, "margin": 80.0}

    summary = result(barca(binary, tmp_path, "get", "summary", "pipeline.py"))
    assert summary["steps_executed"] == 1  # every partition of sales comes from cache
    assert summary["final_output"] == {"total": 1200}
    assert result(barca(binary, tmp_path, "get", "pipeline.py"))["steps_executed"] == 0


def test_overview_topic_example(binary, topics, tmp_path):
    write_example(topics, "overview", tmp_path)
    nodes = result(barca(binary, tmp_path, "list", "pipeline.py", "--json"))["nodes"]
    assert {n["id"] for n in nodes} == {"pipeline.py:numbers", "pipeline.py:total"}
    first = result(barca(binary, tmp_path, "get", "total", "pipeline.py"))
    assert first["steps_executed"] == 2 and first["final_output"] == {"total": 6}
    assert result(barca(binary, tmp_path, "get", "total", "pipeline.py"))["steps_executed"] == 0


def test_assets_topic_example(binary, topics, tmp_path):
    write_example(topics, "assets", tmp_path)
    clean = result(barca(binary, tmp_path, "get", "clean", "pipeline.py"))
    assert clean["steps_executed"] == 2 and clean["final_output"] == {"x": 2}
    assert result(barca(binary, tmp_path, "get", "pinned", "pipeline.py"))["final_output"] == {
        "x": 0
    }
    # A Schedule asset still materializes on `barca get`; the schedule fires only under serve.
    assert result(barca(binary, tmp_path, "get", "daily", "pipeline.py"))["final_output"] == {
        "x": 2
    }


def test_assets_topic_accepted_arguments_example(binary, topics, tmp_path):
    """The "Accepted arguments" section: the example is rejected with exactly the two lines
    the manual prints, exit 2, and the table lists what the Python stubs take."""
    import inspect

    import barca as stubs

    body = topics["assets"]
    snippet = next(b for b in blocks(body, "python") if "`input` for `inputs`" in b)
    header = 'from barca import asset\n\n\n@asset()\ndef raw() -> dict:\n    return {"n": 1}\n\n\n'
    (tmp_path / "pipeline.py").write_text(header + snippet)
    documented = next(
        b for b in blocks(body, "") if b.startswith("$ barca list pipeline.py --pretty")
    )
    proc = barca(binary, tmp_path, "list", "pipeline.py", "--pretty")
    assert proc.returncode == 2
    assert proc.stderr == documented.split("\n", 1)[1]
    # Corrected as the message says, it plans.
    (tmp_path / "pipeline.py").write_text(header + snippet.replace("input=", "inputs="))
    nodes = result(barca(binary, tmp_path, "list", "pipeline.py", "--json"))["nodes"]
    assert {n["id"]: n["inputs"] for n in nodes}["pipeline.py:report"] == ["pipeline.py:raw"]

    # The table against the stubs a type checker reads (the Rust list is held to both by
    # `cargo test -p barca-core decorator_args`).
    rows = re.findall(r"^\| `@?(\w+)(?:\(\))?` \| (\w+) \| (.*) \|$", body, re.M)
    assert [r[0] for r in rows] == [
        "asset", "sensor", "task", "sink",
        "partitions", "partitions_from", "collect", "asset_ref", "Schedule",
    ]  # fmt: skip
    for name, positional, keywords in rows:
        params = inspect.signature(getattr(stubs, name)).parameters.values()
        by_keyword = [p.name for p in params if p.kind in (p.KEYWORD_ONLY, p.POSITIONAL_OR_KEYWORD)]
        by_position = [p.name for p in params if p.kind == p.POSITIONAL_ONLY and p.name != "fn"]
        assert by_keyword == ([] if keywords == "none" else re.findall(r"`(\w+)`", keywords)), name
        assert len(by_position) == {"none": 0, "one": 1}[positional], name


def test_assets_topic_unused_input_example(binary, topics, tmp_path):
    """The "Unused inputs" section: the example pipeline produces exactly the warning line and
    the JSON entry the manual prints, on every planning command, and exit 0."""
    body = topics["assets"]
    code = next(b for b in blocks(body, "python") if "is never used" in b)
    (tmp_path / "pipeline.py").write_text(code)
    documented_line = next(
        b.strip() for b in blocks(body, "") if b.startswith("[barca] warning: pipeline.py:report")
    )
    documented_json = json.loads(next(b for b in blocks(body, "json") if "unused_input" in b))
    for args in (
        ["plan", "pipeline.py"],
        ["get", "report", "pipeline.py", "--json"],
        ["get", "report", "pipeline.py", "--dry-run", "--json"],
        ["get", "pipeline.py", "--json"],
    ):
        proc = barca(binary, tmp_path, *args)
        out = result(proc)
        assert documented_line in proc.stderr.splitlines(), (args, proc.stderr)
        [warning] = out["warnings"]
        assert {k: warning[k] for k in ("kind", "node", "param")} == {
            k: documented_json[k] for k in ("kind", "node", "param")
        }
        assert "[barca] warning: " + warning["message"] == documented_line
        assert warning["message"].startswith(documented_json["message"].removesuffix("..."))
    # "not about other steps in the file": `raw` alone has nothing to report.
    proc = barca(binary, tmp_path, "get", "raw", "pipeline.py", "--json")
    assert result(proc)["warnings"] == [] and "warning" not in proc.stderr
    # "`barca list` and `barca status` do not report it."
    for cmd in ("list", "status"):
        proc = barca(binary, tmp_path, cmd, "pipeline.py", "--json")
        assert "warnings" not in result(proc) and "never uses" not in proc.stderr
    # The fix the message names: `_raw` is ordering only, receives None, and is not flagged.
    fixed = code.replace('{"raw": raw}', '{"_raw": raw}').replace(
        "report(raw: list)", "report(_raw)"
    )
    assert fixed != code
    (tmp_path / "pipeline.py").write_text(fixed.replace("return 42", "return _raw"))
    proc = barca(binary, tmp_path, "get", "report", "pipeline.py", "--json")
    out = result(proc)
    assert out["warnings"] == [] and out["final_output"] is None
    assert "warning" not in proc.stderr


def test_site_ordering_only_pattern_example_runs(binary, tmp_path):
    """The site's "Ordering-Only Dependencies" page: its "right way" example runs, in order,
    without the unused-input warning. (The page is not a manual topic, so it is read from the
    repository; the helpers it calls but does not define are stubbed.)"""
    page = Path(__file__).resolve().parents[2] / (
        "site/src/content/docs/patterns/03-ordering-only-deps.md"
    )
    if not page.exists():
        pytest.skip("site docs are not in this checkout")
    code = blocks(page.read_text(), "python")[0]
    assert "def seed_data(_migrate):" in code
    stubs = (
        "\n\ndef run_migrations():\n    open('order.log', 'a').write('migrate\\n')\n"
        "\n\ndef insert_seed_records():\n    open('order.log', 'a').write('seed\\n')\n"
    )
    (tmp_path / "pipeline.py").write_text(code + stubs)
    proc = barca(binary, tmp_path, "run", "seed_data", "pipeline.py", "--json")
    out = result(proc)
    assert out["status"] == "success" and out["warnings"] == []
    assert "warning" not in proc.stderr
    assert (tmp_path / "order.log").read_text() == "migrate\nseed\n"
    # Without the parameter the step cannot be called: barca passes `_migrate=None`.
    broken = code.replace("def seed_data(_migrate):", "def seed_data():")
    (tmp_path / "pipeline.py").write_text(broken + stubs)
    proc = barca(binary, tmp_path, "run", "seed_data", "pipeline.py", "--json")
    assert proc.returncode == 1 and "TypeError" in proc.stderr


def test_scheduling_topic_example(binary, topics, tmp_path):
    write_example(topics, "scheduling", tmp_path)
    nodes = result(barca(binary, tmp_path, "list", "pipeline.py", "--json"))["nodes"]
    assert {n["id"] for n in nodes} == {"pipeline.py:daily_report", "pipeline.py:heartbeat"}
    report = result(barca(binary, tmp_path, "get", "daily_report", "pipeline.py"))
    assert report["final_output"] == {"rows": 1}
    beat = barca(binary, tmp_path, "run", "heartbeat", "pipeline.py")
    assert result(beat)["status"] == "success"


def test_example_deploy_task(binary, topics, tmp_path):
    write_example(topics, "examples/deploy-task", tmp_path)
    assert result(barca(binary, tmp_path, "run", "deploy", "pipeline.py"))["steps_executed"] == 2
    assert result(barca(binary, tmp_path, "run", "deploy", "pipeline.py"))["steps_executed"] == 1
    refreshed = barca(binary, tmp_path, "run", "deploy", "pipeline.py", "--refresh", "model")
    assert result(refreshed)["steps_executed"] == 2
    only = barca(
        binary, tmp_path, "run", "deploy", "pipeline.py", "--refresh", "model", "--no-cascade"
    )
    assert result(only)["steps_executed"] == 2  # nothing besides the task is downstream of model
    wrong = barca(binary, tmp_path, "get", "deploy", "pipeline.py")
    assert wrong.returncode == 2 and "barca run" in wrong.stderr


def test_types_topic_example_reads_one_parquet_two_ways(binary, topics, tmp_path, monkeypatch):
    pytest.importorskip("duckdb")
    pytest.importorskip("polars")
    pytest.importorskip("pyarrow")
    write_example(topics, "types", tmp_path)
    total = result(barca(binary, tmp_path, "get", "total", "pipeline.py"))
    assert total["final_output"] == {"total": 9.5}
    pointer = result(barca(binary, tmp_path, "get", "as_polars", "pipeline.py"))
    assert pointer["final_output"]["_barca_artifact"]["format"] == "parquet"
    lazy = result(barca(binary, tmp_path, "get", "big_ids", "pipeline.py"))
    assert lazy["final_output"]["_barca_artifact"]["format"] == "parquet"

    import barca as barca_api

    monkeypatch.chdir(tmp_path)
    df = barca_api.get("as_polars", "pipeline.py")  # the Python API loads parquet for you
    assert df["doubled"].tolist() == [19.0]
    assert barca_api.get("big_ids", "pipeline.py")["id"].tolist() == [1]


def test_assets_topic_env_example(binary, topics, tmp_path):
    """`@asset(env=[...])`: declared values are hashed, reported, and secrets redacted."""
    import os

    code = next(c for c in blocks(topics["assets"], "python") if "env=[" in c)
    (tmp_path / "pipeline.py").write_text(code)

    def get(**env):
        base = {k: v for k, v in os.environ.items() if k not in ("SOURCE_CSV", "API_TOKEN")}
        return subprocess.run(
            [binary, "get", "summary", "pipeline.py", "--agent"],
            cwd=tmp_path,
            capture_output=True,
            text=True,
            env={**base, **env},
        )

    first = get(SOURCE_CSV="a.csv")
    out = result(first)
    assert out["steps_executed"] == 2
    assert out["final_output"] == {"from": "a.csv"}
    raw = next(s for s in out["steps"] if s["id"] == "pipeline.py:raw")
    assert raw["env"] == {"API_TOKEN": None, "SOURCE_CSV": "a.csv"}
    summary = next(s for s in out["steps"] if s["id"] == "pipeline.py:summary")
    assert "env" not in summary  # declares nothing
    assert "env API_TOKEN=<unset> SOURCE_CSV=a.csv" in first.stderr

    cached = get(SOURCE_CSV="a.csv")
    assert result(cached)["steps_executed"] == 0
    assert "step:pipeline.py:raw cached env API_TOKEN=<unset> SOURCE_CSV=a.csv" in cached.stderr

    changed = result(get(SOURCE_CSV="b.csv"))
    assert changed["steps_executed"] == 2  # raw and everything downstream
    assert changed["final_output"] == {"from": "b.csv"}

    # A secret is part of the hash but never printed.
    secret = get(SOURCE_CSV="b.csv", API_TOKEN="hunter2")
    out = result(secret)
    assert out["steps_executed"] == 2
    raw = next(s for s in out["steps"] if s["id"] == "pipeline.py:raw")
    assert raw["env"]["API_TOKEN"] == "<redacted>"
    assert "hunter2" not in secret.stdout + secret.stderr
    assert "API_TOKEN=<redacted>" in secret.stderr

    # Unset and empty are different values.
    assert result(get(SOURCE_CSV="b.csv", API_TOKEN=""))["steps_executed"] == 2

    nodes = result(barca(binary, tmp_path, "list", "pipeline.py", "--json"))["nodes"]
    by_id = {n["id"]: n for n in nodes}
    assert by_id["pipeline.py:raw"]["env"] == ["SOURCE_CSV", "API_TOKEN"]
    assert by_id["pipeline.py:summary"]["env"] == []
    table = barca(binary, tmp_path, "list", "pipeline.py", "--pretty").stdout
    assert "ENV" in table.splitlines()[0] and "SOURCE_CSV, API_TOKEN" in table


def test_env_must_be_a_literal_list(binary, tmp_path):
    (tmp_path / "pipeline.py").write_text(
        "from barca import asset\n\nNAMES = ['A']\n\n\n"
        "@asset(env=NAMES)\ndef a() -> int:\n    return 1\n"
    )
    proc = barca(binary, tmp_path, "list", "pipeline.py")
    assert proc.returncode == 2  # a parse error is a usage error
    error = json.loads(proc.stderr.strip().splitlines()[-1])["error"]  # JSON when piped
    assert "invalid env=" in error and 'env=["SOURCE_CSV"' in error


def steps(run: dict) -> dict:
    return {s["id"].split(":")[-1]: s for s in run["steps"]}


def test_tasks_topic_example(binary, topics, tmp_path):
    write_example(topics, "tasks", tmp_path)

    # send_email receives the report: the asset runs, then the task.
    first = barca(binary, tmp_path, "run", "send_email", "pipeline.py")
    assert "sending report with 42 rows" in first.stderr  # a step's print goes to stderr
    assert {k: v["status"] for k, v in steps(result(first)).items()} == {
        "report": "ran",
        "send_email": "ran",
    }

    # "task runs; upstream assets come from cache"
    again = result(barca(binary, tmp_path, "run", "send_email", "pipeline.py"))
    assert steps(again)["report"]["status"] == "cached"
    assert steps(again)["send_email"]["status"] == "ran"

    # --refresh report / --no-cascade / --refresh-all re-materialize report.
    for flags in (
        ["--refresh", "report"],
        ["--refresh", "report", "--no-cascade"],
        ["--refresh-all"],
    ):
        run = result(barca(binary, tmp_path, "run", "send_email", "pipeline.py", *flags))
        assert steps(run)["report"]["status"] == "ran", flags

    # notify runs after migrate; `_migrate` is ordering only and receives None.
    notify = barca(binary, tmp_path, "run", "notify", "pipeline.py")
    assert set(steps(result(notify))) == {"migrate", "notify"}
    assert notify.stderr.index("migrating") < notify.stderr.index("migration done")

    # get on a task is an error; a bare get never runs tasks and says how to run them.
    wrong = barca(binary, tmp_path, "get", "send_email", "pipeline.py")
    assert wrong.returncode == 2 and "barca run" in wrong.stderr
    bare = barca(binary, tmp_path, "get", "pipeline.py")
    assert set(steps(result(bare))) == {"report"}
    assert "barca run" in bare.stderr


def test_tasks_topic_fan_out_example(binary, topics, tmp_path):
    """Fan-out: a branch returns a dict holding a date and a set, one branch raises."""
    code = blocks(topics["tasks"], "python")[2]
    assert "def check_all" in code
    (tmp_path / "pipeline.py").write_text(code)
    doc = result(barca(binary, tmp_path, "run", "check_all", "pipeline.py"))
    assert doc["final_output"] == {
        "checked": ["2026-01-02", "2026-01-02"],
        "failed": ["ap"],
        "zones": ["a", "b"],
    }
    # The value the topic prints is this one.
    printed = re.search(r"^# final_output: (.*)$", topics["tasks"], re.M)
    assert printed and json.loads(printed.group(1)) == doc["final_output"]


def test_tasks_topic_what_a_branch_may_return(binary, topics, tmp_path):
    """Every sentence of "What a branch may return", "When a branch fails" and "Limits"."""
    (tmp_path / "pipeline.py").write_text(
        """
import datetime
from functools import partial

from barca import ParallelError, asset, parallel, task


@task()
def value(kind: str):
    return {"tuple": (1, 2), "keys": {1: "a"}, "none": None, "set": {1, 2}}[kind]


@task()
def raises(i: int):
    raise ValueError("boom")


@task()
def handle(i: int):
    return open(__file__)


@task()
def kind_of(x) -> str:
    return type(x).__name__


@task()
def values() -> list:
    out = parallel(*(partial(value, k) for k in ["tuple", "keys", "none", "set"]))
    failed = parallel(partial(raises, 0))[0]
    return [repr(v) for v in out] + [isinstance(failed, ParallelError), failed.error.splitlines()[0]]


@task()
def unpassable() -> list:
    return parallel(partial(value, "set"), partial(handle, 1))


@task()
def set_argument() -> list:
    return parallel(partial(kind_of, {1, 2}))


@task()
def tuple_argument() -> list:
    return parallel(partial(kind_of, (1, 2)))


@asset()
def from_an_asset() -> list:
    return [sorted(v) for v in parallel(partial(value, "set"))]
"""
    )
    doc = result(barca(binary, tmp_path, "run", "values", "pipeline.py"))
    assert doc["final_output"] == [
        "[1, 2]",  # a tuple comes back as a list
        "{'1': 'a'}",  # non-string keys as strings
        "None",
        "{1, 2}",
        True,
        "ValueError: boom",
    ]

    proc = barca(binary, tmp_path, "run", "unpassable", "pipeline.py")
    assert proc.returncode == 1
    error = json.loads(proc.stdout)["error"]
    quoted = " ".join(blocks(topics["tasks"], "")[0].split())
    assert error.startswith(quoted), (error, quoted)

    proc = barca(binary, tmp_path, "run", "set_argument", "pipeline.py")
    assert proc.returncode == 1 and "TypeError" in json.loads(proc.stdout)["error"]
    doc = result(barca(binary, tmp_path, "run", "tuple_argument", "pipeline.py"))
    assert doc["final_output"] == ["list"]

    # From an asset the branches run; served from cache, the asset does not call them again.
    first = result(barca(binary, tmp_path, "get", "from_an_asset", "pipeline.py"))
    assert first["final_output"] == [[1, 2]] and first["steps_executed"] == 1
    again = result(barca(binary, tmp_path, "get", "from_an_asset", "pipeline.py"))
    assert again["final_output"] == [[1, 2]] and again["steps_executed"] == 0

    # What the branches returned is gone with the runs; the artifact directory holds the
    # results of the steps that finished and nothing per branch.
    barca_dir = tmp_path / ".barca"
    assert [p for p in (barca_dir / "branches").rglob("*") if p.is_file()] == []
    assert sorted(p.name for p in (barca_dir / "artifacts").iterdir()) == [
        "pipeline.py--from_an_asset",
        "pipeline.py--tuple_argument",
        "pipeline.py--values",
    ]


def test_sinks_topic_example(binary, topics, tmp_path):
    pytest.importorskip("pyarrow")
    pd = pytest.importorskip("pandas")

    code = blocks(topics["sinks"], "python")[0]
    remote = "s3://my-bucket/exports/orders.parquet"
    assert remote in code
    # Same pipeline, with the remote sink pointed at a local path.
    (tmp_path / "pipeline.py").write_text(code.replace(remote, "./remote/orders.parquet"))
    result(barca(binary, tmp_path, "get", "pipeline.py"))
    expected = [{"id": 1, "amount": 9.5}, {"id": 2, "amount": 20.0}]
    for path in ("exports/orders.parquet", "remote/orders.parquet"):
        assert pd.read_parquet(tmp_path / path).to_dict("records") == expected, path
    assert json.loads((tmp_path / "exports" / "summary.json").read_text()) == {"orders": 2}


def test_a_parquet_sink_of_a_non_frame_fails_the_sink_not_the_asset(binary, tmp_path):
    (tmp_path / "pipeline.py").write_text(
        "from barca import asset, sink\n\n\n"
        "@asset()\n@sink('./exports/banana.parquet')\n"
        "def banana() -> dict:\n    return {'a': 1}\n"
    )
    run = barca(binary, tmp_path, "get", "banana", "pipeline.py")
    assert result(run)["final_output"] == {"a": 1}
    assert "[barca] SINK FAILED" in run.stderr and "cannot be written as parquet" in run.stderr
    assert not (tmp_path / "exports" / "banana.parquet").exists()


def test_a_failing_sink_does_not_fail_its_asset(binary, tmp_path):
    (tmp_path / "pipeline.py").write_text(
        "from barca import asset, sink\n\n\n"
        "@asset()\n@sink('nosuchscheme://bucket/x.json')\n"
        "def banana() -> dict:\n    return {'a': 1}\n"
    )
    run = barca(binary, tmp_path, "get", "banana", "pipeline.py")
    assert result(run)["final_output"] == {"a": 1}
    assert "[barca] SINK FAILED" in run.stderr


def test_partitioned_sinks_insert_the_key_before_the_extension(binary, tmp_path):
    (tmp_path / "pipeline.py").write_text(
        "from barca import asset, partitions, sink\n\n\n"
        "@asset(partitions={'region': partitions(['emea', 'amer'])})\n"
        "@sink('./out/out.json')\n"
        "def per_region(region: str) -> dict:\n    return {'region': region}\n"
    )
    result(barca(binary, tmp_path, "get", "per_region", "pipeline.py"))
    written = sorted(p.name for p in (tmp_path / "out").iterdir())
    assert written == ["out_region_amer.json", "out_region_emea.json"]


def test_tasks_topic_several_targets_example(binary, topics, tmp_path):
    (tmp_path / "pipeline.py").write_text(blocks(topics["tasks"], "python")[1])
    targets = "validate_registry,validate_names"
    dry = result(barca(binary, tmp_path, "run", targets, "pipeline.py", "--dry-run"))
    assert list(dry["targets"]) == ["validate_registry", "validate_names"]
    assert dry["targets"]["validate_names"]["summary"]["will_run"] == 2  # registry + the check
    assert dry["summary"]["will_run"] == 3
    first = result(barca(binary, tmp_path, "run", targets, "pipeline.py"))
    assert first["steps_executed"] == 3  # registry materializes once for both checks
    assert [s["id"] for s in first["steps"]].count("pipeline.py:registry") == 1
    assert first["targets"] == {
        "validate_registry": {"status": "success", "final_output": {"models": 2}},
        "validate_names": {"status": "success", "final_output": {"lowercase": True}},
    }
    assert "final_output" not in first
    second = result(barca(binary, tmp_path, "run", targets, "pipeline.py"))
    assert second["steps_executed"] == 2  # registry from cache; the tasks always re-run


def test_cache_topic_external_data_sensor_example(binary, topics, tmp_path):
    """The etag sensor re-runs its consumers when the data changes in place, and only then; a
    dry run predicts from the sensor's last output, and is `unknown` before it ever ran (#183)."""
    (code,) = (
        b for b in blocks(topics["cache"], "python") if "def orders_etag" in b and "@asset" in b
    )
    (tmp_path / "pipeline.py").write_text(code)
    (tmp_path / "orders.csv").write_text("id,total\n1,10\n")

    def get(*extra: str) -> dict:
        return result(barca(binary, tmp_path, "get", "silver", "pipeline.py", *extra))

    def by_name(r: dict) -> dict:
        return {s["id"].split(":")[-1]: s for s in r["steps"]}

    never = by_name(get("--dry-run"))
    assert never["bronze"]["action"] == "unknown"
    assert never["bronze"]["reason"] == "sensor_output_unknown"
    assert never["silver"]["action"] == "unknown"

    first = get()
    assert first["steps_executed"] == 3 and first["final_output"] == 2

    second = get()
    assert second["steps_executed"] == 1, "only the sensor runs when the etag is unchanged"
    assert by_name(second)["bronze"]["status"] == "cached"

    (tmp_path / "orders.csv").write_text("id,total\n1,10\n2,20\n")
    dry = by_name(get("--dry-run"))
    assert dry["bronze"]["action"] == "cached"
    assert (
        "assumes sensor 'orders_etag' returns the same value as its last run"
        in dry["bronze"]["detail"]
    )
    third = get()
    assert third["steps_executed"] == 3 and third["final_output"] == 3
    assert by_name(third)["bronze"]["status"] == "ran"
    assert by_name(third)["silver"]["status"] == "ran"

    # The trap block: a value that changes every run, flagged in the text.
    (trap,) = (b for b in blocks(topics["cache"], "python") if "checked_at" in b)
    assert "re-runs bronze every time" in trap


def test_cache_topic_helper_module_example(binary, topics, tmp_path):
    """Editing a used helper re-runs the step, editing an unused one doesn't, and every spelling
    of the pipeline path computes the same run hash (#178)."""
    helpers, pipeline = (b for b in blocks(topics["cache"], "python") if "helpers" in b)
    assert helpers.startswith("# helpers.py") and pipeline.startswith("# pipeline.py")
    (tmp_path / "helpers.py").write_text(helpers)
    (tmp_path / "pipeline.py").write_text(pipeline)

    def get() -> dict:
        return result(barca(binary, tmp_path, "get", "rows", "pipeline.py"))

    first = get()
    assert first["steps_executed"] == 1 and first["final_output"] == [1, 2]
    assert get()["steps_executed"] == 0

    (tmp_path / "helpers.py").write_text(helpers.replace("editing this", "edited, this"))
    assert get()["steps_executed"] == 0, "editing an unused helper re-runs nothing"

    (tmp_path / "helpers.py").write_text(helpers.replace("if r]", "if r] + [3]"))
    edited = get()
    assert edited["steps_executed"] == 1 and edited["final_output"] == [1, 2, 3]

    def run_hash(cwd: Path, arg: str) -> str:
        status = result(barca(binary, cwd, "status", "rows", arg, "--json"))
        return status["nodes"][0]["cache"]["run_hash"]

    spellings = [
        (tmp_path, "pipeline.py"),
        (tmp_path, "./pipeline.py"),
        (tmp_path, str(tmp_path / "pipeline.py")),
        (tmp_path.parent, f"{tmp_path.name}/pipeline.py"),
    ]
    assert len({run_hash(cwd, arg) for cwd, arg in spellings}) == 1


def test_status_topic_example(binary, topics, tmp_path):
    pytest.importorskip("pyarrow")
    write_example(topics, "status", tmp_path)
    assert result(barca(binary, tmp_path, "get", "total", "pipeline.py"))["run_id"]
    table = barca(binary, tmp_path, "status", "pipeline.py", "--pretty")
    assert table.returncode == 0, table.stderr
    assert "3 rows x 2 cols" in table.stdout and "dict (1 key)" in table.stdout
    assert "2 cached, 0 stale, 0 never run, 0 partial, 0 unknown, 1 always run" in table.stdout
    doc = result(barca(binary, tmp_path, "status", "total", "pipeline.py", "--json"))
    assert doc["target"] == "total"
    by = {n["name"]: n for n in doc["nodes"]}
    assert list(by) == ["orders", "total"]
    assert by["orders"]["shape"]["columns"] == [
        {"name": "id", "type": "int64"},
        {"name": "amount", "type": "double"},
    ]
    assert by["orders"]["cache"]["artifact"] == by["orders"]["last_materialization"]["artifact"]
    sampled = result(barca(binary, tmp_path, "status", "pipeline.py", "--json", "--sample", "2"))
    assert len({n["name"]: n for n in sampled["nodes"]}["orders"]["shape"]["sample"]) == 2


# ─── Machine-readable inspection commands ─────────────────────────────────────


def test_json_inspection_commands(binary, tmp_path):
    (tmp_path / "pipeline.py").write_text(
        "from barca import asset\n\n\n"
        "@asset()\ndef numbers() -> list:\n    return [1, 2, 3]\n\n\n"
        '@asset(inputs={"nums": numbers})\ndef total(nums: list) -> dict:\n'
        '    return {"total": sum(nums)}\n'
    )
    listing = result(barca(binary, tmp_path, "list", "pipeline.py", "--json"))
    assert listing["total"] == 2 and listing["truncated"] is False
    by_id = {n["id"]: n for n in listing["nodes"]}
    assert by_id["pipeline.py:total"]["inputs"] == ["pipeline.py:numbers"]
    assert by_id["pipeline.py:numbers"]["kind"] == "asset"

    assert result(barca(binary, tmp_path, "get", "total", "pipeline.py"))["steps_executed"] == 2
    history = result(barca(binary, tmp_path, "history", "--json"))
    assert history["total"] == 1 and history["runs"][0]["status"] == "success"
    stats = result(barca(binary, tmp_path, "stats", "total", "pipeline.py", "--json"))
    assert stats["id"] == "pipeline.py:total"
    assert barca(binary, tmp_path, "get", "total", "pipeline.py").stdout.count("\n") == 1


def test_big_inputs_topic_example(binary, topics, tmp_path, monkeypatch):
    pytest.importorskip("duckdb")
    pytest.importorskip("polars")
    pytest.importorskip("pandas")
    pytest.importorskip("pyarrow")
    write_example(topics, "big-inputs", tmp_path)

    # Aggregate over a lazy relation: written as parquet, a pointer on stdout.
    per_bucket = result(barca(binary, tmp_path, "get", "per_bucket", "pipeline.py"))
    assert per_bucket["final_output"]["_barca_artifact"]["format"] == "parquet"
    bucket_3 = result(barca(binary, tmp_path, "get", "bucket_3", "pipeline.py"))
    assert bucket_3["final_output"]["_barca_artifact"]["format"] == "parquet"

    import barca as barca_api

    monkeypatch.chdir(tmp_path)
    assert barca_api.get("per_bucket", "pipeline.py")["n"].tolist() == [10000] * 10
    assert barca_api.get("bucket_3", "pipeline.py")["id"].min() == 3

    # The pandas path: narrow first, convert the small result.
    first = result(barca(binary, tmp_path, "get", "first_ids", "pipeline.py"))
    assert first["final_output"] == {"ids": [3, 13, 23, 33, 43]}

    # An ordering-only input runs after its upstream and warns about nothing.
    proc = barca(binary, tmp_path, "get", "after_events", "pipeline.py")
    assert result(proc)["final_output"] == {"ran": True}
    assert "warning" not in proc.stderr
