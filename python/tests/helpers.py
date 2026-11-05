"""Helpers every test module may share: the barca CLI as a subprocess, a scrubbed
environment, waiting on a condition, and the subprocess timeouts.

`conftest.py` turns the pieces that want a `tmp_path` into fixtures (`project`, `serve`,
`clean_barca_dir`). A `barca serve` child is `served.Serve` (through the `serve` fixture);
nothing else may spawn one.
"""

import os
import socket
import subprocess
import textwrap
import time
from collections.abc import Callable
from pathlib import Path
from typing import Any

import pytest

from barca.api import _find_binary

# Subprocess timeouts, in seconds. They bound a hang, not the expected duration: a test that
# hits one has failed, and `[tool.pytest.ini_options] timeout` (120 s) ends a test that waits
# longer than RUN for anything at all.
QUICK = 30  # an inspection or usage-error call, or one run of a tiny pipeline
RUN = 120  # a pipeline run on the local machine
SERVE = 300  # a run against an object store (or its emulator), a server's whole lifetime

# Variables the machine's own setup may hold (a store, credentials, an emulator, a BARCA_*
# override) that would change what a test sees; `clean_env` drops them.
SCRUB = ("BARCA_", "FSSPEC_", "AWS_", "AZURE_", "GOOGLE_", "GCSFS_", "STORAGE_EMULATOR_HOST")

TESTS = Path(__file__).resolve().parent
REPO = TESTS.parents[1]


def clean_env(*more: str, scrub: tuple[str, ...] = SCRUB, **overrides: str) -> dict[str, str]:
    """`os.environ` without the variables that start with `scrub` or `more`, plus `overrides`.

    `clean_env()` is the default environment of a `barca` call in a test: the machine's own
    barca configuration cannot leak into it. `clean_env(BARCA_POOL_SIZE="2")` sets one
    variable; `clean_env("DD_", "SLACK_")` also drops telemetry settings.
    """
    prefixes = scrub + more
    env = {k: v for k, v in os.environ.items() if not k.startswith(prefixes)}
    env.update(overrides)
    return env


def free_port() -> int:
    """A TCP port nothing listens on right now (the kernel's pick, released at once)."""
    with socket.socket() as s:
        s.bind(("127.0.0.1", 0))
        return s.getsockname()[1]


def write_module(directory: Path, filename: str, code: str, *, strip: bool = False) -> str:
    """Write dedented `code` to `directory/filename`; the path, as a string.

    `strip=True` also drops the leading newline of a triple-quoted block that starts with
    one, for a test that reads the file back by line.
    """
    body = textwrap.dedent(code)
    if strip:
        body = body.lstrip("\n")
    path = directory / filename
    path.parent.mkdir(parents=True, exist_ok=True)
    path.write_text(body)
    return str(path)


def wait_until(
    predicate: Callable[[], Any],
    what: str = "the condition",
    *,
    timeout: float = 60,
    interval: float = 0.05,
    proc: subprocess.Popen | None = None,
) -> Any:
    """Poll `predicate` every `interval` seconds until it returns something truthy, and
    return that value; fail the test after `timeout` seconds, naming `what`.

    With `proc`, a child that exits while the test waits fails the test at once, with the
    child's exit code, instead of after the timeout.
    """
    deadline = time.monotonic() + timeout
    while True:
        value = predicate()
        if value:
            return value
        if proc is not None and proc.poll() is not None:
            pytest.fail(f"the process exited with {proc.returncode} before {what}")
        if time.monotonic() >= deadline:
            pytest.fail(f"timed out after {timeout:.0f}s waiting for {what}")
        time.sleep(interval)


def barca(
    cwd: Path,
    *args: str,
    env: dict[str, str] | None = None,
    timeout: float = RUN,
    **popen: Any,
) -> subprocess.CompletedProcess:
    """Run `barca *args` in `cwd` with its output captured as text; the completed process,
    whatever its exit code.

    `env=None` is `clean_env()`. Pass `env={**os.environ, ...}` to inherit the machine's
    configuration on purpose. Extra keyword arguments go to `subprocess.run`.
    """
    popen.setdefault("stdin", subprocess.DEVNULL)
    return subprocess.run(
        [_find_binary(), *args],
        cwd=cwd,
        env=clean_env() if env is None else env,
        capture_output=True,
        text=True,
        check=False,
        timeout=timeout,
        **popen,
    )
