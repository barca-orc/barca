#!/usr/bin/env python3
"""The Python test files a diff needs, one per line: CI's merge tier and scripts/check-fast.sh.

Reads the changed paths (relative to the repository root) on stdin. Selects:

- the quick subset in scripts/fast-tests.txt, always;
- the test files that changed;
- the test files that name a changed python/barca module (python/barca/_storage.py ->
  the word `_storage`) or a changed helper module beside the tests (python/tests/served.py);
- test_cli_contract.py and test_docs_examples.py when the CLI surface or the manual changed;
- every test file when conftest.py, python/tests/__init__.py or test data (fixtures/,
  hold/) changed: anything may use it.

With `--root DIR`, paths are resolved under DIR (default: the repository root, this script's
parent's parent).
"""

import argparse
import re
import sys
from pathlib import Path

CLI_SURFACE = re.compile(
    r"^crates/barca-cli/(src|docs|snapshots)/"
    r"|^python/tests/snapshots/"
    r"|^crates/barca-compile/src/decorator_args\.rs$"
)
CLI_TESTS = ("test_cli_contract.py", "test_docs_examples.py")
SKIP_MODULES = {"", "__init__", "__main__"}


def fast_subset(root):
    lines = (root / "scripts/fast-tests.txt").read_text().splitlines()
    return [line.strip() for line in lines if line.strip() and not line.startswith("#")]


def select(root, changed):
    tests = root / "python/tests"
    all_tests = sorted(p.name for p in tests.glob("test_*.py"))
    picked = set(fast_subset(root))
    # Modules whose name selects the tests that mention it: python/barca's, and the shared
    # helpers beside the tests (python/tests/served.py -> `served`).
    modules = set()
    for path in changed:
        if m := re.fullmatch(r"python/barca/([^/]*)\.py", path):
            modules.add(m.group(1))
        if not path.startswith("python/tests/"):
            continue
        rest = path[len("python/tests/") :]
        if re.fullmatch(r"test_[^/]*\.py", rest):
            picked.add(rest)
        elif re.fullmatch(r"[^/]*\.py", rest) and rest not in ("conftest.py", "__init__.py"):
            modules.add(rest[: -len(".py")])
        elif not rest.startswith("snapshots/"):
            # conftest.py, fixtures/, hold/: anything may use it.
            return [f"python/tests/{name}" for name in all_tests]
    modules -= SKIP_MODULES
    if modules:
        words = re.compile(r"\b(" + "|".join(map(re.escape, sorted(modules))) + r")\b")
        for name in all_tests:
            if words.search((tests / name).read_text(errors="replace")):
                picked.add(name)
    if any(CLI_SURFACE.search(path) for path in changed):
        picked.update(CLI_TESTS)
    return [f"python/tests/{name}" for name in sorted(picked) if (tests / name).is_file()]


def main():
    parser = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    parser.add_argument("--root", type=Path, default=Path(__file__).resolve().parent.parent)
    args = parser.parse_args()
    changed = [line.strip() for line in sys.stdin if line.strip()]
    for path in select(args.root, changed):
        print(path)


if __name__ == "__main__":
    main()
