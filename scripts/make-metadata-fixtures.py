#!/usr/bin/env python3
"""Rebuild the metadata DB fixtures that `python/tests/test_metadata_compat.py` opens (#629).

For each released barca version, install its wheel from PyPI into a private venv, run the
fixture pipeline with it (a sensor, an asset read from it, a task that prints to stdout and
stderr, and an asset that fails), and save the `.barca/` directory it left as
`python/tests/fixtures/metadata/<version>.tar.gz`. The test opens each with the current build.

Run it from the repository root after a release that changes the metadata schema, adding the
new version to VERSIONS (one release per minor from 0.21.0, the first in metadata format 1, plus
every release that added to format 1, and one legacy-format release):

    scripts/make-metadata-fixtures.py              # every version in VERSIONS
    scripts/make-metadata-fixtures.py 0.30.0       # only these

Work directories go under `target-local/metadata-fixtures/` (deleted per version when done).
Needs `uv` and network access to PyPI. Not run by CI: the fixtures are committed, so CI tests
them without downloading anything.
"""

import io
import os
import shutil
import subprocess
import sys
import tarfile
from pathlib import Path

ROOT = Path(__file__).resolve().parents[1]
OUT = ROOT / "python/tests/fixtures/metadata"
WORK = ROOT / "target-local/metadata-fixtures"

# The pipeline every version runs, which the test runs too. It must parse with all of them: no
# decorator arguments newer than 0.20.
PIPELINE = (OUT / "pipeline.py").read_text()

# 0.20.1: the last release in legacy format 0. 0.21.0: the first in format 1. Then the last
# release of each minor, and every 0.29 patch that added to format 1 (0.29.2 `logs.stream` and
# `logs.ts`, 0.29.3 partial indexes, 0.29.4 the run queue tables).
VERSIONS = [
    "0.20.1",
    "0.21.0",
    "0.22.0",
    "0.23.0",
    "0.24.3",
    "0.25.1",
    "0.26.1",
    "0.27.0",
    "0.28.3",
    "0.29.1",
    "0.29.2",
    "0.29.3",
    "0.29.4",
    "0.29.6",
    "0.30.0",
    "0.31.0",
]

# What each version runs, in order. Exit codes are checked: 0 for these, 1 for the failure.
STEPS = [
    (["get", "refreshed", "pipeline.py"], 0),
    (["run", "job", "pipeline.py"], 0),
    (["get", "broken", "pipeline.py"], 1),
]


def run(cmd: list[str], cwd: Path, env: dict[str, str], expect: int | None = 0) -> None:
    result = subprocess.run(
        cmd, check=False, cwd=cwd, env=env, capture_output=True, text=True, timeout=300
    )
    if expect is not None and result.returncode != expect:
        sys.exit(
            f"{' '.join(cmd)} exited {result.returncode}, expected {expect}\n"
            f"{result.stdout}\n{result.stderr}"
        )


def build(version: str) -> Path:
    work = WORK / version
    if work.exists():
        shutil.rmtree(work)
    project = work / "project"
    project.mkdir(parents=True)
    venv = work / "venv"
    env = {k: v for k, v in os.environ.items() if k not in ("VIRTUAL_ENV", "PYTHONPATH")}
    run(["uv", "venv", "-q", "--python", "3.12", str(venv)], work, env)
    run(
        ["uv", "pip", "install", "-q", "--python", str(venv / "bin/python"), f"barca=={version}"],
        work,
        env,
    )
    (project / "pipeline.py").write_text(PIPELINE)
    env = {
        **env,
        "VIRTUAL_ENV": str(venv),
        "PATH": f"{venv / 'bin'}:{env['PATH']}",
        "BARCA_OUTPUT": "json",
    }
    for args, expect in STEPS:
        run([str(venv / "bin/barca"), *args], project, env, expect)
    # The history the test checks is there: every run, its lines and the failure.
    out = OUT / f"{version}.tar.gz"
    OUT.mkdir(parents=True, exist_ok=True)
    buffer = io.BytesIO()
    with tarfile.open(fileobj=buffer, mode="w:gz", compresslevel=9) as tar:

        def keep(info: tarfile.TarInfo) -> tarfile.TarInfo | None:
            # Locks and run-owner markers belong to the process that wrote them.
            if info.name.endswith(".lock") or "/run-owners/" in info.name + "/":
                return None
            info.uid = info.gid = 0
            info.uname = info.gname = ""
            info.mtime = 0
            return info

        tar.add(project / ".barca", arcname=".barca", filter=keep)
    out.write_bytes(buffer.getvalue())
    shutil.rmtree(work)
    return out


def main() -> None:
    versions = sys.argv[1:] or VERSIONS
    for version in versions:
        out = build(version)
        print(f"{version}: {out.relative_to(ROOT)} ({out.stat().st_size} bytes)")


if __name__ == "__main__":
    main()
