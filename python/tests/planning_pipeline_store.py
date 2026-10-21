"""The planning pipeline (#250) against the S3 emulator: machines, source modes, measurements.

A `Machine` is one checkout of the generated project with its own `.barca/`. Its environment
selects one of the three source modes of #250:

- `local`: sources in the project's `data/` directory, barca's artifacts local;
- `store`: sources in the bucket, read by DuckDB's httpfs with a secret on the default
  connection, and barca's artifacts in the same bucket (`BARCA_REMOTE_URI`);
- `hybrid`: sources in the bucket, barca's artifacts local.

In `store` and `hybrid` modes the local `data/` directory is deleted, so a source read that
went to the local directory would fail.
"""

from __future__ import annotations

import contextlib
import importlib.util
import json
import os
import shutil
import socket
import subprocess
import sys
import uuid
from dataclasses import dataclass, field
from pathlib import Path
from urllib.parse import urlsplit

import pytest
from barca.api import _find_binary

from . import emulators
from .planning_pipeline import GENERATOR, write_planning_pipeline

S3_ENDPOINT = os.environ.get("BARCA_TEST_S3_ENDPOINT", "http://localhost:9100")
S3_KEY = os.environ.get("BARCA_TEST_S3_KEY", "minioadmin")
S3_SECRET = os.environ.get("BARCA_TEST_S3_SECRET", "minioadmin")
# Variables of the test environment that must not reach the machines.
SCRUB = ("BARCA_", "AWS_", "FSSPEC_", "PLANNING_", "REGISTRY_")


def _reachable(url: str) -> bool:
    parts = urlsplit(url)
    try:
        with socket.create_connection((parts.hostname, parts.port or 80), timeout=0.5):
            return True
    except OSError:
        return False


class Bucket:
    """A fresh bucket on the S3 emulator, removed at teardown."""

    def __init__(self):
        pytest.importorskip("s3fs")
        emulators.require("s3", _reachable(S3_ENDPOINT), S3_ENDPOINT)
        import fsspec

        self.name = f"planning-{uuid.uuid4().hex[:10]}"
        self.fs = fsspec.filesystem(
            "s3",
            key=S3_KEY,
            secret=S3_SECRET,
            client_kwargs={"endpoint_url": S3_ENDPOINT},
            skip_instance_cache=True,
        )
        self.fs.mkdir(self.name)

    def uri(self, path: str) -> str:
        return f"s3://{self.name}/{path}"

    def put_sources(self, project_root: Path, prefix: str) -> str:
        """Upload the project's `data/` directory under `prefix`; returns its URI."""
        self.fs.put(f"{project_root / 'data'}/", f"{self.name}/{prefix}/", recursive=True)
        return self.uri(prefix)

    def cat(self, uri: str) -> bytes:
        self.fs.invalidate_cache()
        return self.fs.cat_file(uri)

    def remove(self) -> None:
        # Best effort: the emulator is thrown away with the job.
        with contextlib.suppress(OSError):
            self.fs.rm(self.name, recursive=True)


@dataclass
class Machine:
    """One checkout of the project with its own `.barca/` and source mode."""

    root: Path
    mode: str
    bucket: Bucket | None = None
    sources_uri: str = ""
    store_uri: str = ""
    store_endpoint: str = S3_ENDPOINT
    pool: int = 4
    extra: dict = field(default_factory=dict)

    @property
    def env(self) -> dict:
        env = {k: v for k, v in os.environ.items() if not k.startswith(SCRUB)}
        env.update({"PYTHONDONTWRITEBYTECODE": "1", "BARCA_POOL_SIZE": str(self.pool)})
        if self.mode in ("store", "hybrid"):
            env.update(
                {
                    # The project's own fsspec probe and DuckDB's secret read these.
                    "AWS_ACCESS_KEY_ID": S3_KEY,
                    "AWS_SECRET_ACCESS_KEY": S3_SECRET,
                    "FSSPEC_S3_ENDPOINT_URL": S3_ENDPOINT,
                    "PLANNING_SOURCE_MODE": self.mode,
                    "PLANNING_STORE_SOURCE_ROOT": self.sources_uri,
                    "PLANNING_STORE_KEY_ID": S3_KEY,
                    "PLANNING_STORE_SECRET": S3_SECRET,
                    "PLANNING_STORE_ENDPOINT": urlsplit(S3_ENDPOINT).netloc,
                }
            )
        if self.mode == "store":
            env["BARCA_REMOTE_URI"] = self.store_uri
            # barca's own client; it may go through a counting proxy.
            options = {"key": S3_KEY, "secret": S3_SECRET, "endpoint_url": self.store_endpoint}
            env["BARCA_STORAGE_OPTIONS"] = json.dumps({"s3": options})
        env.update(self.extra)
        return env

    def cli(self, *args: str, env: dict | None = None, timeout: int = 300):
        return subprocess.run(
            [_find_binary(), *args],
            cwd=self.root,
            env={**self.env, **(env or {})},
            capture_output=True,
            text=True,
            timeout=timeout,
            check=False,
        )

    def ok(self, *args: str, env: dict | None = None) -> dict:
        proc = self.cli(*args, env=env)
        assert proc.returncode == 0, (
            f"barca {' '.join(args[:2])}: exit {proc.returncode}\n{proc.stderr}"
        )
        return json.loads(proc.stdout) if proc.stdout.strip() else {}

    def status(self) -> dict[str, dict]:
        out = self.ok("status", "assets.py", "--json")
        assert not out["truncated"]
        return {n["name"]: n for n in out["nodes"]}

    def local_copy(self, node: dict) -> Path:
        """This machine's copy of a node's artifact, wherever its recorded location is."""
        artifact = node["cache"]["artifact"]
        if "://" not in artifact:
            return self.root / artifact
        return self.root / ".barca" / "artifacts" / artifact.split("/artifacts/", 1)[1]

    def clone(self, dest: Path, **changes) -> Machine:
        """A fresh checkout of the same project on another machine: no `.barca/`."""
        shutil.copytree(self.root, dest, ignore=shutil.ignore_patterns(".barca"))
        fields = {**self.__dict__, "root": dest, "extra": dict(self.extra), **changes}
        return Machine(**fields)

    def measured(self, *args: str) -> tuple[dict, int, int]:
        """Run barca and measure its memory (`benchmarks/planning_pipeline/measure.py`).
        Returns (JSON result, peak RSS of the whole process tree, peak RSS of its largest
        single process), in bytes."""
        out = self.root / ".measured.out"
        err = self.root / ".measured.err"
        with out.open("w") as o, err.open("w") as e:
            code, tree_peak, process_peak = measure().run(
                [_find_binary(), *args], cwd=self.root, env=self.env, stdout=o, stderr=e
            )
        assert code == 0, f"barca {args[0]}: exit {code}\n{err.read_text()}"
        result = json.loads(out.read_text())
        out.unlink()
        err.unlink()
        return result, tree_peak, process_peak


def measure():
    """benchmarks/planning_pipeline/measure.py, loaded by path."""
    name = "barca_planning_pipeline_measure"
    if name not in sys.modules:
        spec = importlib.util.spec_from_file_location(name, GENERATOR.parent / "measure.py")
        sys.modules[name] = importlib.util.module_from_spec(spec)
        spec.loader.exec_module(sys.modules[name])
    return sys.modules[name]


def new_machine(
    dest: Path,
    mode: str,
    bucket: Bucket | None,
    *,
    scale: int = 1,
    prefix: str = "",
    store_endpoint: str = S3_ENDPOINT,
) -> tuple[Machine, object]:
    """Write the project into `dest` and set it up for `mode`. Returns (machine, project)."""
    project = write_planning_pipeline(dest, scale=scale)
    machine = Machine(root=project.root, mode=mode, store_endpoint=store_endpoint)
    if mode in ("store", "hybrid"):
        assert bucket is not None
        prefix = prefix or f"{mode}-{uuid.uuid4().hex[:6]}"
        machine.bucket = bucket
        machine.sources_uri = bucket.put_sources(project.root, f"{prefix}/sources")
        machine.store_uri = bucket.uri(f"{prefix}/barca") if mode == "store" else ""
        shutil.rmtree(project.root / "data")
    return machine, project
