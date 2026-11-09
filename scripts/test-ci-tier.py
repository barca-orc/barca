#!/usr/bin/env python3
"""Release merge groups must retain the full release gates without payload commit text."""

from pathlib import Path
import runpy
import unittest

select = runpy.run_path(str(Path(__file__).with_name("ci-tier.py")))["tier"]


class TierTests(unittest.TestCase):
    def test_ui_scope(self):
        for event in ("pull_request", "merge_group"):
            for files in (["ui/src/App.tsx"], ["ui/pnpm-lock.yaml", "CHANGELOG.md"]):
                self.assertEqual(select(event, "feat/ui", [], "ui", files), "ui")
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

    def test_unknown_event_fails_closed(self):
        with self.assertRaises(ValueError):
            select("push", "", [], "")


if __name__ == "__main__":
    unittest.main()
