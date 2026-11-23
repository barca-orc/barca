#!/usr/bin/env python3
"""Run with a fresh wheel-only Python: python -I scripts/check-vendored-doctor.py.

No pytest or external packaging installation is permitted. This also checks that upstream
licenses and provenance ship in the wheel and that normal worker imports stay parser-free.
"""

import importlib.metadata
import json
import subprocess
import sys
import tempfile
from pathlib import Path

import barca
import barca._worker

assert not any("packaging" in name for name in sys.modules)
assert importlib.metadata.requires("barca") is not None
assert all("extra ==" in requirement for requirement in importlib.metadata.requires("barca"))
try:
    importlib.metadata.distribution("packaging")
except importlib.metadata.PackageNotFoundError:
    pass
else:
    raise AssertionError("This check needs a fresh environment with only the barca wheel")

vendor = Path(barca.__file__).parent / "_vendor"
assert "26.3" in (vendor / "README.md").read_text()
for name in ("LICENSE", "LICENSE.APACHE", "LICENSE.BSD"):
    assert (vendor / "packaging" / name).stat().st_size > 100

binary = Path(sys.executable).parent / "barca"
with tempfile.TemporaryDirectory(prefix="barca-wheel-doctor-") as directory:
    root = Path(directory)
    (root / "pipeline.py").write_text(
        "from pathlib import Path\nfrom barca import asset\n"
        "Path('IMPORTED').touch()\n@asset()\ndef a(): return 1\n"
    )
    for requirement, ok in [
        (f"barca=={barca.__version__}", True),
        ("barca[s3]>=0.1,<99", True),
        ("barca>=99", False),
        ('barca>=99; python_version < "2"', True),
        ('barca>=99; python_version >= "3" and extra == ""', False),
        ('barca>=99; extra == "s3"', True),
        ("barca @ https://example.org/barca.whl", False),
        ("invalid requirement :::", False),
    ]:
        (root / "pyproject.toml").write_text(
            "[project]\ndependencies = " + json.dumps([requirement]) + "\n"
        )
        result = subprocess.run(
            [str(binary), "doctor", "--json"], cwd=root, capture_output=True, text=True, timeout=30
        )
        assert result.returncode == (0 if ok else 3), result.stdout + result.stderr
        check = next(
            c for c in json.loads(result.stdout)["checks"] if c["name"] == "project_version"
        )
        assert check["ok"] is ok, check
        assert not (root / "IMPORTED").exists()
        assert not (root / ".barca/metadata.db").exists()
print(
    "wheel-only doctor: 8 requirement controls passed; no external dependencies or parser imports in workers"
)
