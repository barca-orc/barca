"""Settings every test process needs before any test module is imported."""

import os

import pytest

from .planning_pipeline import write_planning_pipeline

# gcsfs >= 2026.10 defaults to an experimental mode that asks the gRPC Storage Control API for a
# bucket's type, and fake-gcs-server only speaks HTTP, so the call hangs. gcsfs reads the toggle
# once, when it is first imported, so it has to be set before any test can import it: a test
# that set it later for itself stalled whenever an earlier test had imported gcsfs first.
os.environ.setdefault("GCSFS_EXPERIMENTAL_ZB_HNS_SUPPORT", "false")


@pytest.fixture()
def planning_pipeline(tmp_path):
    """The 116-node planning-pipeline project (#250), written into a fresh temp dir."""
    return write_planning_pipeline(tmp_path / "planning_pipeline")
