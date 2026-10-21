"""A step that raises keeps its worker; one that leaves the interpreter unsound does not (#523).

A failed partition key used to stop its worker and start a fresh one, about 45 ms a key, which
dominated runs where many keys fail. These tests count worker starts rather than time: the
coordinator's `BARCA_TRACE_TIMING` trace ends with "workers started N, failed steps that kept
their worker M".
"""

import json
import os
import re
import subprocess
import textwrap

import pytest

from barca.api import _find_binary

TRACE = re.compile(
    r"workers started (\d+), failed steps that kept their worker (\d+), steps dispatched (\d+)"
)


def run(tmp_path, source, target, pool=2):
    (tmp_path / "pipeline.py").write_text(textwrap.dedent(source).lstrip("\n"))
    proc = subprocess.run(
        [_find_binary(), "get", target, "pipeline.py", "--json"],
        cwd=tmp_path,
        env={
            **os.environ,
            "BARCA_REMOTE": "off",
            "BARCA_POOL_SIZE": str(pool),
            "BARCA_TRACE_TIMING": "1",
        },
        text=True,
        capture_output=True,
        timeout=120,
    )
    found = TRACE.findall(proc.stderr)
    assert found, proc.stderr
    started, kept, dispatched = (int(n) for n in found[-1])
    return proc, started, kept, dispatched


def envelope(proc):
    """The error envelope barca prints on stderr for the failed run."""
    lines = [line for line in proc.stderr.splitlines() if line.startswith('{"')]
    assert lines, proc.stderr
    return json.loads(lines[-1])


def test_failed_keys_cost_no_worker_start(tmp_path):
    proc, started, kept, dispatched = run(
        tmp_path,
        """
        from barca import asset, partitions

        KEYS = [f"k{i:03d}" for i in range(60)]

        @asset(partitions={"key": partitions(KEYS)})
        def fetch(key: str) -> dict:
            if int(key[1:]) % 2:
                raise ValueError(f"{key} fails on purpose")
            return {"key": key}

        @asset(inputs={"data": fetch}, partitions={"key": partitions(KEYS)})
        def score(data: dict, key: str) -> dict:
            return {"key": key}
        """,
        "score",
        pool=2,
    )
    assert proc.returncode == 1, proc.stderr
    # Worker starts per failed key: 0. Only the pool itself was started.
    assert started <= 2, proc.stderr
    assert kept == 30
    # Each step was sent to a worker once: 60 fetch keys and the 30 score keys they allow.
    # A failure used to stop its worker and re-send the rest of its batch.
    assert dispatched == 90, proc.stderr
    # The failure still carries its exception and its traceback into the user's file.
    error = envelope(proc)
    assert error["kind"] == "step_failed"
    assert "fails on purpose" in error["error"]
    assert "pipeline.py" in error["traceback"] and "raise ValueError" in error["traceback"]
    result = json.loads(proc.stdout)
    assert sorted(result["final_output"]) == [f"key=k{i:03d}" for i in range(0, 60, 2)]


FATAL = {
    "sys.exit": ("import sys", "sys.exit(3)", ""),
    "timeout": ("import time", "time.sleep(30)", ", timeout_seconds=1"),
    "MemoryError": ("", "raise MemoryError()", ""),
}


@pytest.mark.parametrize("case", sorted(FATAL))
def test_a_step_that_leaves_the_interpreter_unsound_replaces_its_worker(tmp_path, case):
    imports, bad, extra = FATAL[case]
    proc, started, kept, _ = run(
        tmp_path,
        f"""
        {imports}
        from barca import asset, partitions

        @asset(partitions={{"key": partitions(["a", "b", "c", "d"])}}{extra})
        def part(key: str) -> str:
            if key == "b":
                {bad}
            return key
        """,
        "part",
        pool=1,
    )
    assert proc.returncode == 1, proc.stderr
    assert kept == 0
    assert started == 2, proc.stderr
    assert envelope(proc)["node"] == "pipeline.py:part[key=b]"
    assert sorted(json.loads(proc.stdout)["final_output"]) == ["key=a", "key=c", "key=d"]


def test_a_failed_import_replaces_its_worker(tmp_path):
    proc, _, kept, _ = run(
        tmp_path,
        """
        from barca import asset

        raise RuntimeError("module body fails")

        @asset()
        def one() -> int:
            return 1
        """,
        "one",
        pool=1,
    )
    assert proc.returncode == 1, proc.stderr
    assert kept == 0
    assert "module body fails" in envelope(proc)["error"]


def test_a_step_that_raises_after_parallel_keeps_its_worker(tmp_path):
    proc, _, kept, _ = run(
        tmp_path,
        """
        from functools import partial
        from barca import asset, task, parallel

        @task()
        def branch(n: int) -> int:
            return n * 2

        @asset()
        def fan() -> int:
            values = parallel(*(partial(branch, n) for n in range(4)))
            raise ValueError(f"after parallel: {sum(values)}")
        """,
        "fan",
        pool=2,
    )
    assert proc.returncode == 1, proc.stderr
    assert kept == 1
    error = envelope(proc)
    assert error["node"] == "pipeline.py:fan"
    assert "after parallel: 12" in error["error"]
