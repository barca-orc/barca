#!/usr/bin/env python3
"""Integration controls for specification link parsing using a disposable repository."""

import contextlib
import hashlib
import importlib.util
import io
import json
import tempfile
import unittest
from pathlib import Path

CHECKER = Path(__file__).with_name("check-specs.py")


class NavigationControls(unittest.TestCase):
    def setUp(self):
        temporary = tempfile.TemporaryDirectory(prefix="barca-spec-navigation-")
        self.addCleanup(temporary.cleanup)
        root = Path(temporary.name).resolve()
        spec = importlib.util.spec_from_file_location("spec_navigation_control", CHECKER)
        self.checker = importlib.util.module_from_spec(spec)
        spec.loader.exec_module(self.checker)
        self.checker.ROOT = root
        self.checker.SPECS = root / "specs"
        self.checker.ENGINEERING = root / "docs/engineering"
        specs = self.checker.SPECS
        (specs / "behavior").mkdir(parents=True)
        (specs / "models").mkdir()
        (root / "GLOSSARY.md").write_text("# Glossary\n")
        sections = []
        for heading in self.checker.BEHAVIOR_SECTIONS:
            body = "Fixture behavior."
            if heading == "Scope and status":
                body = "Specification version: 1. Status: approved fixture. Scope: link controls."
            elif heading == "Invariants":
                body = "- **INV-FIXTURE-01:** The fixture has one invariant owner."
            sections.append(f"## {heading}\n\n{body}\n")
        (specs / "behavior/cache.md").write_text("# Fixture\n\n" + "\n".join(sections))
        model = b"fixture model, not executed by these navigation tests\n"
        (specs / "models/AssetOwnership.tla").write_bytes(model)
        (specs / "models/verification.json").write_text(
            json.dumps({"model_sha256": hashlib.sha256(model).hexdigest(), "configs_sha256": {}})
        )
        self.index = specs / "README.md"
        self.index.write_text(
            "# Fixture index\n\n[Cache\ndecisions](behavior/cache.md)\n"
            "[Model](models/AssetOwnership.tla)\n[Evidence](models/verification.json)\n"
        )

    def run_checker(self):
        output = io.StringIO()
        with contextlib.redirect_stdout(output), contextlib.redirect_stderr(output):
            status = self.checker.main()
        return status, output.getvalue()

    def test_multiline_label_reaches_its_owner(self):
        status, output = self.run_checker()
        self.assertEqual(status, 0, output)

    def test_missing_multiline_target_is_rejected(self):
        self.index.write_text(self.index.read_text() + "\n[Broken\ncache](missing.md)\n")
        status, output = self.run_checker()
        self.assertEqual(status, 1, output)
        self.assertIn("missing local link missing.md", output)

    def test_missing_single_line_target_is_still_rejected(self):
        self.index.write_text(self.index.read_text() + "\n[Broken](missing.md)\n")
        status, output = self.run_checker()
        self.assertEqual(status, 1, output)
        self.assertIn("missing local link missing.md", output)


if __name__ == "__main__":
    unittest.main()
