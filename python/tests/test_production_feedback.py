"""Production feedback from a pipeline under `barca serve` (#627, #628): one history for `serve`
and the CLI, why a step failed, per-step memory, `run_id` in `barca stats`, a required `--env`,
the nondeterminism warning and computed partition keys (`barca docs status`, `barca docs cache`,
`barca docs partitions`).
"""

import json
import os
import subprocess
import sys
from pathlib import Path

import pytest

from barca.api import _find_binary

from . import served
from .test_server_ledger import SCRUB, WAIT

NOT_DETERMINISTIC = "produced different bytes for the same code and inputs"


def _env(**extra: str) -> dict[str, str]:
    env = {k: v for k, v in os.environ.items() if not k.startswith(SCRUB)}
    env["BARCA_POOL_SIZE"] = "1"
    env.update(extra)
    return env


def barca(cwd: Path, *args: str, **env: str) -> subprocess.CompletedProcess:
    return subprocess.run(
        [_find_binary(), *args],
        cwd=cwd,
        capture_output=True,
        text=True,
        env=_env(**env),
        timeout=WAIT,
    )


def ok(out: subprocess.CompletedProcess) -> dict:
    assert out.returncode == 0, out.stderr
    return json.loads(out.stdout)


# ─── #628.1: `barca plan --json` ─────────────────────────────────────────────


def test_plan_accepts_json_and_pretty_and_prints_the_same_plan(tmp_path):
    (tmp_path / "p.py").write_text("from barca import asset\n\n@asset()\ndef a() -> int:\n    return 1\n")
    plain = barca(tmp_path, "plan", "p.py")
    assert plain.returncode == 0, plain.stderr
    for flag in ("--json", "--pretty"):
        out = barca(tmp_path, "plan", "p.py", flag)
        assert out.returncode == 0, out.stderr
        assert out.stdout == plain.stdout
    assert barca(tmp_path, "plan", "p.py", "--json", "--pretty").returncode == 0
    assert "capacity_source" in json.loads(plain.stdout)


# ─── #627.1 and #627.2: serve runs in the CLI's history, and why a step failed ─

SERVED = """
import sys
from barca import asset


@asset()
def source() -> int:
    print("loading the source")
    return 1


@asset(inputs={"n": source})
def checked(n: int) -> int:
    print(f"checking the keys of source {n}")
    for key in range(25):
        print(f"key {key} failed the check")
    print("explained on stderr", file=sys.stderr)
    raise AssertionError("checked: 25 check(s) failed")
"""


def test_runs_serve_started_are_in_the_clis_history_and_logs(tmp_path):
    (tmp_path / "barca.toml").write_text('default_env = "staging"\n')
    (tmp_path / "pipeline.py").write_text(SERVED)
    with served.Serve(tmp_path, env=_env()) as serve:
        serve.wait("the server", lambda: serve.get("/health"))
        started = serve.post("/runs", {"command": "get", "targets": ["checked"]})
        run_id = started["run_id"]
        serve.wait(
            "the run to finish",
            lambda: serve.get(f"/runs/{run_id}")["status"] not in served.LIVE,
        )
    # The id the server answered with is the one the CLI reads, by default_env, --env and
    # BARCA_ENV alike: serve and the CLI share .barca/envs/staging/metadata.db.
    for args, env in (((), {}), (("--env", "staging"), {}), ((), {"BARCA_ENV": "staging"})):
        runs = ok(barca(tmp_path, "history", "--json", *args, **env))["runs"]
        assert run_id in [r["run_id"] for r in runs], (args, env, runs)
        logs = barca(tmp_path, "logs", run_id, "--pretty", *args, **env)
        assert logs.returncode == 0, logs.stderr
        assert "key 24 failed the check" in logs.stdout
    assert ok(barca(tmp_path, "history", "--json", "--env", "default"))["runs"] == []
    # The lines are in the database under .barca/, not only in the server's memory.
    assert (tmp_path / ".barca" / "envs" / "staging" / "metadata.db").is_file()

    # failed_node and error, flat, for --fields.
    runs = ok(barca(tmp_path, "history", "--json", "--fields", "run_id,failed_node,error"))["runs"]
    assert {"run_id": run_id, "failed_node": "pipeline.py:checked",
            "error": "AssertionError: checked: 25 check(s) failed"} in runs

    # The run detail: the traceback, then the last lines the step printed, without the
    # traceback again.
    view = ok(barca(tmp_path, "history", run_id, "--json"))
    [step] = [s for s in view["steps"] if s["status"] == "failed"]
    tail = step["output_tail"]
    assert len(tail) == 20
    assert tail[-2:] == ["key 24 failed the check", "explained on stderr"]
    assert not any("Traceback" in line for line in tail)
    [good] = [s for s in view["steps"] if s["status"] == "success"]
    assert good["output_tail"] is None
    text = barca(tmp_path, "history", run_id, "--pretty").stdout
    assert "last 20 lines of its output:" in text, text
    assert "  | key 24 failed the check" in text
    assert "PEAK_RSS" in text


KEYED_FAILURE = """
from barca import asset, partitions


@asset(partitions={"key": partitions(["k0", "k1", "k2", "k3", "k4", "k5"])})
def scan(key: str) -> str:
    print(f"scanning {key}")
    if key == "k5":
        print("k5 is malformed")
        raise ValueError("bad key k5")
    return key
"""


def test_a_failed_step_keeps_its_output_tail_in_a_bounded_page_of_steps(tmp_path):
    """A run's steps are paged (#623, failed steps kept first); the failed key recorded last
    is still listed past the limit, with its output tail (#627)."""
    (tmp_path / "pipeline.py").write_text(KEYED_FAILURE)
    out = barca(tmp_path, "get", "scan", "--json")
    assert out.returncode == 1, out.stderr
    run_id = json.loads(out.stdout)["run_id"]
    view = ok(barca(tmp_path, "history", run_id, "--limit", "2", "--json"))
    assert view["steps_page"] == {"total": 6, "truncated": True}, view["steps_page"]
    assert len(view["steps"]) == 2
    [failed] = [s for s in view["steps"] if s["status"] == "failed"]
    assert failed["partition_key"] == "k5" or failed["node_id"].endswith("k5]"), failed
    assert failed["output_tail"] == ["scanning k5", "k5 is malformed"], failed
    runs = ok(barca(tmp_path, "history", "--json", "--fields", "run_id,failed_node,error"))
    [run] = runs["runs"]
    assert run["run_id"] == run_id and "k5" in run["failed_node"], run
    assert run["error"].startswith("ValueError: bad key k5"), run


# ─── #627.3 and #627.4: per-step memory, and run_id in barca stats ───────────

MEMORY = """
from barca import asset


@asset()
def big() -> int:
    block = bytearray(300 * 1024 * 1024)
    for i in range(0, len(block), 4096):
        block[i] = 1
    return len(block)


@asset(inputs={"n": big})
def small(n: int) -> int:
    return n + 1
"""


@pytest.mark.skipif(not sys.platform.startswith("linux"), reason="the per-step peak is Linux's")
def test_peak_memory_is_each_steps_own(tmp_path):
    (tmp_path / "pipeline.py").write_text(MEMORY)
    # One worker runs both steps: the lifetime peak would give `small` the 300 MB of `big`.
    out = ok(barca(tmp_path, "get", "small", "--json"))
    view = ok(barca(tmp_path, "history", out["run_id"], "--json"))
    rss = {s["node_id"].split(":")[1]: s["max_rss_bytes"] for s in view["steps"]}
    assert rss["big"] > 300 * 1024 * 1024, rss
    assert rss["small"] < 200 * 1024 * 1024, rss
    assert all(s["cpu_seconds"] is not None for s in view["steps"])

    stats = ok(barca(tmp_path, "stats", "big", "--json"))
    [entry] = stats["recent_runs"]
    assert entry["run_id"] == out["run_id"]
    assert entry["max_rss_bytes"] == rss["big"]
    assert stats["max_rss_bytes"] == stats["median_max_rss_bytes"] == rss["big"]
    trimmed = ok(barca(tmp_path, "stats", "big", "--fields", "run_id,max_rss_bytes"))
    assert trimmed["recent_runs"] == [{"run_id": out["run_id"], "max_rss_bytes": rss["big"]}]
    text = barca(tmp_path, "stats", "big", "--pretty").stdout
    assert "Peak memory (RSS):" in text and out["run_id"] in text, text
    rows = ok(barca(tmp_path, "sql", "select node_id, max_rss_bytes from barca.materializations",
                    "--json"))["rows"]
    assert {r["node_id"].split(":")[1]: r["max_rss_bytes"] for r in rows} == rss


# ─── #627.5: a required --env, and a note when there are several ─────────────

ONE = "from barca import asset\n\n@asset()\ndef a() -> int:\n    return 1\n"


def test_require_env_and_the_note_about_several_environments(tmp_path):
    (tmp_path / "p.py").write_text(ONE)
    (tmp_path / "barca.toml").write_text('default_env = "staging"\n')
    first = barca(tmp_path, "get", "a", "--json")
    assert first.returncode == 0, first.stderr
    assert "[barca] note: no --env given" not in first.stderr  # one environment so far
    assert barca(tmp_path, "get", "a", "--env", "prod", "--json").returncode == 0

    out = barca(tmp_path, "history", "--json")
    assert out.returncode == 0, out.stderr
    assert (
        "[barca] note: no --env given, so this uses env staging (default_env in barca.toml); "
        "this project has 2 environments (prod, staging)" in out.stderr
    )
    assert out.stderr.count("[barca] note: no --env given") == 1
    assert "note" not in barca(tmp_path, "history", "--json", "--env", "prod").stderr
    assert "note" not in barca(tmp_path, "history", "--json", BARCA_ENV="prod").stderr

    (tmp_path / "barca.toml").write_text('default_env = "staging"\nrequire_env = true\n')
    for args in (("history",), ("get", "a"), ("status",), ("stats", "a"), ("logs",),
                 ("sql", "select 1"), ("gc", "--dry-run")):
        out = barca(tmp_path, *args)
        assert out.returncode == 2, (args, out.stderr)
        assert "requires one: pass --env NAME or set BARCA_ENV" in out.stderr, out.stderr
        assert "environments here: prod, staging" in out.stderr
    assert barca(tmp_path, "history", "--env", "prod").returncode == 0
    assert barca(tmp_path, "history", BARCA_ENV="prod").returncode == 0
    assert barca(tmp_path, "history", BARCA_REQUIRE_ENV="0").returncode == 0
    assert barca(tmp_path, "plan").returncode == 0  # reads no environment
    assert barca(tmp_path, "list").returncode == 0
    serve = barca(tmp_path, "serve", "--port", "0")
    assert serve.returncode == 2, serve.stderr

    (tmp_path / "barca.toml").write_text("")
    out = barca(tmp_path, "history", BARCA_REQUIRE_ENV="1")
    assert out.returncode == 2 and "BARCA_REQUIRE_ENV requires one" in out.stderr, out.stderr
    assert barca(tmp_path, "history", BARCA_REQUIRE_ENV="maybe").returncode == 2


# ─── #627.6 and #628.7a: the nondeterminism warning ──────────────────────────

CHANGING = """
import time
from barca import asset, sensor


@sensor()
def clock() -> tuple[bool, float]:
    return True, time.time()


@asset(inputs={"t": clock})
def from_clock(t: float) -> float:
    return t


@asset(deterministic=False)
def snapshot() -> float:
    return time.time()


@asset()
def careless() -> float:
    return time.time()
"""


def test_deterministic_false_and_sensors_do_not_warn(tmp_path):
    (tmp_path / "p.py").write_text(CHANGING)
    for _ in range(2):
        out = barca(tmp_path, "get", "--json")
        assert out.returncode == 0, out.stderr
        assert NOT_DETERMINISTIC not in out.stderr
    refresh = barca(tmp_path, "get", "--refresh", "snapshot,careless", "--json")
    assert refresh.returncode == 0, refresh.stderr
    warned = [line for line in refresh.stderr.splitlines() if NOT_DETERMINISTIC in line]
    assert warned == [f"[barca] warning: p.py:careless {NOT_DETERMINISTIC}; the function is not "
                      "deterministic"], refresh.stderr
    steps = {s["id"].split(":")[1]: s for s in json.loads(refresh.stdout)["steps"]}
    assert steps["careless"].get("bytes_changed") is True
    assert not steps["snapshot"].get("bytes_changed")
    # Caching is unchanged: without --refresh the snapshot is served from cache.
    again = ok(barca(tmp_path, "get", "snapshot", "--json"))
    [step] = [s for s in again["steps"] if s["id"].endswith(":snapshot")]
    assert step["status"] == "cached", step
    # Not part of the run hash: removing it recomputes nothing.
    (tmp_path / "p.py").write_text(CHANGING.replace("@asset(deterministic=False)", "@asset()"))
    after = ok(barca(tmp_path, "get", "snapshot", "--json"))
    [step] = [s for s in after["steps"] if s["id"].endswith(":snapshot")]
    assert step["status"] == "cached", step


def test_deterministic_must_be_a_literal_bool(tmp_path):
    (tmp_path / "p.py").write_text(ONE.replace("@asset()", "@asset(deterministic='no')"))
    out = barca(tmp_path, "plan", "p.py")
    assert out.returncode == 2
    assert "deterministic= must be True or False, got `'no'`" in out.stderr, out.stderr


# ─── #628.7b: computed partition keys ────────────────────────────────────────

COMPUTED = """
from barca import asset, partitions
from regions import REGIONS


@asset(partitions={"region": partitions(sorted(REGIONS))})
def sales(region: str) -> str:
    return region
"""


def test_computed_keys_import_project_modules_and_report_failures(tmp_path):
    pipelines = tmp_path / "pipelines"
    pipelines.mkdir()
    (tmp_path / "barca.toml").write_text("")
    # A module at the project root, imported from a file in a subdirectory: a worker finds it
    # through the project root on sys.path, and so does the key evaluation.
    (tmp_path / "regions.py").write_text('REGIONS = ["us", "eu"]\n')
    (pipelines / "sales.py").write_text(COMPUTED)
    out = barca(tmp_path, "get", "sales", "--json")
    assert out.returncode == 0, out.stderr
    assert set(json.loads(out.stdout)["final_output"]) == {"region=eu", "region=us"}

    (tmp_path / "regions.py").write_text("REGIONS = undefined_name\n")
    for args in (("list",), ("status",), ("get", "sales"), ("plan",)):
        out = barca(tmp_path, *args, "--json")
        assert out.returncode == 2, (args, out.stderr)
        envelope = json.loads(out.stderr.strip().splitlines()[-1])
        assert envelope["error"] == (
            "could not load the partition keys of pipelines/sales.py:sales: "
            "partitions(sorted(REGIONS)) for dimension 'region' raised NameError: name "
            "'undefined_name' is not defined"
        ), envelope
        assert 'regions.py", line 1, in <module>' in envelope["remediation"]
        assert "_source_import" not in envelope["remediation"]
    # Under serve, the node is a load error on /health and the rest of the file is served.
    (tmp_path / "pipeline.py").write_text(
        COMPUTED + "\n\n@asset()\ndef other() -> int:\n    return 1\n"
    )
    with served.Serve(tmp_path, env=_env()) as serve:
        health = serve.wait("the server", lambda: serve.get("/health"))
        [error] = health["load_errors"]
        assert error["affected_nodes"] == ["pipeline.py:sales"], error
        assert "NameError: name 'undefined_name' is not defined" in error["error"]
        assert [n["id"] for n in serve.get("/list")["nodes"]] == ["pipeline.py:other"]
