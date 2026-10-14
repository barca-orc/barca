"""Run hashes of a project that uses none of the patterns #194 started tracking (classes,
imports inside a function body, aliased `from` imports of project modules, a module used as a
value, a root module imported from a subdirectory, a pipeline file inside a package).

A cold `--dry-run` run hash depends only on the definitions (the function from `def` on, the
decorator arguments that count, the dependency cone) of a step and its upstreams. If one of
these moves, every cache entry of projects like this one is recomputed after upgrading: change
them only on purpose, with a release note.

They were pinned to the values barca 0.17.0 computed until 0.19.0, which changed all of them on
purpose (#283, "After upgrading to 0.19" in `barca docs cache`). See `RUN_HASHES_0_19_0`.
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
        from helpers import compute, fact
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


        @asset()
        def recursive_helper() -> int:
            # `fact` calls itself: it is in the cone twice, as 0.17.0 hashed it.
            return fact(5)


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


        def fact(n):
            return 1 if n < 2 else n * fact(n - 1)


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

# `barca get <target> --dry-run --json`, step id -> run hash, from barca 0.19.0 on.
#
# Every value differs from the one 0.17.0 and 0.18 computed (kept below), and for the same
# single reason: up to 0.18 the definition hash covered the decorator as text (`@asset()`,
# `@asset(inputs={"rows": from_import})`) plus the freshness; from 0.19 it covers the decorator
# arguments that count, in canonical form, and the function from `def` on. `downstream` also
# moves because the run hash of its upstream `from_import` did.
#
# Nothing else moved. The dependency cones of these steps are pinned separately and still have
# their 0.17.0 values (`cone_hashes_from_0_17_0_are_unchanged` in `crates/barca-core/src/cone.rs`),
# and `run_hash_unchanged_for_pipelines_without_module_attribute_helpers` in
# `crates/barca-core/src/cache.rs` recomputes the old values from the old decorator text.
RUN_HASHES_0_19_0 = {
    "jobs/nightly.py:nightly": "a11455419b8b32ec9afb231a1d7d15eb23a8d157cbf7c01efbe7fa123586306b",
    "pipeline.py:dotted": "31677379d8ef231b6758257598610cd5b1c5a01fa5082c690c26bf11af94f0a0",
    "pipeline.py:downstream": "735807a933f890d5eae32273196833c92743ee7c41bcffbe941048cfb4b14408",
    "pipeline.py:from_import": "0947b450745fb8e992083a13d0c0b8d582d2fbe11a3f6a9262510ee693818cab",
    "pipeline.py:local_only": "14bdbde38e8938ff2866124302145d74cd803ef9eb04c318bc21766fbdcdeb6b",
    "pipeline.py:module_attr": "da14b051b0ea3bca9e92d44454fecee7a9aa9c44601a58b55d578f8d2eb7c157",
    "pipeline.py:no_deps": "04c886dc1198d92e34d35f143bc747db3f0eef2da1b1e1f229444d69062967e0",
    "pipeline.py:package_reexport": "c91872c06b81d7952718b8237030fd922f8dc92fae105a8f19f287e339dc309f",
    "pipeline.py:recursive_helper": "3d5207870fbf37dd45df4654685d25679006f249c9ccecb02834dec319a4e871",
    "pipeline.py:third_party": "1699490d20cb63b17dff5657bab53248137f3f6f89f1957a35d38b24938963c5",
}

# What barca 0.17.0 to 0.18.1 computed for the same files. Not what barca computes any more:
# kept so the change is on record next to its reason (above).
RUN_HASHES_0_17_0 = {
    "jobs/nightly.py:nightly": "855ea9c29be7031d3829cf4d8878420b4994a777cd14c8192b752be030467e98",
    "pipeline.py:dotted": "88f897c0fe7cd2fc5a456b2084d4cad1e3afa5d2cef39747fd9334a69300263a",
    "pipeline.py:downstream": "885e21cf50bcfedefecf39b2c701da359cf1db9bb4a81ad38460c99ca90acf8c",
    "pipeline.py:from_import": "32c83361969a1dfe3088eb7985866fd73dc05d1125906c711f1a59795a200cb7",
    "pipeline.py:local_only": "5ed1ddd2a41914973d09e2ebcb437e58b096d49fc51af136575ae64a48925380",
    "pipeline.py:module_attr": "553577bf11607c545f127d09b0fcca2b19bec8c2790f676598b570ca35e29f33",
    "pipeline.py:no_deps": "aded58bb5cd144572b0a1c9c4058a89dc7ef543afe5267467f5062214bb1fd23",
    "pipeline.py:package_reexport": "4acda3d4a0afe22a0b322e83651b1ed3db122749e83b428cf82f821dc7a92767",
    "pipeline.py:recursive_helper": "5f6fac423a49e526665f44a55aca401089ede0f22455f825a410b245070b2451",
    "pipeline.py:third_party": "e3892716fb7c5a003ea61f8abc2dc1efbf4b4cb509172309891af93e5a5bad8e",
}


def cold_run_hashes(root: Path, files: dict | None = None) -> dict:
    for rel, code in (files or FILES).items():
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


def test_run_hashes_from_0_19_0_are_unchanged(tmp_path):
    hashes = cold_run_hashes(tmp_path)
    assert hashes == RUN_HASHES_0_19_0
    # Every step moved in 0.19.0, none by accident kept its old value.
    assert not set(hashes.values()) & set(RUN_HASHES_0_17_0.values())


def test_run_hashes_do_not_depend_on_how_the_decorators_are_written(tmp_path):
    """The same project with every decorator reformatted, re-quoted, commented, its keywords
    reordered, and `description`, `tags`, `retries`, `retry_backoff`, `timeout_seconds` and
    `freshness` added: the pinned run hashes, unchanged."""
    bare = """@asset(  # reformatted
            description='A step',
            tags={"team": 'data',},
            retries=3, retry_backoff=1.5,
            timeout_seconds=60,
            freshness=Always,
        )"""
    with_inputs = """@asset(
            freshness = Manual,  # only on request
            description = "Downstream",
            inputs = {
                'rows' : from_import ,
            },
            retries=2,
        )"""
    files = dict(FILES)
    for rel in ("pipeline.py", "jobs/nightly.py"):
        code = files[rel]
        assert "@asset()" in code
        code = code.replace("@asset()", bare)
        code = code.replace('@asset(inputs={"rows": from_import})', with_inputs)
        code = code.replace("from barca import asset", "from barca import Always, Manual, asset")
        files[rel] = code
    assert "'rows' : from_import" in files["pipeline.py"]
    assert cold_run_hashes(tmp_path, files) == RUN_HASHES_0_19_0
