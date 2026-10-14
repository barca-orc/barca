"""Tests for cross-file cone analysis: subdirectory imports and __init__.py re-exports."""

import textwrap

import barca
import pytest


@pytest.fixture(autouse=True)
def clean_barca_dir(tmp_path, monkeypatch):
    # The API resolves .barca relative to cwd. Give each test its own project,
    # so another pytest worker cannot delete its database or input artifacts.
    monkeypatch.chdir(tmp_path)


def write_file(base, relpath, code):
    p = base / relpath
    p.parent.mkdir(parents=True, exist_ok=True)
    p.write_text(textwrap.dedent(code))
    return str(p)


class TestSubdirectoryImports:
    """from subpkg.utils import helper should be tracked for cache invalidation."""

    def test_subdir_import_detected(self, tmp_path):
        """Changing a helper in a subdirectory should invalidate the asset."""
        write_file(
            tmp_path,
            "utils/math.py",
            """
            def double(x):
                return x * 2
        """,
        )
        asset_file = write_file(
            tmp_path,
            "pipeline.py",
            """
            from barca import asset
            from utils.math import double

            @asset()
            def result():
                return {"value": double(21)}
        """,
        )

        # First run
        r1 = barca.api._exec(["get", "result", asset_file])
        assert r1["steps_executed"] == 1

        # Second run — cached
        r2 = barca.api._exec(["get", "result", asset_file])
        assert r2["steps_executed"] == 0

        # Change the helper in the subdirectory
        write_file(
            tmp_path,
            "utils/math.py",
            """
            def double(x):
                return x * 99
        """,
        )

        # Third run — should detect the change and re-execute
        r3 = barca.api._exec(["get", "result", asset_file])
        assert r3["steps_executed"] == 1

    def test_nested_subdir_import(self, tmp_path):
        """from pkg.sub.module import func should work."""
        write_file(
            tmp_path,
            "pkg/sub/helpers.py",
            """
            def compute(x):
                return x + 1
        """,
        )
        asset_file = write_file(
            tmp_path,
            "pipeline.py",
            """
            from barca import asset
            from pkg.sub.helpers import compute

            @asset()
            def result():
                return {"value": compute(10)}
        """,
        )

        r1 = barca.api._exec(["get", "result", asset_file])
        assert r1["steps_executed"] == 1

        r2 = barca.api._exec(["get", "result", asset_file])
        assert r2["steps_executed"] == 0

        # Change nested helper
        write_file(
            tmp_path,
            "pkg/sub/helpers.py",
            """
            def compute(x):
                return x + 999
        """,
        )

        r3 = barca.api._exec(["get", "result", asset_file])
        assert r3["steps_executed"] == 1


class TestRelativeImports:
    """from .submodule import func (relative imports in __init__.py)."""

    def test_relative_import_in_init(self, tmp_path):
        """__init__.py using `from .core import transform` should track core.py changes."""
        write_file(
            tmp_path,
            "mylib/__init__.py",
            """
            from .core import transform
        """,
        )
        write_file(
            tmp_path,
            "mylib/core.py",
            """
            def transform(x):
                return x * 2
        """,
        )
        asset_file = write_file(
            tmp_path,
            "pipeline.py",
            """
            from barca import asset
            from mylib import transform

            @asset()
            def result():
                return {"value": transform(21)}
        """,
        )

        r1 = barca.api._exec(["get", "result", asset_file])
        assert r1["steps_executed"] == 1

        r2 = barca.api._exec(["get", "result", asset_file])
        assert r2["steps_executed"] == 0

        # Change the implementation behind the relative import
        write_file(
            tmp_path,
            "mylib/core.py",
            """
            def transform(x):
                return x * 99
        """,
        )

        r3 = barca.api._exec(["get", "result", asset_file])
        assert r3["steps_executed"] == 1

    def test_relative_import_in_regular_submodule(self, tmp_path):
        """mylib/core.py re-exporting `from .util import h` must resolve to
        the sibling `mylib.util`, not `mylib.core.util` (parent_module used
        to be the submodule's own dotted name instead of its parent
        package)."""
        write_file(tmp_path, "mylib/__init__.py", "")
        write_file(
            tmp_path,
            "mylib/util.py",
            """
            def h():
                return 1
        """,
        )
        write_file(
            tmp_path,
            "mylib/core.py",
            """
            from .util import h
        """,
        )
        asset_file = write_file(
            tmp_path,
            "pipeline.py",
            """
            from barca import asset
            from mylib.core import h

            @asset()
            def result():
                return {"value": h()}
        """,
        )

        r1 = barca.api._exec(["get", "result", asset_file])
        assert r1["steps_executed"] == 1

        r2 = barca.api._exec(["get", "result", asset_file])
        assert r2["steps_executed"] == 0

        # Change the sibling module the relative import actually points at.
        write_file(
            tmp_path,
            "mylib/util.py",
            """
            def h():
                return 999
        """,
        )

        r3 = barca.api._exec(["get", "result", asset_file])
        assert r3["steps_executed"] == 1

    def test_relative_import_level_two(self, tmp_path):
        """mylib/sub/core.py re-exporting `from ..util import h` (level=2)
        must walk two package levels up to `mylib.util`, distinct from
        level=1."""
        write_file(tmp_path, "mylib/__init__.py", "")
        write_file(tmp_path, "mylib/sub/__init__.py", "")
        write_file(
            tmp_path,
            "mylib/util.py",
            """
            def h():
                return 1
        """,
        )
        write_file(
            tmp_path,
            "mylib/sub/core.py",
            """
            from ..util import h
        """,
        )
        asset_file = write_file(
            tmp_path,
            "pipeline.py",
            """
            from barca import asset
            from mylib.sub.core import h

            @asset()
            def result():
                return {"value": h()}
        """,
        )

        r1 = barca.api._exec(["get", "result", asset_file])
        assert r1["steps_executed"] == 1

        r2 = barca.api._exec(["get", "result", asset_file])
        assert r2["steps_executed"] == 0

        write_file(
            tmp_path,
            "mylib/util.py",
            """
            def h():
                return 999
        """,
        )

        r3 = barca.api._exec(["get", "result", asset_file])
        assert r3["steps_executed"] == 1


class TestInitReExports:
    """from mypackage import thing where thing is in __init__.py."""

    def test_init_reexport_detected(self, tmp_path):
        """Changing a function re-exported through __init__.py should invalidate."""
        write_file(
            tmp_path,
            "mylib/__init__.py",
            """
            from mylib.core import transform
        """,
        )
        write_file(
            tmp_path,
            "mylib/core.py",
            """
            def transform(x):
                return x * 2
        """,
        )
        asset_file = write_file(
            tmp_path,
            "pipeline.py",
            """
            from barca import asset
            from mylib import transform

            @asset()
            def result():
                return {"value": transform(21)}
        """,
        )

        r1 = barca.api._exec(["get", "result", asset_file])
        assert r1["steps_executed"] == 1

        r2 = barca.api._exec(["get", "result", asset_file])
        assert r2["steps_executed"] == 0

        # Change the underlying implementation
        write_file(
            tmp_path,
            "mylib/core.py",
            """
            def transform(x):
                return x * 99
        """,
        )

        r3 = barca.api._exec(["get", "result", asset_file])
        assert r3["steps_executed"] == 1


class TestTransitiveCrossFileImports:
    """A helper's own dependency can be an import from a *third* file — that
    third file's changes must still invalidate the cache."""

    def test_helper_calling_a_transitively_imported_function(self, tmp_path):
        """mylib/core.py's `helper()` calls `h()`, which core.py imports from
        mylib/util.py. Changing util.py must invalidate `result`, even though
        util.py is two hops away from the entry file's own import."""
        write_file(
            tmp_path,
            "mylib/util.py",
            """
            def h():
                return 1
        """,
        )
        write_file(
            tmp_path,
            "mylib/core.py",
            """
            from mylib.util import h

            def helper():
                return h()
        """,
        )
        asset_file = write_file(
            tmp_path,
            "pipeline.py",
            """
            from barca import asset
            from mylib.core import helper

            @asset()
            def result():
                return {"value": helper()}
        """,
        )

        r1 = barca.api._exec(["get", "result", asset_file])
        assert r1["steps_executed"] == 1

        r2 = barca.api._exec(["get", "result", asset_file])
        assert r2["steps_executed"] == 0

        # Change the transitively-imported function two hops away.
        write_file(
            tmp_path,
            "mylib/util.py",
            """
            def h():
                return 999
        """,
        )

        r3 = barca.api._exec(["get", "result", asset_file])
        assert r3["steps_executed"] == 1
