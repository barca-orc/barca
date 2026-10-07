"""Error propagation tests — verify clean, user-friendly error messages.

Tests that error messages contain the key diagnostic information
(file names, error types, function names) and do NOT leak barca
internal paths like _worker.py.
"""

import textwrap

import barca
import pytest
from barca.api import BarcaError


@pytest.fixture(autouse=True)
def clean_barca_dir(tmp_path, monkeypatch):
    # The API resolves .barca relative to cwd. Give each test its own project,
    # so another pytest worker cannot delete its database or input artifacts.
    monkeypatch.chdir(tmp_path)


def write_module(tmp_path, filename, code):
    p = tmp_path / filename
    p.write_text(textwrap.dedent(code))
    return str(p)


class TestUserErrors:
    def test_syntax_error(self, tmp_path):
        """Python syntax error shows file name and syntax issue."""
        f = write_module(
            tmp_path,
            "bad_syntax.py",
            """\
            from barca import asset

            @asset()
            def broken():
                return {
            """,
        )
        with pytest.raises(BarcaError) as exc_info:
            barca.get(f)
        msg = str(exc_info.value)
        assert "bad_syntax.py" in msg

    def test_runtime_zerodivision(self, tmp_path):
        """ZeroDivisionError shows user file and line, not worker internals."""
        f = write_module(
            tmp_path,
            "divzero.py",
            """\
            from barca import asset

            @asset()
            def oops():
                return 1 / 0
            """,
        )
        with pytest.raises(BarcaError) as exc_info:
            barca.get(f)
        msg = str(exc_info.value)
        assert "ZeroDivisionError" in msg
        assert "divzero.py" in msg
        assert "_worker.py" not in msg

    def test_runtime_keyerror(self, tmp_path):
        """KeyError shows the key and user code location."""
        f = write_module(
            tmp_path,
            "keyerr.py",
            """\
            from barca import asset

            @asset()
            def source():
                return {"a": 1}

            @asset(inputs={"data": source})
            def consumer(data):
                return data["missing_key"]
            """,
        )
        with pytest.raises(BarcaError) as exc_info:
            barca.get(f)
        msg = str(exc_info.value)
        assert "KeyError" in msg
        assert "missing_key" in msg
        assert "_worker.py" not in msg

    def test_import_error(self, tmp_path):
        """ModuleNotFoundError shows the module name."""
        f = write_module(
            tmp_path,
            "bad_import.py",
            """\
            from barca import asset
            import nonexistent_module_xyz_12345

            @asset()
            def uses_bad_import():
                return nonexistent_module_xyz_12345.do_thing()
            """,
        )
        with pytest.raises(BarcaError) as exc_info:
            barca.get(f)
        msg = str(exc_info.value)
        assert "nonexistent_module_xyz_12345" in msg
        assert "_worker.py" not in msg

    def test_file_not_found(self):
        """Missing file shows the file path."""
        with pytest.raises(BarcaError) as exc_info:
            barca.get("/tmp/definitely_does_not_exist_12345.py")
        msg = str(exc_info.value)
        assert "definitely_does_not_exist_12345.py" in msg

    def test_asset_not_found(self, tmp_path):
        """Unknown asset lists available assets."""
        f = write_module(
            tmp_path,
            "simple.py",
            """\
            from barca import asset

            @asset()
            def real_asset():
                return 42
            """,
        )
        with pytest.raises(BarcaError) as exc_info:
            barca.get("nonexistent", f)
        msg = str(exc_info.value)
        assert "nonexistent" in msg
        assert "real_asset" in msg

    def test_wrong_arg_count(self, tmp_path):
        """TypeError from wrong args shows function name."""
        f = write_module(
            tmp_path,
            "wrong_args.py",
            """\
            from barca import asset

            @asset()
            def needs_no_args():
                return 1

            @asset(inputs={"a": needs_no_args, "b": needs_no_args})
            def takes_one(a):
                return a
            """,
        )
        with pytest.raises(BarcaError) as exc_info:
            barca.get(f)
        msg = str(exc_info.value)
        assert "TypeError" in msg
        assert "takes_one" in msg
        assert "_worker.py" not in msg

    def test_asset_returns_none(self, tmp_path):
        """Asset returning None is valid, not an error."""
        f = write_module(
            tmp_path,
            "none_asset.py",
            """\
            from barca import asset

            @asset()
            def returns_none():
                return None
            """,
        )
        # Should not raise
        result = barca.get(f)
        assert result is None
