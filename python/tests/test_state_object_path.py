"""A state URI names one history object; artifact roots remain prefixes (#298)."""

import json
import subprocess

import pytest

from barca.api import _find_binary

from .test_serve_robustness import _env


@pytest.mark.parametrize("scheme", ["plain", "file"])
def test_state_directory_names_an_object_before_user_import(tmp_path, scheme):
    shared = tmp_path / "shared"
    shared.mkdir()
    (shared / "keep").write_text("existing data")
    (tmp_path / "barca.toml").write_text("")
    (tmp_path / "pipeline.py").write_text(
        "from pathlib import Path\nfrom barca import asset\n"
        "Path('imported').touch()\n@asset\ndef value():\n    return 42\n"
    )
    env = _env()
    env.update(BARCA_STATE_URI=shared.as_uri() if scheme == "file" else str(shared))
    proc = subprocess.run(
        [_find_binary(), "get", "pipeline.py", "--json"],
        cwd=tmp_path,
        env=env,
        text=True,
        capture_output=True,
        timeout=15,
    )
    assert proc.returncode == 3, proc.stderr
    envelope = json.loads(proc.stderr.strip().splitlines()[-1])
    assert envelope["kind"] == "infra"
    assert "BARCA_STATE_URI" in envelope["error"]
    assert "file or object" in envelope["error"]
    assert "metadata.db" in envelope["error"]
    assert not (tmp_path / "imported").exists()
    assert (shared / "keep").read_text() == "existing data"
    assert sorted(p.name for p in shared.iterdir()) == ["keep"]
