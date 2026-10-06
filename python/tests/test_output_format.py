"""Output format follows the terminal: human output on a TTY, JSON everywhere else.

The rule (`barca docs agents`): `--json` / `--pretty` (or `-o` on get/run) win; otherwise
`BARCA_OUTPUT=json|pretty`; otherwise a TTY on stdout gets tables/pretty text and anything else
(a pipe, a subprocess, an agent) gets JSON. The TTY cases run barca with stdout attached to a
real pseudo-terminal from the stdlib `pty` module.
"""

import errno
import json
import os
import pty
import subprocess
from pathlib import Path

import pytest

from barca.api import _find_binary

PIPELINE = """
from barca import asset, task


@asset()
def total() -> int:
    return 42


@task(inputs={"t": total})
def report(t: int) -> dict:
    return {"t": t}
"""


@pytest.fixture()
def project(tmp_path) -> Path:
    (tmp_path / "pipeline.py").write_text(PIPELINE)
    return tmp_path


def _env(extra: dict | None) -> dict:
    env = {k: v for k, v in os.environ.items() if k != "BARCA_OUTPUT"}
    env.update(extra or {})
    return env


def piped(project: Path, *args: str, env: dict | None = None) -> subprocess.CompletedProcess:
    return subprocess.run(
        [_find_binary(), *args], cwd=project, capture_output=True, text=True, env=_env(env)
    )


def on_tty(project: Path, *args: str, env: dict | None = None) -> tuple[int, str, str]:
    """Run barca with stdout on a pseudo-terminal (stderr piped). Returns (code, stdout, stderr)."""
    master, slave = pty.openpty()
    proc = subprocess.Popen(
        [_find_binary(), *args],
        cwd=project,
        stdin=subprocess.DEVNULL,
        stdout=slave,
        stderr=subprocess.PIPE,
        env=_env(env),
    )
    os.close(slave)
    chunks = []
    while True:
        try:
            data = os.read(master, 65536)
        except OSError as e:  # Linux raises EIO once the child closes the pty
            if e.errno != errno.EIO:
                raise
            break
        if not data:
            break
        chunks.append(data)
    os.close(master)
    assert proc.stderr is not None
    stderr = proc.stderr.read().decode()
    code = proc.wait(timeout=60)
    out = b"".join(chunks).decode().replace("\r\n", "\n")
    return code, out, stderr


def is_json(text: str) -> bool:
    try:
        json.loads(text)
        return True
    except json.JSONDecodeError:
        return False


@pytest.fixture()
def warmed(project) -> Path:
    """A project with one recorded run, so history and stats have something to show."""
    assert piped(project, "get", "total", "pipeline.py").returncode == 0
    return project


INSPECTION = [
    ("list", "pipeline.py"),
    ("history",),
    ("stats", "total", "pipeline.py"),
]
RESULTS = [
    ("get", "total", "pipeline.py"),
    ("run", "report", "pipeline.py"),
]
ALL = INSPECTION + RESULTS


@pytest.mark.parametrize("args", ALL, ids=lambda a: a[0])
def test_piped_stdout_gets_json(warmed, args):
    proc = piped(warmed, *args)
    assert proc.returncode == 0, proc.stderr
    assert is_json(proc.stdout), proc.stdout


@pytest.mark.parametrize("args", ALL, ids=lambda a: a[0])
def test_terminal_stdout_gets_human_output(warmed, args):
    code, out, err = on_tty(warmed, *args)
    assert code == 0, err
    assert out.strip() and not is_json(out), out


@pytest.mark.parametrize("args", ALL, ids=lambda a: a[0])
def test_json_flag_forces_json_on_a_terminal(warmed, args):
    code, out, err = on_tty(warmed, *args, "--json")
    assert code == 0, err
    assert is_json(out), out


@pytest.mark.parametrize("args", ALL, ids=lambda a: a[0])
def test_pretty_flag_forces_human_output_when_piped(warmed, args):
    proc = piped(warmed, *args, "--pretty")
    assert proc.returncode == 0, proc.stderr
    assert proc.stdout.strip() and not is_json(proc.stdout), proc.stdout


@pytest.mark.parametrize("args", ALL, ids=lambda a: a[0])
def test_env_override(warmed, args):
    proc = piped(warmed, *args, env={"BARCA_OUTPUT": "pretty"})
    assert proc.returncode == 0, proc.stderr
    assert not is_json(proc.stdout), proc.stdout
    code, out, err = on_tty(warmed, *args, env={"BARCA_OUTPUT": "json"})
    assert code == 0, err
    assert is_json(out), out


def test_flag_beats_env(warmed):
    proc = piped(warmed, "list", "pipeline.py", "--json", env={"BARCA_OUTPUT": "pretty"})
    assert is_json(proc.stdout), proc.stdout
    code, out, _ = on_tty(warmed, "list", "pipeline.py", "--pretty", env={"BARCA_OUTPUT": "json"})
    assert code == 0 and not is_json(out), out


def test_invalid_env_value_is_a_usage_error(project):
    proc = piped(project, "list", "pipeline.py", env={"BARCA_OUTPUT": "yaml"})
    assert proc.returncode == 2
    assert "BARCA_OUTPUT" in proc.stderr and "json" in proc.stderr and "pretty" in proc.stderr
    assert proc.stdout == ""


def test_json_and_pretty_conflict(project):
    proc = piped(project, "list", "pipeline.py", "--json", "--pretty")
    assert proc.returncode == 2


def test_o_still_works_on_get_and_run(warmed):
    proc = piped(warmed, "get", "total", "pipeline.py", "-o", "pretty")
    assert proc.returncode == 0 and not is_json(proc.stdout)
    proc = piped(warmed, "get", "total", "pipeline.py", "-o", "value")
    assert proc.returncode == 0 and json.loads(proc.stdout) == 42
    code, out, _ = on_tty(warmed, "run", "report", "pipeline.py", "-o", "json")
    assert code == 0 and "run_id" in json.loads(out)


def test_o_conflicts_with_json_and_pretty(project):
    for flag in ("--json", "--pretty"):
        proc = piped(project, "get", "total", "pipeline.py", "-o", "value", flag)
        assert proc.returncode == 2, flag


def test_no_ansi_on_piped_streams(project):
    proc = piped(project, "get", "total", "pipeline.py", "--pretty")
    assert proc.returncode == 0, proc.stderr
    assert "\x1b" not in proc.stdout
    assert "\x1b" not in proc.stderr


def test_python_api_is_unaffected_by_env_override(warmed, monkeypatch):
    import barca

    monkeypatch.chdir(warmed)
    monkeypatch.setenv("BARCA_OUTPUT", "pretty")
    assert barca.get("total", "pipeline.py") == 42
    assert barca.run("report", "pipeline.py") == {"t": 42}
    runs = barca.history()
    assert runs and {"run_id", "status", "started_at"} <= set(runs[0])
    assert barca.stats("total", "pipeline.py")["total_runs"] >= 1
