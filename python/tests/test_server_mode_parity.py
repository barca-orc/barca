"""`--server` prints what a local run prints (RFC-0007 §4.1 "Output parity is the contract").

The CLI contract's case table (test_cli_contract.py) runs twice on two copies of the same
project: once locally on copy A, and once with `--server` against `barca serve` on copy B, in
the same order, so both see the same cache and history at every step. For every case the
normalized stdout JSON (or text), the stderr line sequence and the exit code must be equal.
The planning-pipeline fixture (#250) runs a shorter table the same way.

Normalized, because they differ by design (the declared differences, listed in
`barca docs server-mode`):

- Cached-only `path_scope` identifies local versus server ownership. Artifact locators
  compare their hash/format filename, whether original or retained for a concurrent read.
- Step reports and contiguous agent step lines compare by node; independent streams can
  finish in different orders, including when one fails beside cached steps.
- `root`, artifact paths and tracebacks name the server's project directory: both roots read
  `<root>`.
- `triggered_by` is `manual` for a run typed here and `api` for a run started over HTTP.
- Lines the Python workers write to stderr themselves (`[barca] importing <module>`) go to the
  server's stderr: they are dropped from both sides.
- Run ids, timestamps and durations differ between any two runs; they read `<run_id>`, `<t>`
  and `<n>s`. A step's `import_seconds` is reported only when its import took over a second,
  so whether the key is there at all depends on load: it is dropped from both sides.
- The hint of `barca history RUN_ID` names the server it read (`--server <url>`): dropped.
- The commands of a rollup root's action (`roots[].action.command` and `run_once`) end in
  `--server` when a server answered, so the run joins that server's queue: the suffix is dropped.
- `disk_usage` measures each host's storage and cache budget, not graph equivalence. Its
  fields and accounting invariants are checked, then host-specific counts are normalized.

Each copy computes its own artifacts, so an artifact's `size_bytes` and `content_hash` read
`<size_bytes>` and `<content_hash>` (the planning pipeline writes parquet whose bytes vary).
Which key of a partitioned node `barca status` shows as its last materialization depends on the
order a run recorded its keys in, which is not fixed; for a partitioned node it reads `<key>`.

The "no .py files" usage error (the server has its files) and the progress bar (a terminal
only; these runs are piped) do not arise here.
"""

from __future__ import annotations

import json
import re
import signal
import subprocess
import time
from pathlib import Path

import pytest
from barca.api import _find_binary

from . import helpers
from .helpers import QUICK, SERVE, clean_env, free_port, wait_until
from .planning_pipeline import write_planning_pipeline
from .served import Serve
from .test_cli_contract import AGENT_RUNS, CASES, PIPELINE

# The commands that take `--server` and print what the server prints. `sql` takes it too, but
# only resolves its views there and reads the results from the store the two machines share,
# which these two copies do not: test_sql_server.py compares it with a local `barca sql`.
SERVER_COMMANDS = {"get", "run", "status", "list", "history", "stats", "logs"}
# Contract cases that only exist on this machine: `--reconcile` writes local history.
# `history --activity` counts runs, and the two sides do not record the same ones (a server
# records a request it refuses as a run with status `error`; the CLI records none for a usage
# error): test_serve_running.py compares it with the server's own `GET /runs` instead, and the
# table below compares the count that is the same on both sides, the schedule's (none).
LOCAL_ONLY = {"history_reconciled", "history_activity"}

CONTRACT_TABLE: list[tuple[str, list[str]]] = [
    (case, argv)
    for case, argv, _, _ in CASES
    if argv[0] in SERVER_COMMANDS and case not in LOCAL_ONLY
]
CONTRACT_TABLE += [(f"agent_{i}", argv) for i, argv in enumerate(AGENT_RUNS)]
# The human renderings, rebuilt here from the server's JSON.
CONTRACT_TABLE += [
    ("pretty_get", ["get", "total", "pipeline.py", "--pretty"]),
    ("value_get", ["get", "total", "pipeline.py", "-o", "value"]),
    ("value_pointer", ["get", "frame", "pipeline.py", "-o", "value"]),
    ("pretty_multi", ["get", "total,frame", "pipeline.py", "--pretty"]),
    ("pretty_dry_run", ["get", "total", "pipeline.py", "--dry-run", "--pretty"]),
    ("pretty_run_logs", ["run", "report", "pipeline.py", "--pretty"]),
    ("logs_latest", ["logs", "--json"]),
    ("logs_step_text", ["logs", "--step", "report", "--timestamps", "--pretty"]),
    ("pretty_run_failed", ["run", "broken", "pipeline.py", "--pretty"]),
    ("logs_stderr", ["logs", "--stream", "stderr", "--json"]),
    ("logs_follow_ended", ["logs", "-f", "--pretty"]),
    ("logs_unknown_step", ["logs", "--step", "nope", "--json"]),
    ("pretty_unknown", ["get", "nope", "pipeline.py", "--pretty"]),
    ("pretty_list", ["list", "pipeline.py", "--pretty"]),
    ("pretty_groups", ["list", "pipeline.py", "--groups", "--pretty"]),
    ("pretty_status", ["status", "total", "pipeline.py", "--pretty"]),
    ("pretty_history", ["history", "--pretty", "-l", "3"]),
    ("pretty_stats", ["stats", "total", "pipeline.py", "--pretty"]),
    ("json_fields", ["get", "total", "pipeline.py", "--json", "--fields", "id,status"]),
    ("status_fields", ["status", "pipeline.py", "--fields", "id,cache", "--limit", "2"]),
    ("whole_project", ["get", "--json"]),
    ("pretty_partition_dry_run", ["get", "--partition", "k=a", "--dry-run", "--pretty"]),
    ("run_partition_no_dimension", ["run", "report", "pipeline.py", "--partition", "k=a"]),
    ("get_partition_skipped_target", ["get", "keys", "pipeline.py", "--partition", "k=a"]),
    ("stats_unknown", ["stats", "nope", "pipeline.py", "--json"]),
    # History filters and one run in detail. `FAILED_RUN` is the newest failed run of the side
    # the case runs on.
    ("history_failed", ["history", "--status", "failed", "--json"]),
    ("history_filters", ["history", "--status", "success", "--target", "total", "--since", "1d"]),
    ("history_no_schedule", ["history", "--triggered-by", "schedule", "--json"]),
    # #991: the runs in progress (none here) and the last day's activity.
    ("history_running", ["history", "--status", "running", "--json"]),
    ("history_activity_schedule", ["history", "--activity", "--triggered-by", "schedule"]),
    # #969: the filters the database applies. Each names a status both sides record the same
    # runs of (a server also records a refused request, as `error`), so the counts agree.
    (
        "history_not_schedule",
        ["history", "--status", "failed", "--not-triggered-by", "schedule", "--json"],
    ),
    (
        "history_only_others",
        ["history", "--status", "success", "--not-triggered-by", "manual,api,client", "--json"],
    ),
    ("history_search", ["history", "--status", "success", "--search", "TOTAL", "--json"]),
    ("history_search_none", ["history", "--search", "no_such_node", "--pretty"]),
    (
        "history_window",
        ["history", "--status", "success", "--since", "1d", "--until", "2999-01-01", "--pretty"],
    ),
    ("history_until_past", ["history", "--until", "2000-01-01", "--json"]),
    ("history_bad_not_trigger", ["history", "--not-triggered-by", "cron", "--json"]),
    ("history_bad_until", ["history", "--until", "soon", "--json"]),
    (
        "history_activity_failed_schedule",
        ["history", "--activity", "--status", "failed", "--triggered-by", "schedule", "--json"],
    ),
    ("history_activity_search", ["history", "--activity", "--search", "no_such_node"]),
    ("pretty_history_failed", ["history", "--status", "failed", "--pretty", "-l", "2"]),
    ("history_bad_status", ["history", "--status", "nope", "--json"]),
    ("run_detail_failed", ["history", "FAILED_RUN", "--json"]),
    ("pretty_run_detail", ["history", "FAILED_RUN", "--pretty"]),
    # One partitioned node's keys (#990): the list, a state filter, the grid, one key, and the
    # usage errors, as tables (the JSON forms are contract cases).
    ("pretty_status_keys", ["status", "flaky", "pipeline.py", "--keys", "--pretty"]),
    ("status_keys_failed", ["status", "flaky", "pipeline.py", "--keys", "--state", "failed"]),
    ("status_keys_limit", ["status", "per_key", "pipeline.py", "--keys", "--limit", "1"]),
    ("pretty_status_grid", ["status", "flaky", "pipeline.py", "--grid", "--pretty"]),
    ("pretty_status_key", ["status", "flaky", "pipeline.py", "--key", "k=bad", "--pretty"]),
    ("status_key_sample", ["status", "per_key", "pipeline.py", "--key", "k=a", "--sample", "1"]),
    ("status_keys_unpartitioned", ["status", "total", "pipeline.py", "--keys", "--json"]),
    ("status_key_missing", ["status", "flaky", "pipeline.py", "--key", "k=nope", "--json"]),
    ("status_keys_bad_state", ["status", "flaky", "pipeline.py", "--keys", "--state", "cached"]),
    ("status_keys_no_target", ["status", "pipeline.py", "--grid", "--json"]),
]

PLANNING_TABLE: list[tuple[str, list[str]]] = [
    ("list", ["list", "assets.py", "--json"]),
    ("dry_run", ["get", "assets.py", "--dry-run", "--json"]),
    ("tasks", ["run", "TASKS", "assets.py", "--json", "--agent"]),
    ("get_all", ["get", "assets.py", "--json"]),
    ("cached", ["get", "assets.py", "--json", "--agent"]),
    ("status", ["status", "assets.py", "--json"]),
    ("history", ["history", "--json"]),
]

RUN_ID = re.compile(r"\b[0-9a-f]{12}\b")
DURATION = re.compile(r"\b\d+(\.\d+)?(ms|s)\b")
BYTES = re.compile(r"\b\d+(?:\.\d+)? (?:B|KB|MB|GB|TB)\b")
TIMESTAMP = re.compile(r"\d{4}-\d{2}-\d{2}[T ]\d{2}:\d{2}:\d{2}(\.\d+)?(Z|[+-]\d{2}:?\d{2})?")


TRIGGER = re.compile(r"\btriggered by (manual|api)\b")


def _text(s: str, roots: list[str]) -> str:
    for root in roots:
        s = s.replace(root, "<root>")
    # Concurrent cache reads retain an inode under sql-cache; ordinary execution returns
    # the original artifact. Compare the same hash/format, independent of its host locator.
    s = re.sub(
        r"(?:<root>/)?\.barca/(?:sql-cache/cached-[^/]+|artifacts/[^/]+)/([0-9a-f]{64}\.[a-z0-9]+)",
        r"<artifact>/\1",
        s,
    )
    s = TRIGGER.sub("triggered by <trigger>", s)
    s = TIMESTAMP.sub("<t>", s)
    s = RUN_ID.sub("<run_id>", s)
    # Pretty memory measurements vary per process just like JSON *_rss_bytes; sizes of
    # independently serialized artifacts may vary too, as `_value` already allows.
    s = BYTES.sub("<bytes>", s)
    # A table pads its columns to the widest cell, and a duration's width varies per process.
    return re.sub(r" {2,}", "  ", DURATION.sub("<n>s", s))


def _value(v, roots: list[str], key: str = ""):
    if key == "path_scope":
        assert v in ("local", "server")
        return "<host>"
    if key == "disk_usage" and isinstance(v, dict):
        assert set(v) == {"size_bytes", "budget_bytes", "backup_bytes", "limit_bytes"}
        assert all(
            x is None or (isinstance(x, int) and not isinstance(x, bool) and x >= 0)
            for x in v.values()
        )
        measured = v["budget_bytes"] is not None and v["backup_bytes"] is not None
        assert (v["size_bytes"] is not None) == measured
        if measured:
            assert v["size_bytes"] == v["budget_bytes"] + v["backup_bytes"]
        return {k: "<host-bytes>" for k in v}
    if key == "steps_page" and isinstance(v, dict):
        # The server completes a finished run's nodes with the planned ones it skipped or found
        # cached; the local `history RUN_ID` lists only the nodes with steps (server-mode.md).
        v = {k: x for k, x in v.items() if k != "nodes"}
    if key == "explanation" and isinstance(v, dict) and isinstance(v.get("steps"), str):
        # Skipped nodes are in the per-node summary only the server has (see `steps_page`).
        v = {**v, "steps": re.sub(r" · [\d,]+ skipped", "", v["steps"])}
    if key in ("attempted_at", "elapsed_ms", "size_bytes") and isinstance(v, list):
        # The per-key columns of `status --grid --json`: times and sizes, as elsewhere.
        return [None if x is None else f"<{key}>" for x in v]
    if key == "attempted_base":
        return "<t>"
    if isinstance(v, dict) and v.get("partitioned") is True:
        v = {**v, "last_materialization": "<key>", "shape": "<key>"}
    if isinstance(v, dict):
        return {k: _value(x, roots, k) for k, x in v.items() if k != "import_seconds"}
    if isinstance(v, list):
        if key == "steps" and all(isinstance(x, dict) and "id" in x for x in v):
            v = sorted(v, key=lambda x: x["id"])
        return [_value(x, roots) for x in v]
    if key == "triggered_by" and v in ("manual", "api"):
        return "<trigger>"
    if key == "requested_by" and v in ("manual", "api"):
        # The explanation's wording of `triggered_by`.
        return "<trigger>"
    if key in ("command", "run_once") and isinstance(v, str):
        # A root's action names the server when a server answered.
        return _text(v.removesuffix(" --server"), roots)
    if key in ("size_bytes", "content_hash"):
        # Each copy computes its own bytes: a step whose output is not byte-for-byte the same
        # every time (parquet metadata, timestamps in the data) differs in size and hash.
        return f"<{key}>"
    if key.endswith(("seconds", "_at")):
        return "<t>" if v is not None else None
    if key.endswith("rss_bytes"):
        # Peak memory is measured per process, like a time.
        return "<rss>" if v is not None else None
    if isinstance(v, str):
        return _text(v, roots)
    return v


def _normalize(text: str, roots: list[str]):
    """A stream as comparable values: each line that is JSON as its normalized value (a whole
    pretty-printed document as one), the rest as normalized text."""
    try:
        return [_value(json.loads(text), roots)]
    except json.JSONDecodeError:
        pass
    out = []
    for line in text.splitlines():
        if line.startswith("[barca] importing "):
            continue
        try:
            out.append(_value(json.loads(line), roots))
        except json.JSONDecodeError:
            out.append(_text(line, roots))
    # Only reorder contiguous per-step progress: retain warnings, summaries, errors and
    # their boundaries, while independent streams may finish on either side of a cache hit.
    start = 0
    while start < len(out):
        end = start
        while end < len(out) and isinstance(out[end], str) and out[end].startswith("[barca] step:"):
            end += 1
        out[start:end] = sorted(out[start:end])
        start = max(start + 1, end)
    return out


def _parity_env() -> dict[str, str]:
    return clean_env(BARCA_POOL_SIZE="1")


def _first_diff(a, b, path: str = "") -> str | None:
    """Where two normalized values first differ, for a readable failure."""
    if type(a) is not type(b):
        return f"{path or '.'}: {a!r} != {b!r}"
    if isinstance(a, dict):
        for k in sorted(set(a) | set(b)):
            if k not in a or k not in b:
                return f"{path}.{k}: only {'local' if k in a else 'server'}"
            if (d := _first_diff(a[k], b[k], f"{path}.{k}")) is not None:
                return d
        return None
    if isinstance(a, list):
        for i, (x, y) in enumerate(zip(a, b)):
            if (d := _first_diff(x, y, f"{path}[{i}]")) is not None:
                return d
        return None if len(a) == len(b) else f"{path}: {len(a)} items != {len(b)}"
    return None if a == b else f"{path or '.'}: {a!r} != {b!r}"


def _run(cwd: Path, argv: list[str]) -> subprocess.CompletedProcess:
    return helpers.barca(cwd, *argv, env=_parity_env(), timeout=SERVE)


def Served(root: Path, *files: str) -> Serve:
    """`barca serve` on a project copy, on a free port, with no scheduler."""
    return Serve(root, "--no-schedule", files=files, env=_parity_env())


def _resolve(cwd: Path, argv: list[str], server: list[str]) -> list[str]:
    """`argv` with `FAILED_RUN` replaced by the newest failed run that side recorded."""
    if "FAILED_RUN" not in argv:
        return argv
    out = _run(cwd, ["history", "--status", "failed", "-l", "1", "--json", *server])
    [run] = json.loads(out.stdout)["runs"]
    return [run["run_id"] if a == "FAILED_RUN" else a for a in argv]


def _parity(local_root: Path, served_root: Path, table, files: tuple[str, ...]):
    """Run `table` locally on `local_root`, then with `--server` against `served_root`; return
    each case whose normalized output differs."""
    spellings = {str(p) for r in (local_root, served_root) for p in (r, r.resolve())}
    roots = sorted(spellings, key=len, reverse=True)
    local = [(case, _run(local_root, _resolve(local_root, argv, []))) for case, argv in table]
    server = Served(served_root, *files)
    flag = ["--server", server.url]
    try:
        remote = [
            (case, _run(local_root, [*_resolve(local_root, argv, flag), *flag]))
            for case, argv in table
        ]
    finally:
        server.stop()
    # A hint names the server it read (`barca history RUN_ID`'s `barca logs ... --server URL`).
    named = f" --server {server.url}"
    diffs = []
    for (case, here), (_, there) in zip(local, remote, strict=True):
        there_out = there.stdout.replace(named, "")
        there_err = there.stderr.replace(named, "")
        a = (here.returncode, _normalize(here.stdout, roots), _normalize(here.stderr, roots))
        b = (there.returncode, _normalize(there_out, roots), _normalize(there_err, roots))
        if a != b:
            diffs.append(
                f"--- {case}\nexit: {a[0]} local, {b[0]} server\n"
                f"stdout differs at {_first_diff(a[1], b[1])}\n"
                f"stderr differs at {_first_diff(a[2], b[2])}\n"
                f"stdout local:  {a[1]}\nstdout server: {b[1]}\n"
                f"stderr local:  {a[2]}\nstderr server: {b[2]}\n"
            )
    return local, diffs


def test_the_contract_case_table_prints_the_same_locally_and_through_a_server(tmp_path):
    local_root, served_root = tmp_path / "local", tmp_path / "served"
    for root in (local_root, served_root):
        root.mkdir()
        (root / "pipeline.py").write_text(PIPELINE)
    local, diffs = _parity(local_root, served_root, CONTRACT_TABLE, ("pipeline.py",))
    assert not diffs, "\n".join(diffs)
    # The table covers success, a failed step, a usage error and every inspection command.
    codes = {case: out.returncode for case, out in local}
    assert codes["get"] == 0 and codes["run_failed"] == 1 and codes["error_usage"] == 2
    assert codes["error_usage_parse"] == 2


def test_the_planning_pipeline_prints_the_same_locally_and_through_a_server(tmp_path):
    local = write_planning_pipeline(tmp_path / "local")
    served = write_planning_pipeline(tmp_path / "served")
    tasks = ",".join(local.names("task"))
    table = [(case, [tasks if a == "TASKS" else a for a in argv]) for case, argv in PLANNING_TABLE]
    results, diffs = _parity(local.root, served.root, table, ("assets.py",))
    assert not diffs, "\n".join(diffs)
    assert all(out.returncode == 0 for _, out in results), [out.stderr for _, out in results]


# `day` (3 values) by `region` (2): one key of `events` fails with one error, another with a
# second, so the same keys of `scored` wait on them; `wide` has more keys than a server lists in
# one page, and never ran.
KEYS_PIPELINE = """
from barca import asset, partitions

DAYS = ["d1", "d2", "d3"]
REGIONS = ["emea", "apac"]
MANY = [f"k{i:04}" for i in range(1203)]


@asset(partitions={"day": partitions(DAYS), "region": partitions(REGIONS)})
def events(day: str, region: str) -> list:
    if day == "d2" and region == "apac":
        raise ValueError(f"no rows for {region} on {day}")
    if day == "d3" and region == "emea":
        raise KeyError("price")
    return [{"day": day, "region": region}]


@asset(partitions={"day": partitions(DAYS), "region": partitions(REGIONS)}, inputs={"events": events})
def scored(day: str, region: str, events: list) -> list:
    return [{**row, "score": 1} for row in events]


@asset(partitions={"k": partitions(MANY)})
def wide(k: str) -> str:
    return k
"""

# Every filter, order and page `barca status NODE --keys` takes (#1036), as text and as JSON,
# then the overview: each printed here and through a server.
KEYS_TABLE: list[tuple[str, list[str]]] = [
    ("keys_all", ["--keys"]),
    ("keys_state", ["--keys", "--state", "failed,blocked"]),
    ("keys_needs_run", ["--keys", "--needs-run"]),
    ("keys_value", ["--keys", "--value", "region=apac"]),
    ("keys_values", ["--keys", "--value", "region=apac,emea", "--value", "day=d2,d3"]),
    ("keys_range", ["--keys", "--range", "day=d3..d2"]),
    ("keys_error", ["--keys", "--state", "failed", "--error", "1"]),
    ("keys_match", ["--keys", "--match", "D2"]),
    ("keys_sort_state", ["--keys", "--sort", "state"]),
    ("keys_sort_attempted", ["--keys", "--sort", "attempted", "--state", "fresh"]),
    ("keys_page", ["--keys", "--limit", "2"]),
    ("keys_next_page", ["--keys", "--limit", "2", "--offset", "2"]),
    ("keys_last_page", ["--keys", "--limit", "2", "--offset", "4"]),
    (
        "keys_every_filter",
        [
            "--keys",
            "--needs-run",
            "--value",
            "region=emea",
            "--range",
            "day=d1..d3",
            "--match",
            "d3",
            "--sort",
            "elapsed",
        ],
    ),
    ("keys_none", ["--keys", "--state", "pinned"]),
    ("overview", ["--overview"]),
    ("overview_rows", ["--overview", "--limit", "1"]),
]
KEYS_ERRORS: list[tuple[str, list[str], str]] = [
    ("no_dimension", ["--keys", "--value", "zone=a"], "no dimension 'zone'"),
    ("no_range_value", ["--keys", "--range", "day=d1..d9"], "no day 'd9'"),
    ("no_error_group", ["--keys", "--error", "5"], "--error takes 0 to 1"),
]


def test_the_keys_of_a_node_print_the_same_locally_and_through_a_server(tmp_path, serve):
    """`barca status NODE --keys` with each filter, order and page, and `--overview`: a server
    filters, orders and pages the keys itself (`GET /assets/{name}/partitions/keys`,
    `.../partitions/overview`), and the CLI prints the same as it does on this machine, where
    the same `barca-service` function answers. One project, read both ways: the keys, their
    attempts and their run ids are the same rows."""
    root = tmp_path / "project"
    root.mkdir()
    (root / "pipeline.py").write_text(KEYS_PIPELINE)
    (root / "barca.toml").write_text("")

    def cli(*argv: str) -> subprocess.CompletedProcess:
        return subprocess.run(
            [_find_binary(), *argv],
            cwd=root,
            env=_parity_env(),
            capture_output=True,
            text=True,
            timeout=120,
        )

    assert cli("get", "scored", "pipeline.py", "--json").returncode == 1
    views = [
        (f"{case}_{node}_{form}", ["status", node, *argv, f"--{form}"])
        for case, argv in KEYS_TABLE
        for node in ("events", "scored")
        for form in ("json", "pretty")
        # `scored` has no failed key of its own, so no error group to ask for.
        if (case, node) != ("keys_error", "scored")
    ]
    # More keys than one page of the server's: the CLI reads page after page and prints one list.
    views += [
        ("wide_all", ["status", "wide", "--keys", "--all", "--json"]),
        ("wide_limit", ["status", "wide", "--keys", "--limit", "1100", "--offset", "50", "--json"]),
        ("wide_table", ["status", "wide", "--keys", "--pretty"]),
    ]
    here = [(case, cli(*argv)) for case, argv in views]
    here_errors = [cli("status", "events", *argv, "--json") for _, argv, _ in KEYS_ERRORS]
    server = serve("--no-schedule", root=root, env=_parity_env())
    there = [(case, cli(*argv, "--server", server.url)) for case, argv in views]
    there_errors = [
        cli("status", "events", *argv, "--json", "--server", server.url)
        for _, argv, _ in KEYS_ERRORS
    ]
    for (case, a), (_, b) in zip(here, there, strict=True):
        assert a.returncode == 0 and b.returncode == 0, (case, a.stderr, b.stderr)
        assert a.stdout == b.stdout, case
        # The note of a table cut short (`showing 2 of 6 keys; ...`) is the same line.
        notes = [[line for line in out.stderr.splitlines() if "showing" in line] for out in (a, b)]
        assert notes[0] == notes[1], case
    by_case = {case: out for case, out in here}
    # What the filters keep, so the equality above is not two empty lists.
    keys = lambda case: [k["key"] for k in json.loads(by_case[case].stdout)["keys"]]  # noqa: E731
    assert keys("keys_state_events_json") == ["day=d2,region=apac", "day=d3,region=emea"]
    assert keys("keys_state_scored_json") == ["day=d2,region=apac", "day=d3,region=emea"]
    assert keys("keys_range_events_json") == [
        f"day={d},region={r}" for d in ("d2", "d3") for r in ("emea", "apac")
    ]
    assert len(keys("keys_error_events_json")) == 1
    assert keys("keys_match_events_json") == ["day=d2,region=emea", "day=d2,region=apac"]
    assert keys("keys_next_page_events_json") == ["day=d2,region=emea", "day=d2,region=apac"]
    assert keys("keys_every_filter_events_json") == ["day=d3,region=emea"]
    assert keys("keys_none_events_json") == []
    page = json.loads(by_case["keys_page_events_json"].stdout)
    assert (page["total"], page["truncated"], page["offset"], page["needs_run"]) == (6, True, 0, 2)
    overview = json.loads(by_case["overview_scored_json"].stdout)
    assert [(i["state"], i["keys"]) for i in overview["issues"]] == [("blocked", 2)]
    assert [r["states"] for r in overview["rows"]] == ["ccb", "cbc"]
    wide = json.loads(by_case["wide_all"].stdout)
    assert len(wide["keys"]) == 1203 and wide["truncated"] is False and "hint" not in wide
    limited = json.loads(by_case["wide_limit"].stdout)
    assert len(limited["keys"]) == 1100 and limited["keys"][0]["key"] == "k=k0050"
    assert limited["truncated"] is True and limited["offset"] == 50
    # A filter the node does not have is the same usage error from both.
    for (case, _, text), a, b in zip(KEYS_ERRORS, here_errors, there_errors, strict=True):
        assert a.returncode == 2 and b.returncode == 2, (case, a.stderr, b.stderr)
        assert text in a.stderr and text in b.stderr, (case, a.stderr, b.stderr)


# ─── what only a client does ─────────────────────────────────────────────────


@pytest.fixture()
def served(tmp_path, serve):
    root = tmp_path / "served"
    root.mkdir()
    (root / "pipeline.py").write_text(PIPELINE)
    return serve("--no-schedule", root=root, env=_parity_env())


def test_a_configured_server_is_announced_once_and_bare_server_uses_it(tmp_path, served):
    here = tmp_path / "laptop"
    here.mkdir()
    (here / "pipeline.py").write_text(PIPELINE)
    (here / "barca.toml").write_text(f'[server]\nurl = "{served.url}/"\n')
    out = _run(here, ["get", "total", "--server", "-o", "value", "--json"])
    assert out.returncode == 0, out.stderr
    assert json.loads(out.stdout)["final_output"] == {"total": 6}
    assert out.stderr.splitlines()[0] == f"[barca] server {served.url} ([server] url in barca.toml)"
    assert out.stderr.count("[barca] server ") == 1
    # `--server` before the target is still bare, not a URL named `total`.
    assert _run(here, ["get", "--server", "total", "--json"]).returncode == 0
    # Without --server the command runs here, against the configured server's ledger, which
    # needs the server's store: this project has none, a usage error that names --local.
    ledger = _run(here, ["get", "total", "--json"])
    assert ledger.returncode == 2, ledger.stderr
    assert "--local" in ledger.stderr and "[remote]" in ledger.stderr
    # --local runs here and touches nothing on the server.
    local = _run(here, ["get", "total", "--local", "--json"])
    assert local.returncode == 0 and "[barca] server" not in local.stderr
    assert (here / ".barca").exists()
    # --local with --server is a usage error.
    both = _run(here, ["get", "total", "--server", "--local", "--json"])
    assert both.returncode == 2, both.stderr
    assert json.loads(both.stderr.splitlines()[-1])["kind"] == "usage"


def test_a_named_file_that_differs_from_the_deployment_prints_a_note(tmp_path, served):
    here = tmp_path / "laptop"
    here.mkdir()
    (here / "pipeline.py").write_text(PIPELINE + "\n# edited here\n")
    out = _run(here, ["get", "total", "pipeline.py", "--json", "--server", served.url])
    assert out.returncode == 0, out.stderr
    assert (
        "[barca] note: pipeline.py differs from the deployment; the server runs the deployed version"
        in out.stderr.splitlines()
    )
    missing = _run(here, ["get", "total", "other.py", "--json", "--server", served.url])
    assert missing.returncode == 2
    envelope = json.loads(missing.stderr.splitlines()[-1])
    assert "not one of the files deployed" in envelope["error"]
    assert "pipeline.py" in envelope["remediation"]


def test_bare_server_with_nothing_configured_and_server_on_local_commands_are_usage_errors(
    tmp_path,
):
    for argv in (
        ["get", "total", "--server", "--json"],
        ["plan", "--server"],
        ["state", "backups", "--server", "http://127.0.0.1:1"],
        ["serve", "--local"],
        ["get", "total", "--server", "ftp://nowhere", "--json"],
    ):
        out = _run(tmp_path, argv)
        assert out.returncode == 2, (argv, out.stderr)


def test_an_unreachable_server_is_an_infra_error_naming_it(tmp_path):
    url = f"http://127.0.0.1:{free_port()}"
    out = _run(tmp_path, ["get", "total", "--json", "--server", url])
    assert out.returncode == 3, out.stderr
    envelope = json.loads(out.stderr.splitlines()[-1])
    assert envelope["kind"] == "infra"
    assert url in envelope["error"]
    assert "--server" in envelope["remediation"]
    assert not (tmp_path / ".barca").exists(), "nothing runs here instead"


def test_env_must_be_the_servers(tmp_path, served):
    out = _run(tmp_path, ["history", "--json", "--env", "prod", "--server", served.url])
    assert out.returncode == 2
    envelope = json.loads(out.stderr.splitlines()[-1])
    assert "runs environment 'default'" in envelope["error"]


def test_the_python_api_runs_on_the_server(tmp_path, served, monkeypatch):
    import barca

    monkeypatch.chdir(tmp_path)
    assert barca.get("total", server=served.url) == {"total": 6}
    frame = barca.get("frame", server=served.url)
    assert list(frame["n"]) == [1, 2, 3]
    monkeypatch.setenv("BARCA_SERVER", served.url)
    assert barca.get("total", server=True) == {"total": 6}
    assert barca.run("report", server=True) == {"reported": 6}
    with pytest.raises(barca.api.BarcaError) as err:
        barca.run("broken", server=True)
    assert err.value.kind == "step_failed"


SLOW = """
import time

from barca import asset


@asset()
def slow() -> int:
    time.sleep(60)
    return 1
"""


def _server_runs(url: str) -> list[dict]:
    import urllib.request

    with urllib.request.urlopen(f"{url}/runs", timeout=10) as resp:
        body = json.loads(resp.read())
    return body["runs"] if isinstance(body, dict) else body


@pytest.mark.parametrize("presses", [1, 2])
def test_ctrl_c_cancels_the_run_on_the_server(tmp_path, presses):
    root = tmp_path / "served"
    root.mkdir()
    (root / "pipeline.py").write_text(SLOW)
    server = Served(root, "pipeline.py")
    try:
        client = subprocess.Popen(
            [_find_binary(), "get", "slow", "--json", "--server", server.url],
            cwd=tmp_path,
            stdout=subprocess.PIPE,
            stderr=subprocess.PIPE,
            text=True,
            env=_parity_env(),
        )
        wait_until(
            lambda: any(r.get("status") == "running" for r in _server_runs(server.url)),
            "the run to start on the server",
        )
        started = time.monotonic()
        for _ in range(presses):
            client.send_signal(signal.SIGINT)
            time.sleep(0.2)  # a fixed wait: two presses are two signals, not one burst
        _, stderr = client.communicate(timeout=QUICK)
        assert client.returncode == 130, stderr
        assert time.monotonic() - started < 30, "the run was cancelled, not waited for"
        envelope = json.loads(stderr.splitlines()[-1])
        assert envelope["kind"] == "cancelled"
        # Either way the server stops the run.
        wait_until(
            lambda: all(r.get("status") == "cancelled" for r in _server_runs(server.url)),
            "the server to record the run as cancelled",
        )
    finally:
        server.stop()
