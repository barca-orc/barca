"""Settings fixed from the environment when this module is first imported.

Nothing here is declared to barca: switching a source changes what a root reads without
changing its run hash, as in the original pipeline.
"""

import os
from pathlib import Path

PROJECT_DIR = Path(__file__).resolve().parent


def _path(value: str) -> str:
    """A local path relative to the project, or a URI left as it is."""
    if "://" in value:
        return value.rstrip("/")
    return str((PROJECT_DIR / value).resolve())


class Settings:
    PROJECT_DIR = PROJECT_DIR
    # local: sources under a local directory; store: sources in an object store;
    # hybrid: sources in the store, barca's own artifacts local.
    SOURCE_MODE = os.environ.get("PLANNING_SOURCE_MODE", "local")
    LOCAL_SOURCE_ROOT = _path(os.environ.get("PLANNING_LOCAL_SOURCE_ROOT", "data"))
    STORE_SOURCE_ROOT = os.environ.get("PLANNING_STORE_SOURCE_ROOT", "")
    STORE_KEY_ID = os.environ.get("PLANNING_STORE_KEY_ID", "")
    STORE_SECRET = os.environ.get("PLANNING_STORE_SECRET", "")
    STORE_ENDPOINT = os.environ.get("PLANNING_STORE_ENDPOINT", "")
    PLAN_YEAR = int(os.environ.get("PLANNING_PLAN_YEAR", "2026"))
    WEEKS = int(os.environ.get("PLANNING_WEEKS", "6"))
    REGISTRY_API_URL = os.environ.get("REGISTRY_API_URL", "")
    REGISTRY_API_KEY = os.environ.get("REGISTRY_API_KEY", "")

    @classmethod
    def source_root(cls) -> str:
        if cls.SOURCE_MODE in ("store", "hybrid"):
            return cls.STORE_SOURCE_ROOT
        return cls.LOCAL_SOURCE_ROOT
