"""An argument a barca decorator does not define is an error, not something to ignore (#284).

Until 0.18.1 `@asset(after=other)`, `@task(when=...)` or a misspelt `input=` planned and ran
with exit 0 and did nothing. The rule is in `barca docs assets`, "Accepted arguments". The
accepted names live in `crates/barca-core/src/decorator_args.rs` (where a Rust test holds the
Python stubs and the documentation tables to them); the parser cases are in
`crates/barca-core/tests/grammar_spec.rs`. This file tests what a user sees: the exit code and
the error on every command that reads the file, which commands a bad file stops, and what the
Python stubs do when the module is imported.
"""

from __future__ import annotations

import inspect
import json
import subprocess
import sys
from pathlib import Path

import pytest

import barca
from barca.api import _find_binary

ASSET_ACCEPTS = (
    "@asset accepts: name, inputs, partitions, serializer, freshness, timeout_seconds, "
    "retries, retry_backoff, description, tags, env"
)

GOOD = """
from barca import asset, task


@asset()
def fine() -> int:
    return 1


@task(inputs={"fine": fine})
def publish(fine: int) -> int:
    return fine
"""


def pipeline(decorator: str) -> str:
    return (
        "from barca import asset, task, sensor, sink, partitions, collect, Schedule\n\n\n"
        "@asset()\ndef raw() -> dict:\n    return {'x': 1}\n\n\n"
        f"{decorator}\ndef node(*args, **kwargs):\n    return 1\n"
    )


def barca_cmd(cwd: Path, *args: str) -> subprocess.CompletedProcess:
    return subprocess.run(
        [_find_binary(), *args], cwd=cwd, capture_output=True, text=True, timeout=120
    )


def envelope(proc: subprocess.CompletedProcess) -> dict:
    return json.loads(proc.stderr.strip().splitlines()[-1])


@pytest.mark.parametrize(
    ("decorator", "error", "remediation"),
    [
        (
            "@asset(after=raw)",
            f"`after` is not an argument of @asset. {ASSET_ACCEPTS}",
            "Remove `after`, or replace it with an argument @asset accepts. See `barca docs assets`.",
        ),
        (
            "@task(when='always')",
            "`when` is not an argument of @task. @task accepts: name, inputs, freshness, "
            "timeout_seconds, retries, retry_backoff, description, tags, env",
            "Remove `when`, or replace it with an argument @task accepts. See `barca docs tasks`.",
        ),
        (
            "@asset(input={'raw': raw})",
            f"`input` is not an argument of @asset. Did you mean `inputs`? {ASSET_ACCEPTS}",
            "Rename `input` to `inputs`, or remove it. See `barca docs assets`.",
        ),
        (
            "@asset(partition={'k': partitions(['a'])})",
            f"`partition` is not an argument of @asset. Did you mean `partitions`? {ASSET_ACCEPTS}",
            "Rename `partition` to `partitions`, or remove it. See `barca docs assets`.",
        ),
        (
            "@asset(serialiser='pickle')",
            f"`serialiser` is not an argument of @asset. Did you mean `serializer`? {ASSET_ACCEPTS}",
            "Rename `serialiser` to `serializer`, or remove it. See `barca docs assets`.",
        ),
        (
            "@asset(**{'inputs': {'raw': raw}})",
            "@asset is called with `**` arguments. barca reads decorator arguments from the "
            f"source without running it, so it cannot see what they are. {ASSET_ACCEPTS}",
            'Write the arguments out, like `@asset(inputs={"param": upstream}, ...)`. '
            "See `barca docs assets`.",
        ),
        (
            "@asset(partitions={'k': partitions(values=['a', 'b'])})",
            "`values` is not an argument of partitions(). partitions() takes no keyword arguments",
            'Pass the value by position, like `partitions(["a", "b"])`, or remove `values`. '
            "See `barca docs partitions`.",
        ),
        (
            "@asset()\n@sink(path='out.json')",
            "`path` is not an argument of @sink. @sink accepts: serializer",
            "Remove `path`, or replace it with an argument @sink accepts. See `barca docs sinks`.",
        ),
        (
            "@asset(freshness=Schedule(cron='0 5 * * *'))",
            "`cron` is not an argument of Schedule(). Schedule() takes no keyword arguments",
            'Pass the value by position, like `Schedule("0 5 * * *")`, or remove `cron`. '
            "See `barca docs scheduling`.",
        ),
    ],
)
def test_the_error_names_the_node_the_argument_and_what_is_accepted(
    tmp_path: Path, decorator: str, error: str, remediation: str
) -> None:
    (tmp_path / "pipeline.py").write_text(pipeline(decorator))
    line = pipeline(decorator).splitlines().index(decorator.splitlines()[-1]) + 1

    proc = barca_cmd(tmp_path, "list", "pipeline.py", "--json")
    assert proc.returncode == 2
    assert proc.stdout == ""
    assert envelope(proc) == {
        "code": 2,
        "kind": "usage",
        "error": f"Parse error: pipeline.py:node (line {line}): {error}",
        "remediation": remediation,
    }

    # Human mode: the same two sentences as prose, the fix on the last line, once.
    proc = barca_cmd(tmp_path, "list", "pipeline.py", "--pretty")
    assert proc.returncode == 2
    assert proc.stderr == (f"Parse error: pipeline.py:node (line {line}): {error}\n{remediation}\n")


@pytest.mark.parametrize(
    "args",
    [
        ("list", "pipeline.py"),
        ("plan", "pipeline.py"),
        ("status", "pipeline.py"),
        ("get", "pipeline.py"),
        ("get", "raw", "pipeline.py"),
        ("get", "raw", "pipeline.py", "--dry-run"),
        ("run", "node", "pipeline.py"),
        ("stats", "raw", "pipeline.py"),
        ("sql", "select 1", "pipeline.py"),
    ],
)
def test_every_command_that_reads_the_file_exits_2_and_runs_nothing(
    tmp_path: Path, args: tuple[str, ...]
) -> None:
    (tmp_path / "pipeline.py").write_text(pipeline("@task(when='always')"))
    proc = barca_cmd(tmp_path, *args)
    assert proc.returncode == 2, proc.stderr
    assert "`when` is not an argument of @task" in proc.stderr
    assert proc.stdout == ""
    # Nothing ran: `raw` (asked for by name, and fine in itself) has no artifact.
    assert not (tmp_path / ".barca" / "artifacts").exists()


def test_one_bad_file_stops_every_command_that_reads_it_and_no_other(tmp_path: Path) -> None:
    """The scope is the file, as for a syntax error: a command that reads the file fails,
    whatever its target; a command given other files does not read it."""
    (tmp_path / "barca.toml").write_text("")
    (tmp_path / "good.py").write_text(GOOD)
    (tmp_path / "sub").mkdir()
    (tmp_path / "sub" / "bad.py").write_text(pipeline("@asset(after=raw)"))

    # No file arguments: the whole project is read, so the target's own file being fine
    # does not help.
    for args in (("list",), ("get", "fine"), ("run", "publish"), ("status", "fine")):
        proc = barca_cmd(tmp_path, *args, "--json")
        assert proc.returncode == 2, args
        assert envelope(proc)["error"].startswith(
            "Parse error: sub/bad.py:node (line 9): `after` is not an argument of @asset."
        ), args

    # Naming the files to read leaves the bad one out.
    proc = barca_cmd(tmp_path, "get", "fine", "good.py", "--json")
    assert proc.returncode == 0, proc.stderr
    assert json.loads(proc.stdout)["final_output"] == 1

    # `history` reads no source file.
    assert barca_cmd(tmp_path, "history", "--json").returncode == 0


def test_a_decorator_that_is_not_barcas_is_not_checked(tmp_path: Path) -> None:
    (tmp_path / "pipeline.py").write_text(
        "import barca\n\n\n"
        "def asset(**options):\n    return lambda f: f\n\n\n"
        "@asset(owner='me')\ndef mine() -> int:\n    return 1\n"
    )
    proc = barca_cmd(tmp_path, "list", "pipeline.py", "--json")
    assert proc.returncode == 0, proc.stderr
    assert [n["id"] for n in json.loads(proc.stdout)["nodes"]] == ["pipeline.py:mine"]


# ─── The Python stubs ─────────────────────────────────────────────────────────

NODE_DECORATORS = (barca.asset, barca.sensor, barca.task)


@pytest.mark.parametrize(
    "stub",
    [
        *NODE_DECORATORS,
        barca.sink,
        barca.partitions,
        barca.partitions_from,
        barca.collect,
        barca.asset_ref,
        barca.Schedule,
    ],
)
def test_no_stub_takes_arbitrary_arguments(stub) -> None:
    kinds = {p.kind for p in inspect.signature(stub).parameters.values()}
    assert inspect.Parameter.VAR_KEYWORD not in kinds
    assert inspect.Parameter.VAR_POSITIONAL not in kinds


def test_an_unknown_argument_is_a_type_error_when_the_module_is_imported(tmp_path: Path) -> None:
    """`python pipeline.py` fails on the same argument the binary rejects."""
    for decorator, message in [
        ("@asset(after=raw)", "asset() got an unexpected keyword argument 'after'"),
        ("@task(when=1)", "task() got an unexpected keyword argument 'when'"),
        ("@sensor(inputs={})", "sensor() got an unexpected keyword argument 'inputs'"),
        ("@asset()\n@sink('o.json', mode='a')", "sink() got an unexpected keyword argument 'mode'"),
        (
            "@asset(freshness=Schedule(cron='* * * * *'))",
            "positional-only arguments passed as keyword",
        ),
        (
            "@asset(inputs={'r': collect(asset_fn=raw)})",
            "positional-only arguments passed as keyword",
        ),
        (
            "@asset(partitions={'k': partitions(values=[1])})",
            "positional-only arguments passed as keyword",
        ),
    ]:
        path = tmp_path / "pipeline.py"
        path.write_text(pipeline(decorator))
        proc = subprocess.run([sys.executable, str(path)], capture_output=True, text=True)
        assert proc.returncode == 1, decorator
        assert "TypeError" in proc.stderr and message in proc.stderr, (decorator, proc.stderr)


def test_every_documented_argument_still_works_standalone() -> None:
    """Each decorator still returns the function unchanged, with every argument it defines."""
    from barca import (
        Always,
        Manual,
        Schedule,
        asset,
        asset_ref,
        collect,
        partitions,
        sensor,
        sink,
        task,
    )

    def fn():
        return 1

    assert asset(fn) is fn and sensor(fn) is fn and task(fn) is fn
    common = dict(
        name="n",
        timeout_seconds=5,
        retries=2,
        retry_backoff=0.5,
        description="d",
        tags={"a": "b"},
        env=["HOME"],
    )
    assert (
        asset(
            inputs={"x": collect(fn), "y": asset_ref("p.py:fn")},
            partitions={"k": partitions(["a"]), "j": barca.partitions_from(fn)},
            serializer="json",
            freshness=Always,
            **common,
        )(fn)
        is fn
    )
    assert sensor(freshness=Schedule("*/5 * * * *"), **common)(fn) is fn
    assert task(inputs={"x": fn}, freshness=Manual, **common)(fn) is fn
    assert sink("out.json", serializer="json")(fn) is fn
    assert Schedule("0 5 * * *").cron == "0 5 * * *"
