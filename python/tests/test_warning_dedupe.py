"""Repeated library warnings from workers are printed once per run, then counted (#215).

aiohttp logs "Could not parse .netrc file" on every client session; on a remote project that is
hundreds of identical lines burying the progress output. A WARNING log record or a `warnings`
warning with the same text is printed the first time in a run (across all workers) and summarised
at the end as `[barca] N more: <text>`. Only records printed because logging is unconfigured are
collapsed: once the project configures logging, every record is printed. A step's own prints, raw
stderr writes, records at other levels and failure messages are not touched.
"""

import json
import os
import subprocess
from pathlib import Path

from barca.api import _find_binary

NETRC = "Could not parse .netrc file"

# Eight partitions on four workers: the same warning from every worker, five times per step.
PARTITIONED = """
import logging
from barca import asset, collect, partitions

log = logging.getLogger("aiohttp.client")


@asset(partitions={"k": partitions(["k0", "k1", "k2", "k3", "k4", "k5", "k6", "k7"])})
def fetch(k: str) -> dict:
    for _ in range(5):
        log.warning("Could not parse .netrc file")
    return {"k": k}


@asset(inputs={"parts": collect(fetch)})
def total(parts: list[dict]) -> dict:
    return {"n": len(parts)}
"""

# One step that emits the same text through every channel, then a step that fails with it.
MIXED = """
import logging
import sys
import warnings
from barca import asset

log = logging.getLogger("somelib")


@asset()
def noisy() -> dict:
    for _ in range(5):
        log.warning("Could not parse .netrc file")
        log.error("an error record")
        print("same stdout line")
        print("same stderr line", file=sys.stderr)
        sys.stderr.write("Could not parse .netrc file\\n")
    warnings.simplefilter("always")
    for _ in range(3):
        warnings.warn("deprecated thing")
    return {"n": 1}


@asset(inputs={"x": noisy})
def broken(x: dict) -> dict:
    for _ in range(4):
        log.warning("Could not parse .netrc file")
    raise ValueError("Could not parse .netrc file")
"""

# One step, no logging configured: COUNT warnings with the text TEXT.
SINGLE = """
import logging
from barca import asset

log = logging.getLogger("somelib")


@asset()
def noisy() -> dict:
    for i in range(COUNT):
        log.warning(TEXT)
    return {"n": 1}
"""

# The project configures logging with basicConfig; a library logger and its own logger warn.
BASICCONFIG = """
import logging
from barca import asset

logging.basicConfig(format="%(levelname)s %(name)s: %(message)s")
lib = logging.getLogger("aiohttp.client")
log = logging.getLogger("pipeline")


@asset()
def noisy() -> dict:
    for _ in range(6):
        lib.warning("Could not parse .netrc file")
        log.warning("retrying")
    return {"n": 1}
"""

# The step attaches its own handlers: a StreamHandler on stderr and a log file.
OWN_HANDLERS = """
import logging
from barca import asset

log = logging.getLogger("somelib")
log.addHandler(logging.StreamHandler())
log.addHandler(logging.FileHandler("step.log"))


@asset()
def noisy() -> dict:
    for _ in range(6):
        log.warning("Could not parse .netrc file")
    return {"n": 1}
"""


def _barca(tmp: Path, *args: str, **env: str) -> subprocess.CompletedProcess:
    full_env = {**os.environ, "BARCA_PROGRESS_SECS": "0", **env}
    return subprocess.run(
        [_find_binary(), *args], cwd=tmp, capture_output=True, text=True, timeout=120, env=full_env
    )


def _single(count: int, text: str = '"Could not parse .netrc file"') -> str:
    return SINGLE.replace("COUNT", str(count)).replace("TEXT", text)


def _not_barca(stderr: str) -> list[str]:
    """stderr without barca's own lines (`[barca] ...` and the JSON error envelope)."""
    return [ln for ln in stderr.splitlines() if not ln.startswith(("[barca]", "{"))]


def test_repeated_log_warning_is_printed_once_per_run_with_a_count(tmp_path: Path) -> None:
    (tmp_path / "p.py").write_text(PARTITIONED)
    r = _barca(tmp_path, "get", "total", "p.py", "--json", BARCA_POOL_SIZE="4")
    assert r.returncode == 0, r.stderr
    lines = r.stderr.splitlines()
    # 8 partitions x 5 = 40 occurrences over 4 workers: one printed, 39 counted.
    assert lines.count(NETRC) == 1, r.stderr
    assert f"[barca] 39 more: {NETRC}" in lines, r.stderr
    assert json.loads(r.stdout)["final_output"] == {"n": 8}


def test_summary_is_printed_before_the_end_of_run_line(tmp_path: Path) -> None:
    (tmp_path / "p.py").write_text(PARTITIONED)
    r = _barca(tmp_path, "get", "total", "p.py", "--agent", BARCA_POOL_SIZE="4")
    assert r.returncode == 0, r.stderr
    lines = r.stderr.splitlines()
    assert lines[-2] == f"[barca] 39 more: {NETRC}", r.stderr
    assert lines[-1].startswith("[barca] 9/9 steps | done in "), r.stderr


def test_a_warning_seen_once_gets_no_summary(tmp_path: Path) -> None:
    (tmp_path / "p.py").write_text(_single(1))
    r = _barca(tmp_path, "get", "noisy", "p.py")
    assert r.returncode == 0, r.stderr
    assert r.stderr.splitlines().count(NETRC) == 1, r.stderr
    assert " more: " not in r.stderr, r.stderr


def test_warnings_module_repeats_are_collapsed(tmp_path: Path) -> None:
    (tmp_path / "p.py").write_text(MIXED)
    r = _barca(tmp_path, "get", "noisy", "p.py")
    assert r.returncode == 0, r.stderr
    assert r.stderr.count("UserWarning: deprecated thing") == 2, r.stderr  # shown once + summary
    assert "[barca] 2 more: UserWarning: deprecated thing" in r.stderr.splitlines(), r.stderr


def test_basicconfig_project_gets_every_record_and_no_summary(tmp_path: Path) -> None:
    """Logging is configured, so nothing is collapsed: not the step's own `retrying`, and not
    the library's warning either. Every record is printed in the project's format."""
    (tmp_path / "p.py").write_text(BASICCONFIG)
    r = _barca(tmp_path, "get", "noisy", "p.py")
    assert r.returncode == 0, r.stderr
    expected = [f"WARNING aiohttp.client: {NETRC}", "WARNING pipeline: retrying"] * 6
    assert _not_barca(r.stderr) == expected, r.stderr
    assert " more: " not in r.stderr, r.stderr


def test_own_stream_handler_gets_every_record_and_no_summary(tmp_path: Path) -> None:
    (tmp_path / "p.py").write_text(OWN_HANDLERS)
    r = _barca(tmp_path, "get", "noisy", "p.py")
    assert r.returncode == 0, r.stderr
    assert _not_barca(r.stderr) == [NETRC] * 6, r.stderr
    assert (tmp_path / "step.log").read_text().splitlines() == [NETRC] * 6
    assert " more: " not in r.stderr, r.stderr


def test_step_prints_raw_stderr_and_error_records_are_unchanged(tmp_path: Path) -> None:
    """Everything that is not a WARNING log record or a `warnings` warning is byte-for-byte
    what the step wrote, in the order it wrote it, next to the collapsed warnings."""
    (tmp_path / "p.py").write_text(MIXED)
    r = _barca(tmp_path, "get", "noisy", "p.py")
    assert r.returncode == 0, r.stderr
    expected = (
        # log.warning x5 (collapsed to one), log.error x5, print(file=stderr) x5, raw write x5
        [NETRC, "an error record", "same stderr line", NETRC]
        + ["an error record", "same stderr line", NETRC] * 4
        # warnings.warn x3 (collapsed to one): location line + source line
        + [
            f"{tmp_path.resolve() / 'p.py'}:20: UserWarning: deprecated thing",
            '  warnings.warn("deprecated thing")',
        ]
        # the step's stdout, flushed when the worker exits
        + ["same stdout line"] * 5
    )
    assert _not_barca(r.stderr) == expected, r.stderr
    barca_lines = [ln for ln in r.stderr.splitlines() if ln.startswith("[barca]")]
    assert barca_lines[:2] == [
        f"[barca] 4 more: {NETRC}",
        "[barca] 2 more: UserWarning: deprecated thing",
    ], r.stderr


def test_failure_message_and_envelope_are_unchanged(tmp_path: Path) -> None:
    """A step that fails with the very text that was collapsed still reports it in full."""
    (tmp_path / "p.py").write_text(MIXED)
    r = _barca(tmp_path, "get", "broken", "p.py", "--json")
    assert r.returncode == 1, r.stderr
    lines = r.stderr.splitlines()
    assert "[barca] run failed: step 'p.py:broken' failed (exit 1)" in lines
    envelope = json.loads(lines[-1])
    assert envelope["error"] == f"step 'p.py:broken' failed: ValueError: {NETRC}"
    assert envelope["kind"] == "step_failed" and envelope["node"] == "p.py:broken"
    assert envelope["traceback"] == (
        f'  File "{tmp_path.resolve() / "p.py"}", line 28, in broken\n'
        f'    raise ValueError("{NETRC}")'
    )
    out = json.loads(r.stdout)
    assert out["error"].splitlines()[0] == f"ValueError: {NETRC}"
    assert out["failed_node"] == "p.py:broken"
    # 5 in `noisy` + 4 in `broken`: one printed, 8 counted, including the failed step's.
    assert f"[barca] 8 more: {NETRC}" in lines, r.stderr


def test_many_distinct_warnings_are_bounded(tmp_path: Path) -> None:
    """600 distinct texts, twice each: a worker collapses the first 512 and prints the rest as
    they come; the summary names the ten most repeated and totals the others in one line."""
    (tmp_path / "p.py").write_text(_single(1200, '"distinct %d", i // 2'))
    r = _barca(tmp_path, "get", "noisy", "p.py")
    assert r.returncode == 0, r.stderr
    lines = r.stderr.splitlines()
    assert len([ln for ln in lines if ln.startswith("distinct ")]) == 512 + 2 * 88, r.stderr
    summary = [ln for ln in lines if " more: " in ln]
    assert len(summary) == 11, r.stderr
    assert summary[-1] == "[barca] 502 more: 502 other repeated warnings", r.stderr
