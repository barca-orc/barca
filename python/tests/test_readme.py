"""The README must stay accurate: its pipelines run, and its CLI table matches `--help`.

Same idea as test_docs_examples.py for the manual: the first pipeline executes and prints what
the text claims, the "first pipeline" commands run against the second one, and every command
and flag the CLI table (and any `barca ...` line in a bash block) names exists in the real CLI.
"""

import json
import re
import shlex
import subprocess
from pathlib import Path

import pytest

from barca.api import _find_binary

README = (Path(__file__).resolve().parents[2] / "README.md").read_text()
FENCE = re.compile(r"^```(\w*)\n(.*?)^```", re.S | re.M)


def blocks(lang: str) -> list[str]:
    return [m.group(2) for m in FENCE.finditer(README) if m.group(1) == lang]


@pytest.fixture(scope="module")
def binary() -> str:
    return _find_binary()


def run(binary: str, cwd: Path, *args: str) -> subprocess.CompletedProcess:
    return subprocess.run([binary, *args], cwd=cwd, capture_output=True, text=True)


def last_json(proc: subprocess.CompletedProcess) -> dict:
    assert proc.returncode == 0, f"exit {proc.returncode}\n{proc.stderr}"
    out = proc.stdout.strip()
    try:
        return json.loads(out)  # list prints indented JSON
    except json.JSONDecodeError:
        return json.loads(out.splitlines()[-1])  # get/run: user prints may precede the JSON line


def helps(binary: str) -> dict[str, str]:
    top = subprocess.run([binary, "--help"], capture_output=True, text=True).stdout
    out = {}
    for sub in re.findall(r"^  ([a-z]+)\s{2,}\S", top.split("Options:")[0], re.M):
        out[sub] = subprocess.run([binary, sub, "--help"], capture_output=True, text=True).stdout
    return out


def test_first_example_runs_and_is_cached(binary, tmp_path):
    pipeline = next(b for b in blocks("python") if "def summary" in b)
    (tmp_path / "pipeline.py").write_text(pipeline)
    first = last_json(run(binary, tmp_path, "get", "summary", "pipeline.py", "--json"))
    assert first["final_output"] == {"count": 3, "total": 6}
    assert first["steps_executed"] == 2
    again = last_json(run(binary, tmp_path, "get", "summary", "pipeline.py", "--json"))
    assert again["steps_executed"] == 0  # "Run it again and both steps come from cache"


def test_first_pipeline_commands_run(binary, tmp_path):
    pipeline = next(b for b in blocks("python") if "def publish" in b)
    (tmp_path / "pipeline.py").write_text(pipeline)
    listed = last_json(run(binary, tmp_path, "list", "--json"))
    assert {n["id"].split(":")[-1] for n in listed["nodes"]} == {"numbers", "total", "publish"}

    cmds = [
        shlex.split(line.split("#")[0])[1:]
        for b in blocks("bash")
        for line in b.splitlines()
        if line.startswith("barca ") and "numbers" in b and "publish" in b
    ]
    assert cmds, "the first-pipeline block was not found"
    for args in cmds:
        if args[0] == "sql":
            continue  # needs duckdb in the environment; covered by test_sql.py
        proc = run(binary, tmp_path, *args, "--json")
        assert proc.returncode == 0, f"barca {' '.join(args)}\n{proc.stderr}"


def test_cli_table_matches_help(binary):
    table = [ln for ln in README.splitlines() if ln.startswith("| `barca ")]
    assert table, "CLI table not found"
    commands = helps(binary)
    seen = set()
    for row in table:
        sub = re.match(r"\| `barca ([a-z]+)", row).group(1)
        seen.add(sub)
        assert sub in commands, f"README names `barca {sub}`, which does not exist"
        cells = row.split("|")
        for flag in set(re.findall(r"`(--[a-z-]+)", "|".join(cells[2:]))):
            assert flag in commands[sub], f"README says `barca {sub}` takes {flag}"
        # mentioned in the description's flag list, e.g. "--refresh a,b [--no-cascade]"
        for flag in set(re.findall(r"(?<![\w-])(--[a-z][a-z-]*)", row)):
            assert flag in commands[sub], f"README says `barca {sub}` takes {flag}"
    # every real command is in the table
    assert set(commands) - {"help"} <= seen, f"missing from the CLI table: {set(commands) - seen}"


def test_bash_blocks_name_real_commands_and_flags(binary):
    commands = helps(binary)
    for b in blocks("bash"):
        for line in b.splitlines():
            if not line.startswith("barca "):
                continue
            words = shlex.split(line.split("#")[0])
            assert words[1] in commands or words[1].endswith(".py"), line
            if words[1] in commands:
                for flag in re.findall(r"--[a-z][a-z-]*", " ".join(words)):
                    assert flag in commands[words[1]], f"{line}: no {flag}"
