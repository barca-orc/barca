"""Run hashes recorded with barca 0.17.0, for a project that uses none of the patterns #194
started tracking (classes, imports inside a function body, aliased `from` imports of project
modules, a module used as a value, a root module imported from a subdirectory, a pipeline file
inside a package).

A cold `--dry-run` run hash depends only on the definitions (source, dependency cone, decorator
arguments) of a step and its upstreams. If one of these moves, every cache entry of projects
like this one is recomputed after upgrading: change them only on purpose, with a release note.
"""

import json
import subprocess
import textwrap
from pathlib import Path

from barca.api import _find_binary

FILES = {
    "barca.toml": "",
    "pipeline.py": """
        import json
        import os.path as osp

        import helpers
        import pkg.core
        import pkg.sub.deep as deep_mod
        from barca import asset
        from helpers import compute
        from numpy import array as arr
        from pkg import core, transform
        from pkg.sub.deep import deep

        RATE = 2
        LIMITS = [RATE, 10]
        TABLE: dict = {"a": RATE}


        def local_helper(x):
            return x * RATE


        def chained(x):
            return local_helper(x) + LIMITS[0]


        @asset()
        def no_deps() -> dict:
            return {"v": 1}


        @asset()
        def local_only() -> int:
            return chained(1) + TABLE["a"]


        @asset()
        def from_import() -> list:
            return [compute(1)]


        @asset()
        def module_attr() -> str:
            return helpers.compute(2) + str(helpers.BASE)


        @asset()
        def package_reexport() -> int:
            return transform(RATE)


        @asset()
        def dotted() -> int:
            return pkg.core.transform(1) + deep_mod.deep(2) + core.transform(3) + deep(4)


        @asset()
        def third_party() -> list:
            return arr(json.dumps(osp.join("a", "b")))


        @asset(inputs={"rows": from_import})
        def downstream(rows: list, RATE=None) -> dict:
            out = [local_helper(len(r)) for r in rows]
            total = sum(out)
            return {"total": total, "f": f"{RATE}", "lam": (lambda v: v + 1)(total)}
    """,
    "helpers.py": """
        import json
        import os.path as osp

        from util import shared

        BASE = 3


        def _inner(x):
            return x + BASE


        def compute(x):
            return json.dumps(_inner(x)) + shared() + osp.sep


        def unused():
            return 0
    """,
    "util.py": """
        def shared():
            return "s"
    """,
    "pkg/__init__.py": """
        from .core import transform
        from . import core
    """,
    "pkg/core.py": """
        from .consts import SCALE


        def transform(x):
            return x * SCALE
    """,
    "pkg/consts.py": "SCALE = 2\n",
    "pkg/sub/__init__.py": "",
    "pkg/sub/deep.py": """
        from ..core import transform


        def deep(x):
            return transform(x) + 1
    """,
    # A pipeline in a plain subdirectory, importing a module beside it and a package below it.
    "jobs/nightly.py": """
        from barca import asset
        from local import scale
        from lib.text import shout

        import local


        @asset()
        def nightly() -> str:
            return shout("x") * scale(2) + local.SUFFIX
    """,
    "jobs/local.py": """
        SUFFIX = "!"
        FACTOR = 3


        def scale(n):
            return n * FACTOR
    """,
    "jobs/lib/text.py": """
        def shout(s):
            return s.upper()
    """,
}

# `barca get <target> --dry-run --json` with barca 0.17.0, step id -> run hash.
RUN_HASHES_0_17_0 = {
    "jobs/nightly.py:nightly": "855ea9c29be7031d3829cf4d8878420b4994a777cd14c8192b752be030467e98",
    "pipeline.py:dotted": "88f897c0fe7cd2fc5a456b2084d4cad1e3afa5d2cef39747fd9334a69300263a",
    "pipeline.py:downstream": "885e21cf50bcfedefecf39b2c701da359cf1db9bb4a81ad38460c99ca90acf8c",
    "pipeline.py:from_import": "32c83361969a1dfe3088eb7985866fd73dc05d1125906c711f1a59795a200cb7",
    "pipeline.py:local_only": "5ed1ddd2a41914973d09e2ebcb437e58b096d49fc51af136575ae64a48925380",
    "pipeline.py:module_attr": "553577bf11607c545f127d09b0fcca2b19bec8c2790f676598b570ca35e29f33",
    "pipeline.py:no_deps": "aded58bb5cd144572b0a1c9c4058a89dc7ef543afe5267467f5062214bb1fd23",
    "pipeline.py:package_reexport": "4acda3d4a0afe22a0b322e83651b1ed3db122749e83b428cf82f821dc7a92767",
    "pipeline.py:third_party": "e3892716fb7c5a003ea61f8abc2dc1efbf4b4cb509172309891af93e5a5bad8e",
}


def cold_run_hashes(root: Path) -> dict:
    for rel, code in FILES.items():
        path = root / rel
        path.parent.mkdir(parents=True, exist_ok=True)
        path.write_text(textwrap.dedent(code).lstrip("\n"))
    hashes = {}
    listing = subprocess.run(
        [_find_binary(), "list", "--json", "--all"], cwd=root, capture_output=True, text=True
    )
    assert listing.returncode == 0, listing.stderr
    for node in json.loads(listing.stdout)["nodes"]:
        proc = subprocess.run(
            [_find_binary(), "get", node["id"], "--dry-run", "--json"],
            cwd=root,
            capture_output=True,
            text=True,
        )
        assert proc.returncode == 0, proc.stderr
        for step in json.loads(proc.stdout)["steps"]:
            hashes[step["id"]] = step["run_hash"]
    return hashes


def test_run_hashes_from_0_17_0_are_unchanged(tmp_path):
    assert cold_run_hashes(tmp_path) == RUN_HASHES_0_17_0
