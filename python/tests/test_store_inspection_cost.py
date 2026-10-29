"""Repeated status inspection must bound reads of unchanged store artifact payloads."""

import json
import os
import subprocess

from barca.api import _find_binary

from .test_serve_robustness import _env


def test_warm_status_does_not_rehash_an_unchanged_store_copy(tmp_path):
    project = tmp_path / "project"
    project.mkdir()
    store = tmp_path / "store"
    audit = tmp_path / "audit"
    audit.mkdir()
    reads = tmp_path / "store-reads.jsonl"
    (project / "barca.toml").write_text(f"[remote]\nuri = {json.dumps(str(store))}\n")
    (project / "pipeline.py").write_text(
        "from barca import asset\n@asset()\ndef value(): return 42\n"
    )
    # Observe real helper payload opens, rather than asserting a wall-clock threshold.
    (audit / "sitecustomize.py").write_text(
        "import sys, os, json\n"
        f"prefix = {str(store) + os.sep!r}\n"
        f"log = {str(reads)!r}\n"
        "def observe(event, args):\n"
        "    if event != 'open' or not isinstance(args[0], str): return\n"
        "    path = args[0]\n"
        "    if path.startswith(prefix) and '/artifacts/' in path and path.endswith('.json'):\n"
        "        with open(log, 'a') as out: out.write(json.dumps(path) + '\\n')\n"
        "sys.addaudithook(observe)\n"
    )
    env = _env() | {"PYTHONPATH": str(audit) + os.pathsep + os.environ.get("PYTHONPATH", "")}

    def command(*args):
        result = subprocess.run(
            [_find_binary(), *args, "--json"],
            cwd=project,
            env=env,
            text=True,
            capture_output=True,
            timeout=20,
        )
        assert result.returncode == 0, result.stderr
        return json.loads(result.stdout)

    command("get", "value", "pipeline.py")
    assert reads.read_text().splitlines(), "the real transfer must exercise the audit hook"
    reads.write_text("")
    first = command("status", "pipeline.py")
    first_reads = reads.read_text().splitlines()
    assert first["nodes"][0]["cache"]["state"] == "cached", first
    second = command("status", "pipeline.py")
    assert second["nodes"][0]["cache"]["state"] == "cached", second
    repeated_reads = reads.read_text().splitlines()[len(first_reads) :]
    assert not repeated_reads, f"unchanged warm status reopened store payloads: {repeated_reads}"
