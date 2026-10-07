"""Helper edits the cone analysis used to miss now invalidate the step (#194).

Every test runs the real binary and a real worker. A step returns what its helper returns, so
`final_output` says which file Python actually imported; the test then checks that editing that
file re-runs the step, and that editing a file Python does not import (an unused definition, a
same-named module it shadows) leaves it cached. The static analysis and the worker's importer
must agree, or the hash tracks code that never runs.
"""

import json
import os
import subprocess
import textwrap
from pathlib import Path

import pytest

from barca.api import _find_binary


def barca(cwd: Path, *args: str, env: dict | None = None) -> dict:
    proc = subprocess.run(
        [_find_binary(), *args],
        cwd=cwd,
        env={**os.environ, "PYTHONDONTWRITEBYTECODE": "1", **(env or {})},
        capture_output=True,
        text=True,
    )
    assert proc.returncode == 0, proc.stderr
    return json.loads(proc.stdout.strip().splitlines()[-1])


def write(root: Path, files: dict) -> None:
    for rel, code in files.items():
        path = root / rel
        path.parent.mkdir(parents=True, exist_ok=True)
        path.write_text(textwrap.dedent(code).lstrip("\n"))


def helper(value, unused=0) -> str:
    """A helper module: `compute` is what steps use, `unused` is never used."""
    return f"def compute():\n    return {value!r}\n\n\ndef unused():\n    return {unused!r}\n"


def pipeline(imports: str, body: str) -> str:
    return f"from barca import asset\n{imports}\n\n\n@asset()\ndef val():\n    {body}\n"


class Project:
    def __init__(self, root: Path, files: dict, target: str = "p.py", env: dict | None = None):
        self.root, self.target, self.env = root, target, env
        write(root, {"barca.toml": "", **files})

    def get(self) -> dict:
        return barca(self.root, "get", "val", self.target, env=self.env)

    def assert_cached(self, why: str) -> None:
        assert self.get()["steps_executed"] == 0, why

    def assert_reruns(self, expected, why: str) -> None:
        result = self.get()
        assert result["steps_executed"] == 1, why
        assert result["final_output"] == expected

    def check(self, loaded: str, first, ignored: tuple[str, ...] = ()) -> None:
        """Cold run returns `first` (so `loaded` is the file Python imports); a rerun is cached;
        editing `unused` in it, or anything in the `ignored` files, stays cached; editing
        `compute` in `loaded` re-runs with the new value."""
        self.assert_reruns(first, "cold run")
        self.assert_cached("nothing changed")
        (self.root / loaded).write_text(helper(first, unused=555))
        self.assert_cached(f"an unused definition in {loaded} changed")
        for rel in ignored:
            (self.root / rel).write_text(helper("edited"))
            self.assert_cached(f"{rel} is not what the step imports")
        (self.root / loaded).write_text(helper("new", unused=555))
        self.assert_reruns("new", f"compute() in {loaded} changed")


# ─── Classes ─────────────────────────────────────────────────────────────────

MODEL = """
from bases import Base


class Model(Base):
    def predict(self):
        return {predict}


class Other:
    def f(self):
        return {other}
"""
BASE = "class Base:\n    def fit(self):\n        return {fit}\n\n\nclass Unused:\n    x = {x}\n"
USES_MODEL = pipeline("from models import Model", "return [Model().predict(), Model().fit()]")


def test_class_body_edit_reruns(tmp_path):
    p = Project(
        tmp_path,
        {
            "p.py": USES_MODEL,
            "models.py": MODEL.format(predict=1, other=0),
            "bases.py": BASE.format(fit=1, x=0),
        },
    )
    p.assert_reruns([1, 1], "cold run")
    write(tmp_path, {"models.py": MODEL.format(predict=1, other=555)})
    p.assert_cached("a class the step never uses changed")
    write(tmp_path, {"models.py": MODEL.format(predict=22, other=555)})
    p.assert_reruns([22, 1], "a method of the class the step uses changed")


def test_base_class_in_another_module_edit_reruns(tmp_path):
    p = Project(
        tmp_path,
        {
            "p.py": USES_MODEL,
            "models.py": MODEL.format(predict=1, other=0),
            "bases.py": BASE.format(fit=1, x=0),
        },
    )
    p.assert_reruns([1, 1], "cold run")
    write(tmp_path, {"bases.py": BASE.format(fit=1, x=555)})
    p.assert_cached("a class nothing inherits from changed")
    write(tmp_path, {"bases.py": BASE.format(fit=33, x=555)})
    p.assert_reruns([1, 33], "the base class of the class the step uses changed")


def test_class_defined_in_the_pipeline_file_edit_reruns(tmp_path):
    code = "from barca import asset\n\n\nclass Model:\n    def predict(self):\n        return {}\n\n\n@asset()\ndef val():\n    return Model().predict()\n"
    p = Project(tmp_path, {"p.py": code.format(1)})
    p.assert_reruns(1, "cold run")
    p.assert_cached("nothing changed")
    write(tmp_path, {"p.py": code.format(22)})
    p.assert_reruns(22, "a method of a class in the pipeline file changed")


# ─── Imports inside the function body, and aliases ───────────────────────────


@pytest.mark.parametrize(
    "imports, body",
    [
        ("", "from helpers import compute\n    return compute()"),
        ("", "import helpers\n    return helpers.compute()"),
        ("", "from helpers import compute as c\n    return c()"),
        ("", "import helpers as h\n    return h.compute()"),
        ("", "if True:\n        from helpers import compute as c\n    return c()"),
        ("from helpers import compute as c", "return c()"),
        ("import helpers as h", "return h.compute()"),
    ],
    ids=[
        "from-import in body",
        "import in body",
        "aliased from-import in body",
        "aliased import in body",
        "aliased from-import in a nested block",
        "aliased from-import at module level",
        "aliased import at module level",
    ],
)
def test_in_function_and_aliased_imports_rerun(tmp_path, imports, body):
    p = Project(tmp_path, {"p.py": pipeline(imports, body), "helpers.py": helper("old")})
    p.check("helpers.py", "old")


# ─── A module used as a value ────────────────────────────────────────────────


@pytest.mark.parametrize(
    "imports, body",
    [
        ("import helpers", 'return getattr(helpers, "compute")()'),
        ("import helpers", "return (lambda m: m.compute())(helpers)"),
        ("", 'import helpers as h\n    return getattr(h, "compute")()'),
    ],
    ids=["getattr", "passed along", "imported in body"],
)
def test_module_used_as_value_any_edit_reruns(tmp_path, imports, body):
    p = Project(tmp_path, {"p.py": pipeline(imports, body), "helpers.py": helper("old")})
    p.assert_reruns("old", "cold run")
    p.assert_cached("nothing changed")
    # Which attribute is read cannot be known: the whole module counts (documented).
    write(tmp_path, {"helpers.py": helper("old", unused=555)})
    p.assert_reruns("old", "any edit to a module used as a value re-runs")
    write(tmp_path, {"helpers.py": helper("new", unused=555)})
    p.assert_reruns("new", "compute() changed")


def test_a_local_variable_named_like_a_module_is_not_the_module(tmp_path):
    code = """
        from barca import asset
        import helpers


        def describe(helpers):
            return [str(helpers) for helpers in [helpers]]


        @asset()
        def val():
            helpers = "local"
            return describe(helpers)
    """
    p = Project(tmp_path, {"p.py": code, "helpers.py": helper("old")})
    p.assert_reruns(["local"], "cold run")
    write(tmp_path, {"helpers.py": helper("new", unused=555)})
    p.assert_cached("the step never touches the module, only locals named like it")


# ─── Which file a name means, per layout ─────────────────────────────────────

USES_HELPERS = pipeline("from helpers import compute", "return compute()")


def test_flat_directory(tmp_path):
    p = Project(tmp_path, {"p.py": USES_HELPERS, "helpers.py": helper("beside")})
    p.check("helpers.py", "beside")


def test_subdirectory_pipeline_imports_a_root_module(tmp_path):
    files = {
        "pipelines/p.py": USES_HELPERS,
        "helpers.py": helper("root"),
        "elsewhere/helpers.py": helper("elsewhere"),
    }
    p = Project(tmp_path, files, target="pipelines/p.py")
    p.check("helpers.py", "root", ignored=("elsewhere/helpers.py",))


def test_subdirectory_pipeline_imports_a_root_package(tmp_path):
    files = {
        "pipelines/p.py": pipeline("from shared.utils import compute", "return compute()"),
        "shared/__init__.py": "",
        "shared/utils.py": helper("shared"),
    }
    p = Project(tmp_path, files, target="pipelines/p.py")
    p.check("shared/utils.py", "shared")


def test_a_module_beside_the_pipeline_shadows_the_root_module(tmp_path):
    files = {
        "pipelines/p.py": USES_HELPERS,
        "pipelines/helpers.py": helper("beside"),
        "helpers.py": helper("root"),
    }
    p = Project(tmp_path, files, target="pipelines/p.py")
    p.check("pipelines/helpers.py", "beside", ignored=("helpers.py",))


def test_package_pipeline_imports_a_bare_name_from_the_root(tmp_path):
    # `pkg/p.py` runs as the module `pkg.p` with the root on the import path, not `pkg/`:
    # `from helpers import ...` is `helpers.py` in the root, however close `pkg/helpers.py` is.
    files = {
        "pkg/__init__.py": "",
        "pkg/p.py": USES_HELPERS,
        "pkg/helpers.py": helper("beside"),
        "helpers.py": helper("root"),
    }
    p = Project(tmp_path, files, target="pkg/p.py")
    p.check("helpers.py", "root", ignored=("pkg/helpers.py",))


@pytest.mark.parametrize(
    "imports",
    ["from .helpers import compute", "from pkg.helpers import compute", "from . import helpers"],
    ids=["relative", "dotted", "from-dot"],
)
def test_package_pipeline_imports_its_sibling(tmp_path, imports):
    body = "return helpers.compute()" if imports.endswith("import helpers") else "return compute()"
    files = {
        "pkg/__init__.py": "",
        "pkg/p.py": pipeline(imports, body),
        "pkg/helpers.py": helper("beside"),
        "helpers.py": helper("root"),
    }
    p = Project(tmp_path, files, target="pkg/p.py")
    p.check("pkg/helpers.py", "beside", ignored=("helpers.py",))


def test_a_pipeline_file_wins_over_a_root_directory_without_init(tmp_path):
    # `a/p.py` imports `shared`, which is the pipeline file `b/shared.py`; the root also has a
    # directory `shared/` with no `__init__.py`. A worker that has loaded `b/shared.py` has `b/`
    # on its import path, and a regular module beats a namespace package wherever it is on the
    # path: the step runs `b/shared.py` (it returns its value), so that file must be hashed.
    shared = """
        from barca import asset


        def compute():
            return {value!r}


        @asset()
        def upstream() -> int:
            return 1
    """
    files = {
        "a/p.py": """
            from barca import asset
            from shared import compute, upstream


            @asset(inputs={"u": upstream})
            def val(u: int):
                return compute()
        """,
        "b/shared.py": shared.format(value="from b"),
        "shared/notes.txt": "a directory that is not a package\n",
    }
    write(tmp_path, {"barca.toml": "", **files})

    first = barca(tmp_path, "get", "val")
    assert first["steps_executed"] == 2 and first["final_output"] == "from b"
    assert barca(tmp_path, "get", "val")["steps_executed"] == 0

    write(tmp_path, {"b/shared.py": shared.format(value="edited")})
    plan = barca(tmp_path, "get", "val", "--dry-run", "--json")
    actions = {step["id"]: step["action"] for step in plan["steps"]}
    assert actions["a/p.py:val"] == "run", "compute() in b/shared.py changed"


# ─── What is never followed ──────────────────────────────────────────────────


def test_a_module_outside_the_root_is_not_followed(tmp_path):
    # `../shared/` is importable only because the pipeline puts it on sys.path itself. The
    # documented boundary: such a module is not hashed, so an edit there stays cached.
    outside = tmp_path / "shared"
    write(outside, {"farlib.py": helper("old")})
    code = """
        import sys
        from pathlib import Path

        sys.path.insert(0, str(Path(__file__).resolve().parent.parent / "shared"))

        from barca import asset
        from farlib import compute


        @asset()
        def val():
            return compute()
    """
    p = Project(tmp_path / "proj", {"p.py": code})
    p.assert_reruns("old", "cold run")
    write(outside, {"farlib.py": helper("new")})
    p.assert_cached("a module outside the project root is not part of the hash")


def test_installed_packages_are_never_hashed(tmp_path):
    # A package on PYTHONPATH stands in for site-packages: importable, not part of the project.
    site = tmp_path / "site"
    write(site, {"thirdparty/__init__.py": helper("old")})
    code = pipeline(
        "import thirdparty\nfrom thirdparty import compute as c",
        "return [c(), getattr(thirdparty, 'compute')()]",
    )
    p = Project(tmp_path / "proj", {"p.py": code}, env={"PYTHONPATH": str(site)})
    p.assert_reruns(["old", "old"], "cold run")
    write(site, {"thirdparty/__init__.py": helper("new")})
    p.assert_cached("an installed package is not part of the hash, even used as a value")


def test_a_virtualenv_inside_the_project_is_never_read(tmp_path):
    # Same-named modules inside directories that look like environments are not on the import
    # path: the root's `helpers.py` is what runs and what is hashed, and edits there change nothing.
    files = {"pipelines/p.py": USES_HELPERS, "helpers.py": helper("root")}
    p = Project(tmp_path, files, target="pipelines/p.py")
    for env in ("venv", ".venv", "env"):
        bad = tmp_path / env / "lib" / "python3.12" / "site-packages" / "helpers.py"
        bad.parent.mkdir(parents=True)
        bad.write_text(helper("site-packages"))
    p.check("helpers.py", "root")
    for env in ("venv", ".venv", "env"):
        bad = tmp_path / env / "lib" / "python3.12" / "site-packages" / "helpers.py"
        bad.write_text(helper("edited"))
    p.assert_cached("files under an environment directory are not project modules")
