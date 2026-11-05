"""Settings every test process needs before any test module is imported, and the fixtures
the test modules share (the helpers behind them are in `helpers.py` and `served.py`)."""

import os
from pathlib import Path

import pytest

from .planning_pipeline import write_planning_pipeline
from .served import Serve

# gcsfs >= 2026.10 defaults to an experimental mode that asks the gRPC Storage Control API for a
# bucket's type, and fake-gcs-server only speaks HTTP, so the call hangs. gcsfs reads the toggle
# once, when it is first imported, so it has to be set before any test can import it: a test
# that set it later for itself stalled whenever an earlier test had imported gcsfs first.
os.environ.setdefault("GCSFS_EXPERIMENTAL_ZB_HNS_SUPPORT", "false")


@pytest.fixture()
def planning_pipeline(tmp_path):
    """The 116-node planning-pipeline project (#250), written into a fresh temp dir."""
    return write_planning_pipeline(tmp_path / "planning_pipeline")


@pytest.fixture()
def clean_barca_dir(tmp_path, monkeypatch):
    """Run the test in its own empty directory.

    The API resolves `.barca` relative to the working directory; a module that calls
    `barca.get(...)` in-process opts in with
    `pytestmark = pytest.mark.usefixtures("clean_barca_dir")`, so another pytest worker
    cannot delete its database or input artifacts.
    """
    monkeypatch.chdir(tmp_path)
    return tmp_path


@pytest.fixture()
def project(request, tmp_path) -> Path:
    """A project directory holding the test module's `PIPELINE` as `pipeline.py`.

    A module with more files, or none, defines its own `project` fixture (which shadows
    this one).
    """
    pipeline = getattr(request.module, "PIPELINE", None)
    if pipeline is None:
        pytest.fail(f"{request.module.__name__} has no PIPELINE; define its own `project`")
    (tmp_path / "pipeline.py").write_text(pipeline)
    return tmp_path


@pytest.fixture()
def serve(tmp_path):
    """Start `barca serve` children (`served.Serve`); every one is stopped at teardown.

    `serve("--no-schedule")` serves `pipeline.py` in `tmp_path` and returns once `/health`
    answers; `serve(root=other, files=("a.py", "b.py"), env=..., wait="bind")` passes the
    `Serve` options through. A test that stops a server itself (`server.stop()`,
    `server.kill()`) may; teardown reaps whatever is still running.
    """
    started: list[Serve] = []

    def start(*args: str, root: Path | None = None, **options) -> Serve:
        server = Serve(tmp_path if root is None else root, *args, **options)
        started.append(server)
        return server

    yield start
    for server in reversed(started):
        server.stop()
