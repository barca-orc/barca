"""Version diagnostics preserve readable metadata and enforce project requirements (#672)."""

import json
import sqlite3

import pytest

from barca import __version__

from .helpers import barca


def test_doctor_rejects_project_requirement(tmp_path):
    (tmp_path / "pipeline.py").write_text(
        "from barca import asset\n@asset()\ndef value(): return 1\n"
    )
    (tmp_path / "pyproject.toml").write_text(
        '[project]\nname = "demo"\nversion = "1"\ndependencies = ["barca>=99"]\n'
    )
    result = barca(tmp_path, "doctor", "--json")
    assert result.returncode == 3, result.stdout
    check = next(c for c in json.loads(result.stdout)["checks"] if c["name"] == "project_version")
    assert not check["ok"]
    assert "barca>=99" in check["detail"] and __version__ in check["detail"]


def test_metadata_records_writer_and_warns_without_overwriting_on_read(tmp_path):
    (tmp_path / "pipeline.py").write_text(
        "from barca import asset\n@asset()\ndef value(): return 1\n"
    )
    result = barca(tmp_path, "get", "pipeline.py", "--json")
    assert result.returncode == 0, result.stderr
    path = tmp_path / ".barca" / "metadata.db"
    with sqlite3.connect(path) as conn:
        assert conn.execute(
            "SELECT value FROM metadata_info WHERE key='last_writer_version'"
        ).fetchone() == (__version__,)
        conn.execute("UPDATE metadata_info SET value='99.0.0' WHERE key='last_writer_version'")
    result = barca(tmp_path, "history", "--json")
    assert result.returncode == 0, result.stderr
    assert "99.0.0" in result.stderr and __version__ in result.stderr
    assert result.stderr.count("last written by barca") == 1
    with sqlite3.connect(path) as conn:
        assert conn.execute(
            "SELECT value FROM metadata_info WHERE key='last_writer_version'"
        ).fetchone() == ("99.0.0",)


def test_metadata_actual_write_replaces_newer_writer(tmp_path):
    (tmp_path / "pipeline.py").write_text(
        "from barca import sensor\n@sensor()\ndef value(): return 1\n"
    )
    assert barca(tmp_path, "get", "pipeline.py", "--json").returncode == 0
    path = tmp_path / ".barca" / "metadata.db"
    with sqlite3.connect(path) as conn:
        conn.execute("UPDATE metadata_info SET value='99.0.0' WHERE key='last_writer_version'")
    result = barca(tmp_path, "get", "pipeline.py", "--json")
    assert result.returncode == 0, result.stderr
    assert "last written by barca 99.0.0" in result.stderr
    with sqlite3.connect(path) as conn:
        assert conn.execute(
            "SELECT value FROM metadata_info WHERE key='last_writer_version'"
        ).fetchone() == (__version__,)


@pytest.mark.parametrize(
    "requirement, ok",
    [
        (f"barca=={__version__}", True),
        ("barca[s3]>=0.1,<99", True),
        (f"barca~={__version__.rsplit('.', 1)[0]}.0", True),
        (f"barca=={__version__.rsplit('.', 1)[0]}.*", True),
        (f"barca!={__version__}", False),
        ("barca<0.1", False),
        ('barca>=99; python_version < "2"', True),
        ('barca>=99; python_version >= "3"', False),
        ("barca @ https://example.org/barca.whl", False),
    ],
)
def test_doctor_project_requirement_forms(tmp_path, requirement, ok):
    (tmp_path / "pipeline.py").write_text(
        "from barca import asset\n@asset()\ndef value(): return 1\n"
    )
    (tmp_path / "pyproject.toml").write_text(
        '[project]\nname = "demo"\nversion = "1"\ndependencies = '
        + json.dumps([requirement])
        + "\n"
    )
    result = barca(tmp_path, "doctor", "--json")
    assert result.returncode == (0 if ok else 3), result.stdout + result.stderr
    check = next(c for c in json.loads(result.stdout)["checks"] if c["name"] == "project_version")
    assert check["ok"] is ok


@pytest.mark.parametrize("writer", [__version__, f"{__version__}+build", "0.0.1", "unrecognized"])
def test_metadata_equal_older_or_unrecognized_version_is_silent(tmp_path, writer):
    (tmp_path / "pipeline.py").write_text(
        "from barca import asset\n@asset()\ndef value(): return 1\n"
    )
    assert barca(tmp_path, "get", "pipeline.py", "--json").returncode == 0
    with sqlite3.connect(tmp_path / ".barca" / "metadata.db") as conn:
        conn.execute("UPDATE metadata_info SET value=? WHERE key='last_writer_version'", (writer,))
    result = barca(tmp_path, "history", "--json")
    assert result.returncode == 0, result.stderr
    assert "last written by barca" not in result.stderr


def test_read_only_server_warns_for_newer_writer_and_preserves_it(tmp_path, serve):
    (tmp_path / "pipeline.py").write_text(
        "from barca import asset\n@asset()\ndef value(): return 1\n"
    )
    assert barca(tmp_path, "get", "pipeline.py", "--json").returncode == 0
    path = tmp_path / ".barca" / "metadata.db"
    with sqlite3.connect(path) as conn:
        conn.execute("UPDATE metadata_info SET value='99.0.0' WHERE key='last_writer_version'")
    server = serve("--read-only")
    assert "last written by barca 99.0.0" in server.stderr
    assert __version__ in server.stderr
    server.stop()
    with sqlite3.connect(path) as conn:
        assert conn.execute(
            "SELECT value FROM metadata_info WHERE key='last_writer_version'"
        ).fetchone() == ("99.0.0",)


@pytest.mark.parametrize(
    "requirement,running,ok",
    [
        ("BaRcA[s3]>=1.0,!=1.5", "1.4", True),
        ("barca>=1.0,!=1.5", "1.5", False),
        ("barca~=1.4.5", "1.5", False),
        ("barca==1.4.*", "1.4.9", True),
        ("barca==1.4", "1.4+local", True),
        ("barca>=1.4rc1", "1.4rc2", True),
        ('barca>=99; python_version < "2" or (os_name == "not-an-os")', "1", True),
        ('barca>=99; python_version >= "3" and extra == ""', "1", False),
        ('barca>=99; extra == "s3"', "1", True),
        ("barca @ https://example.org/barca.whl", "1", False),
        ("not valid :::", "1", False),
    ],
)
def test_doctor_helper_pep_controls(tmp_path, requirement, running, ok):
    from barca import _doctor

    (tmp_path / "pyproject.toml").write_text(
        "[project]\ndependencies = " + json.dumps([requirement]) + "\n"
    )
    result = _doctor.project_version(tmp_path, running)
    assert result["ok"] is ok, result


def test_doctor_helper_checks_cli_version_not_python_package(tmp_path):
    from barca import _doctor

    (tmp_path / "pyproject.toml").write_text('[project]\ndependencies = ["barca==99"]\n')
    assert _doctor.project_version(tmp_path, "99")["ok"]
    assert not _doctor.project_version(tmp_path, __version__)["ok"]
