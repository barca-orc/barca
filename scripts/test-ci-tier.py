#!/usr/bin/env python3
"""The CI tier and its optional parts (scripts/ci-tier.py). Release merge groups must retain
the full release gates without payload commit text; a pull request builds nothing (#945)."""

from pathlib import Path
import runpy
import unittest

module = runpy.run_path(str(Path(__file__).with_name("ci-tier.py")))
select = module["tier"]
parts = module["parts"]
rust_input = module["rust_input"]
ROOT = Path(__file__).resolve().parent.parent
pick = runpy.run_path(str(Path(__file__).with_name("select-python-tests.py")))["select"]


class TierTests(unittest.TestCase):
    def test_ui_scope(self):
        for event in ("pull_request", "merge_group"):
            for files in (["ui/src/App.tsx"], ["ui/pnpm-lock.yaml", "CHANGELOG.md"]):
                self.assertEqual(
                    select(event, "feat/ui", [], "ui", files),
                    "ui" if event == "pull_request" else "ui-merge",
                )
            for files in (
                None,
                [],
                ["CHANGELOG.md"],
                ["ui/a", "python/barca/a.py"],
                ["ui/a", "crates/a.rs"],
                ["ui/a", ".depot/workflows/ci.yml"],
            ):
                self.assertEqual(
                    select(event, "feat/ui", [], "ui", files),
                    "fast" if event == "pull_request" else "merge",
                )
        self.assertEqual(select("pull_request", "release/v1", [], "", ["ui/a"]), "full")
        self.assertEqual(select("pull_request", "feat/ui", ["ci-full"], "", ["ui/a"]), "full")
        self.assertEqual(select("merge_group", "", [], "chore(release)", ["ui/a"]), "full")

    def test_site_scope(self):
        for files in (
            ["site/src/content/docs/guide.md"],
            ["site/astro.config.mjs", "CHANGELOG.md"],
            ["site/wrangler.jsonc", ".github/workflows/site.yml"],
        ):
            self.assertEqual(select("pull_request", "doc/x", [], "", files), "site")
            self.assertEqual(select("merge_group", "", [], "doc: x", files), "site-merge")
        for files in (
            ["CHANGELOG.md"],
            [".github/workflows/site.yml"],
            ["site/a.md", "crates/barca-cli/docs/cache.md"],
            ["site/a.md", "python/barca/a.py"],
            ["site/a.md", ".depot/workflows/ci.yml"],
            ["site/a.md", "ui/src/App.tsx"],
            ["site/src/content/docs/recipes/ordering-only-deps.md"],
        ):
            self.assertEqual(select("pull_request", "doc/x", [], "", files), "fast")
            self.assertEqual(select("merge_group", "", [], "doc: x", files), "merge")
        self.assertEqual(select("pull_request", "release/v1", [], "", ["site/a"]), "full")
        self.assertEqual(select("pull_request", "doc/x", ["ci-full"], "", ["site/a"]), "full")
        self.assertEqual(select("merge_group", "", [], "chore(release)", ["site/a"]), "full")

    def test_prs(self):
        self.assertEqual(select("pull_request", "release/v0.36.0", [], ""), "full")
        self.assertEqual(select("pull_request", "fix/copy", ["ci-full"], ""), "full")
        self.assertEqual(select("pull_request", "fix/copy", [], ""), "fast")

    def test_actual_release_queue_message(self):
        self.assertEqual(
            select("merge_group", "", [], "chore(release): v0.36.0 (#735)\n\nRelease notes"),
            "full",
        )

    def test_regular_queue(self):
        self.assertEqual(select("merge_group", "", [], "fix: preserve cached rows (#733)"), "merge")

    def test_rust_inputs_fail_closed(self):
        for path in (
            "crates/barca-core/src/cache.rs",
            "Cargo.lock",
            "python/barca/_worker.py",
            "README.md",
            "SKILL.md",
            "site/src/content/docs/guide.md",
            "specs/interfaces/server-api.openapi.yaml",
            "ui/src/App.tsx",
            "tests/integration/test_cli.sh",
            "benchmarks/trivial/bench.sh",
            "examples/basic/pipeline.py",
            ".depot/workflows/ci.yml",
            "pyproject.toml",
            "a/new/top-level/thing.txt",
        ):
            self.assertTrue(rust_input(path), path)
        for path in (
            "python/tests/test_cache_limit.py",
            "python/tests/conftest.py",
            "docs/engineering/plan.md",
            "specs/behavior/current-state-and-execution.md",
            "specs/models/AssetOwnership.tla",
            "scripts/check-fast.sh",
            ".github/workflows/release.yml",
            "CLAUDE.md",
            "AGENTS.md",
            "CHANGELOG.md",
            "benchmarks/perf/profile.py",
        ):
            self.assertFalse(rust_input(path), path)

    def test_parts(self):
        # A Python-test-only merge group skips the Rust jobs and the site build.
        self.assertEqual(
            parts("merge_group", "merge", ["python/tests/test_cache_limit.py", "CHANGELOG.md"]),
            {"rust": False, "site": True, "models": False},
        )
        self.assertEqual(
            parts("merge_group", "merge", ["python/tests/test_cache_limit.py"]),
            {"rust": False, "site": False, "models": False},
        )
        self.assertEqual(
            parts("merge_group", "merge", ["crates/barca-core/src/cache.rs"]),
            {"rust": True, "site": False, "models": False},
        )
        # A pull request never builds the site, even when it changes it.
        self.assertFalse(parts("pull_request", "fast", ["site/a.md", "CHANGELOG.md"])["site"])
        self.assertTrue(parts("merge_group", "site-merge", ["site/a.md"])["site"])
        # The models run on a pull request too: they are a static check.
        self.assertTrue(parts("pull_request", "fast", ["specs/models/model.cfg"])["models"])
        # Unknown history, CI itself and the full tier run every part (the site only where a
        # build is allowed).
        for event, selected, files in (
            ("merge_group", "merge", None),
            ("merge_group", "merge", [".depot/workflows/ci.yml"]),
            ("merge_group", "merge", ["scripts/ci-tier.py"]),
            ("pull_request", "full", ["python/tests/test_cache_limit.py"]),
            ("merge_group", "full", ["python/tests/test_cache_limit.py"]),
        ):
            self.assertEqual(
                parts(event, selected, files), {"rust": True, "site": True, "models": True}
            )
        self.assertEqual(
            parts("pull_request", "fast", None), {"rust": True, "site": False, "models": True}
        )

    def test_unknown_event_fails_closed(self):
        with self.assertRaises(ValueError):
            select("push", "", [], "")


class PythonSelectionTests(unittest.TestCase):
    """scripts/select-python-tests.py, which the merge tier and check-fast.sh share."""

    fast = {
        f"python/tests/{line.strip()}"
        for line in (ROOT / "scripts/fast-tests.txt").read_text().splitlines()
        if line.strip() and not line.startswith("#")
    }
    every = {str(p.relative_to(ROOT)) for p in (ROOT / "python/tests").glob("test_*.py")}

    def test_the_fast_subset_always_runs(self):
        self.assertEqual(set(pick(ROOT, [])), self.fast)
        self.assertEqual(set(pick(ROOT, ["crates/barca-core/src/cache.rs"])), self.fast)

    def test_changed_tests_and_modules(self):
        got = set(pick(ROOT, ["python/tests/test_cache_limit.py", "python/barca/_storage.py"]))
        self.assertIn("python/tests/test_cache_limit.py", got)
        self.assertTrue(
            any("_storage" in (ROOT / f).read_text() for f in got - self.fast),
            "a test that names _storage",
        )
        # A helper beside the tests selects the tests that use it, not the whole suite.
        got = set(pick(ROOT, ["python/tests/served.py"]))
        self.assertLess(len(got), len(self.every))
        self.assertTrue(all(f in self.fast or "served" in (ROOT / f).read_text() for f in got))

    def test_cli_surface(self):
        got = set(pick(ROOT, ["crates/barca-cli/docs/cache.md"]))
        self.assertIn("python/tests/test_cli_contract.py", got)
        self.assertIn("python/tests/test_docs_examples.py", got)

    def test_shared_infrastructure_runs_everything(self):
        for path in ("python/tests/conftest.py", "python/tests/hold/sitecustomize.py"):
            self.assertEqual(set(pick(ROOT, [path])), self.every, path)
        # A snapshot is read by the contract test, not by everything.
        got = set(pick(ROOT, ["python/tests/snapshots/cli_contract/get.json"]))
        self.assertIn("python/tests/test_cli_contract.py", got)
        self.assertLess(len(got), len(self.every))


if __name__ == "__main__":
    unittest.main()
