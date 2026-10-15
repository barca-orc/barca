"""#295: star imports in ordinary project helper modules retain source provenance."""

import json
import os
import subprocess

import pytest
from barca.api import _find_binary


@pytest.mark.parametrize(
    "helper,expression",
    [
        ("from values import *\ndef result(): return VALUE", "helpers.result()"),
        ("from values import *\nclass Holder: value = VALUE", "helpers.Holder.value"),
        ("from values import *\nCAPTURED = VALUE", "helpers.CAPTURED"),
        ("from values import *", "helpers.VALUE"),
        ("from values import *\nfrom other import *", "helpers.VALUE"),
        ("if True:\n    from values import *\ndef result(): return VALUE", "helpers.result()"),
        ("from middle import *\ndef result(): return VALUE", "helpers.result()"),
        ("from pkg.exports import *\ndef result(): return VALUE", "helpers.result()"),
        ("from cyclic import *\ndef result(): return VALUE", "helpers.result()"),
    ],
)
def test_edit_to_star_imported_helper_invalidates_its_consumer(tmp_path, helper, expression):
    (tmp_path / "helpers.py").write_text(helper + "\n")
    (tmp_path / "other.py").write_text("OTHER = 7\n")
    (tmp_path / "middle.py").write_text("from values import *\n")
    (tmp_path / "cyclic.py").write_text("from helpers import *\nfrom values import *\n")
    package = tmp_path / "pkg"
    package.mkdir()
    (package / "__init__.py").write_text("")
    (package / "exports.py").write_text("from .data import *\n")
    (package / "data.py").write_text("from values import *\n")
    (tmp_path / "pipeline.py").write_text(
        "import helpers\nfrom barca import asset\n\n"
        + f"@asset()\ndef value(): return {expression}\n\n@asset()\ndef unrelated(): return 7\n"
    )
    values = tmp_path / "values.py"
    env = {k: v for k, v in os.environ.items() if not k.startswith("BARCA_")}
    env["PYTHONDONTWRITEBYTECODE"] = "1"

    def run():
        proc = subprocess.run(
            [_find_binary(), "get", "value,unrelated", "pipeline.py", "--json"],
            cwd=tmp_path,
            env=env,
            capture_output=True,
            text=True,
            timeout=30,
        )
        assert proc.returncode == 0, proc.stderr
        return json.loads(proc.stdout)

    values.write_text("VALUE = 1\n")
    cold = run()
    assert cold["steps_executed"] == 2
    assert cold["targets"]["value"]["final_output"] == 1
    assert run()["steps_executed"] == 0
    values.write_text("VALUE = 222\n")
    changed = run()
    assert changed["steps_executed"] == 1, "helper star import edit served stale result"
    assert changed["targets"]["value"]["final_output"] == 222
    assert changed["targets"]["unrelated"]["final_output"] == 7
