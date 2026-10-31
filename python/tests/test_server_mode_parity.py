"""`--server` prints what a local run prints (RFC-0007 §4.1 "Output parity is the contract").

The CLI contract's case table (test_cli_contract.py) runs twice on two copies of the same
project: once locally on copy A, and once with `--server` against `barca serve` on copy B, in
the same order, so both see the same cache and history at every step. For every case the
normalized stdout JSON (or text), the stderr line sequence and the exit code must be equal.
The planning-pipeline fixture (#250) runs a shorter table the same way.

Normalized, because they differ by design (the declared differences, listed in
`barca docs server-mode`):

- `root`, artifact paths and tracebacks name the server's project directory: both roots read
  `<root>`.
- `triggered_by` is `manual` for a run typed here and `api` for a run started over HTTP.
- Lines the Python workers write to stderr themselves (`[barca] importing <module>`) go to the
  server's stderr: they are dropped from both sides.
- Run ids, timestamps and durations differ between any two runs; they read `<run_id>`, `<t>`
  and `<n>s`. A step's `import_seconds` is reported only when its import took over a second,
  so whether the key is there at all depends on load: it is dropped from both sides.
- The hint of `barca history RUN_ID` names the server it read (`--server <url>`): dropped.
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
import subprocess
from pathlib import Path

import pytest
from barca.api import _find_binary

from .planning_pipeline import write_planning_pipeline
from .test_cli_contract import AGENT_RUNS, CASES, PIPELINE
from .test_serve_robustness import _env, _free_port, wait_for

TIMEOUT = 300

# The commands that take `--server` and print what the server prints. `sql` takes it too, but
# only resolves its views there and reads the results from the store the two machines share,
# which these two copies do not: test_sql_server.py compares it with a local `barca sql`.
SERVER_COMMANDS = {"get", "run", "status", "list", "history", "stats", "logs"}
# Contract cases that only exist on this machine: `--reconcile` writes local history.
LOCAL_ONLY = {"history_reconciled"}

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
    ("pretty_history_failed", ["history", "--status", "failed", "--pretty", "-l", "2"]),
    ("history_bad_status", ["history", "--status", "nope", "--json"]),
    ("run_detail_failed", ["history", "FAILED_RUN", "--json"]),
    ("pretty_run_detail", ["history", "FAILED_RUN", "--pretty"]),
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
    s = TRIGGER.sub("triggered by <trigger>", s)
    s = TIMESTAMP.sub("<t>", s)
    s = RUN_ID.sub("<run_id>", s)
    # Pretty memory measurements vary per process just like JSON *_rss_bytes; sizes of
    # independently serialized artifacts may vary too, as `_value` already allows.
    s = BYTES.sub("<bytes>", s)
    return DURATION.sub("<n>s", s)


def _value(v, roots: list[str], key: str = ""):
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
    if isinstance(v, dict) and v.get("partitioned") is True:
        v = {**v, "last_materialization": "<key>", "shape": "<key>"}
    if isinstance(v, dict):
        return {k: _value(x, roots, k) for k, x in v.items() if k != "import_seconds"}
    if isinstance(v, list):
        return [_value(x, roots) for x in v]
    if key == "triggered_by" and v in ("manual", "api"):
        return "<trigger>"
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
    return out


def _parity_env() -> dict[str, str]:
    return {**_env(), "BARCA_POOL_SIZE": "1"}


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
    return subprocess.run(
        [_find_binary(), *argv],
        check=False,
        cwd=cwd,
        capture_output=True,
        text=True,
        env=_parity_env(),
        timeout=TIMEOUT,
    )


class Served:
    """`barca serve` on a project copy, on a free port, with no scheduler."""

    def __init__(self, root: Path, *files: str) -> None:
        self.port = _free_port()
        self.url = f"http://127.0.0.1:{self.port}"
        self.log = root / "serve.log"
        self.proc = subprocess.Popen(
            [_find_binary(), "serve", *files, "--port", str(self.port), "--no-schedule"],
            cwd=root,
            stdout=subprocess.DEVNULL,
            stderr=self.log.open("w"),
            env=_parity_env(),
        )
        wait_for(self._up, "the server to answer /health")

    def _up(self) -> bool:
        assert self.proc.poll() is None, f"the server exited early:\n{self.log.read_text()}"
        out = _run(self.log.parent, ["history", "--server", self.url, "--json", "-l", "1"])
        return out.returncode == 0

    def stop(self) -> None:
        if self.proc.poll() is None:
            self.proc.terminate()
        try:
            self.proc.wait(timeout=30)
        except subprocess.TimeoutExpired:
            self.proc.kill()
            self.proc.wait()


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


# ─── what only a client does ─────────────────────────────────────────────────


@pytest.fixture()
def served(tmp_path):
    root = tmp_path / "served"
    root.mkdir()
    (root / "pipeline.py").write_text(PIPELINE)
    server = Served(root, "pipeline.py")
    yield server
    server.stop()


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
        ["gc", "--server", "http://127.0.0.1:1"],
        ["serve", "--local"],
        ["get", "total", "--server", "ftp://nowhere", "--json"],
    ):
        out = _run(tmp_path, argv)
        assert out.returncode == 2, (argv, out.stderr)


def test_an_unreachable_server_is_an_infra_error_naming_it(tmp_path):
    url = f"http://127.0.0.1:{_free_port()}"
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
    import signal
    import time

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
        wait_for(
            lambda: any(r.get("status") == "running" for r in _server_runs(server.url)),
            "the run to start on the server",
        )
        started = time.monotonic()
        for _ in range(presses):
            client.send_signal(signal.SIGINT)
            time.sleep(0.2)
        _, stderr = client.communicate(timeout=30)
        assert client.returncode == 130, stderr
        assert time.monotonic() - started < 30, "the run was cancelled, not waited for"
        envelope = json.loads(stderr.splitlines()[-1])
        assert envelope["kind"] == "cancelled"
        # Either way the server stops the run.
        wait_for(
            lambda: all(r.get("status") == "cancelled" for r in _server_runs(server.url)),
            "the server to record the run as cancelled",
        )
    finally:
        server.stop()
