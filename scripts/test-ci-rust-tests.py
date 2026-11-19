#!/usr/bin/env python3
"""The Rust test selection: its rules, and that its tables still match the test sources."""

from pathlib import Path
import fnmatch
import os
import re
import runpy
import shutil
import subprocess
import unittest

REPO = Path(__file__).resolve().parents[1]
mod = runpy.run_path(str(Path(__file__).with_name("ci-rust-tests.py")))
select, outputs, explain = mod["select"], mod["outputs"], mod["explain"]
READERS, WORKSPACE_READERS = mod["READERS"], mod["WORKSPACE_READERS"]


def package(name, deps=(), dev=(), lib=True, ts=False, dir=None):
    """One `cargo metadata` package of a workspace at /w, as far as the selection reads it."""
    return {
        "name": name,
        "manifest_path": f"/w/crates/{dir or name}/Cargo.toml",
        "dependencies": [{"name": d, "kind": None, "path": f"/w/crates/{d}"} for d in deps]
        + [{"name": d, "kind": "dev", "path": f"/w/crates/{d}"} for d in dev]
        + [{"name": "serde", "kind": None}],
        "targets": [{"kind": ["lib" if lib else "bin"]}],
        "features": {"ts": []} if ts else {},
    }


# The crate graph of v0.45.0, written by hand so the rules are tested without cargo: the
# selection itself always reads the real one from `cargo metadata`.
CRATES = mod["parse_metadata"](
    {
        "workspace_root": "/w",
        "packages": [
            package("barca-model", ts=True),
            package("barca-compile", ["barca-model"], ts=True),
            package("barca-db", ["barca-model"], ts=True),
            package("barca-engine", ["barca-model", "barca-compile"]),
            package(
                "barca-core",
                ["barca-model", "barca-compile", "barca-db", "barca-engine"],
                ts=True,
            ),
            package("barca-server", ["barca-core"], ts=True),
            # The package is `barca`; its directory is crates/barca-cli.
            package("barca", ["barca-core", "barca-server"], lib=False, dir="barca-cli"),
        ],
    }
)
ALL = sorted(CRATES)


def run(*files, crates=CRATES):
    sel = select(list(files), crates)
    return sel, outputs(sel, crates)


class Rules(unittest.TestCase):
    def test_a_crate_file_selects_the_crate_and_its_dependents(self):
        sel, out = run("crates/barca-model/src/config.rs")
        self.assertEqual(sorted(sel["whole"]), ALL)
        self.assertEqual(out["rust_tests"], "some")
        self.assertEqual(out["rust_filter"], " | ".join(f"package(={c})" for c in ALL))
        self.assertEqual(out["rust_packages"], " ".join(f"-p {c}" for c in ALL))

        sel, _ = run("crates/barca-db/src/db/runs.rs")
        self.assertEqual(sorted(sel["whole"]), ["barca", "barca-core", "barca-db", "barca-server"])
        self.assertEqual(sel["whole"]["barca-db"], ["owns crates/barca-db/src/db/runs.rs"])
        self.assertEqual(
            sel["whole"]["barca"], ["depends on barca-db (crates/barca-db/src/db/runs.rs)"]
        )

        sel, _ = run("crates/barca-engine/src/wire.rs")
        self.assertEqual(
            sorted(sel["whole"]), ["barca", "barca-core", "barca-engine", "barca-server"]
        )

    def test_a_leaf_crate_runs_the_guards_that_scan_it_and_nothing_else_below(self):
        sel, out = run("crates/barca-server/src/handlers.rs")
        self.assertEqual(sorted(sel["whole"]), ["barca", "barca-server"])
        self.assertEqual(
            sorted(sel["readers"]["barca-core"]),
            [
                ("test", "metadata_writes_go_through_an_owner"),
                ("test", "no_bare_step_status_literal_outside_the_vocabulary"),
            ],
        )
        self.assertEqual(
            sorted(sel["readers"]["barca-engine"]),
            [("test", "every_child_process_is_started_through_this_module")],
        )
        self.assertEqual(
            out["rust_packages"], "-p barca -p barca-core -p barca-engine -p barca-server"
        )
        self.assertEqual(
            out["rust_filter"],
            "package(=barca) | package(=barca-server)"
            " | (package(=barca-core) & (test(~metadata_writes_go_through_an_owner)"
            " | test(~no_bare_step_status_literal_outside_the_vocabulary)))"
            " | (package(=barca-engine)"
            " & (test(~every_child_process_is_started_through_this_module)))",
        )
        # Doc tests: library crates selected whole. `barca` has no library.
        self.assertEqual(out["rust_doc"], "-p barca-server")
        for crate in ("barca-model", "barca-compile", "barca-db"):
            self.assertNotIn(f"-p {crate}", out["rust_packages"])

    def test_a_dev_dependency_counts(self):
        crates = mod["parse_metadata"](
            {
                "workspace_root": "/w",
                "packages": [package("low"), package("high", dev=["low"]), package("other")],
            }
        )
        sel, _ = run("crates/low/src/lib.rs", crates=crates)
        self.assertEqual(sorted(sel["whole"]), ["high", "low"])

    def test_a_new_crate_is_selected_from_the_manifests(self):
        # barca-analysis split out of barca-core (#1056 C6): nothing in the script names it.
        packages = [
            package("barca-model"),
            package("barca-analysis", ["barca-model"]),
            package("barca-core", ["barca-model", "barca-analysis"]),
            package("barca-engine", ["barca-model"]),
        ]
        crates = mod["parse_metadata"]({"workspace_root": "/w", "packages": packages})
        sel, _ = run("crates/barca-analysis/src/health.rs", crates=crates)
        self.assertEqual(sorted(sel["whole"]), ["barca-analysis", "barca-core"])
        # barca-engine: only its guard that scans every crate's sources.
        self.assertEqual(list(sel["readers"]), ["barca-engine"])

    def test_workspace_wide_files(self):
        for path in (
            "Cargo.toml",
            "Cargo.lock",
            "rust-toolchain.toml",
            ".cargo/config.toml",
            ".depot/workflows/ci.yml",
            ".depot/actions/rust-cache-save/action.yml",
            "scripts/ci-rust-tests.py",
            "scripts/test-ci-rust-tests.py",
            "scripts/ci-tier.py",
            "python/barca/_worker.py",
            "pyproject.toml",
        ):
            sel, out = run(path, "docs/engineering/plan.md")
            self.assertIn(path, sel["all"], path)
            self.assertEqual(
                out,
                {
                    "rust_tests": "all",
                    "rust_packages": "--workspace",
                    "rust_filter": "",
                    "rust_doc": "--workspace",
                    "rust_types": "true",
                },
            )

    def test_what_nothing_classifies_selects_the_workspace(self):
        for path in (
            "newdir/file.txt",
            "Makefile",
            ".github/workflows/new.yml",
            # Not inside any crate: a deleted crate's file, a file beside the crates.
            "crates/barca-gone/src/lib.rs",
            "crates/README.md",
        ):
            sel, out = run(path)
            self.assertIn("no rule", sel["all"], path)
            self.assertEqual(out["rust_tests"], "all")

    def test_files_no_rust_test_reads(self):
        for path in (
            "CHANGELOG.md",
            "docs/engineering/plan.md",
            "ui/src/App.tsx",
            "ui/pnpm-lock.yaml",
            "specs/behavior/health.md",
            "scripts/perf-gate.py",
            "python/tests/snapshots/cli_contract/status.json",
            "uv.lock",
            ".github/workflows/site.yml",
        ):
            sel, out = run(path)
            self.assertEqual(
                out,
                {
                    "rust_tests": "none",
                    "rust_packages": "",
                    "rust_filter": "",
                    "rust_doc": "",
                    "rust_types": "false",
                },
                path,
            )
        self.assertEqual(run()[1]["rust_tests"], "none")

    def test_files_outside_a_crate_run_the_tests_that_read_them(self):
        def chosen(path):
            sel, _ = run(path)
            self.assertIsNone(sel["all"], path)
            return sorted(sel["whole"]), {c: sorted(r) for c, r in sel["readers"].items()}

        sweep = ("binary", "unused_input_repo_sweep")
        self.assertEqual(
            chosen("site/src/content/docs/guide.md"), (["barca"], {"barca-core": [sweep]})
        )
        # Outside src/content the site is read by the contract tests only (the sidebar).
        self.assertEqual(chosen("site/astro.config.mjs"), (["barca"], {}))
        self.assertEqual(
            chosen("site/src/content/docs/reference/api/decorators.md"),
            (
                ["barca"],
                {
                    "barca-compile": [
                        ("test", "the_manual_and_the_site_list_exactly_these_arguments")
                    ],
                    "barca-core": [sweep],
                },
            ),
        )
        self.assertEqual(
            chosen("specs/interfaces/server-api.openapi.yaml"),
            ([], {"barca-server": [("binary", "auth"), ("binary", "openapi")]}),
        )
        self.assertEqual(
            chosen("specs/interfaces/artifact-formats.md"),
            ([], {"barca-core": [("test", "format_of_agrees_with_the_spec_table")]}),
        )
        self.assertEqual(
            chosen("ui/src/lib/testdata/weights.json"),
            (
                [],
                {
                    "barca-core": [
                        ("test", "the_ui_fixture_health_table_is_what_classify_says"),
                        ("test", "the_ui_fixture_weights_are_the_rollups"),
                    ]
                },
            ),
        )
        self.assertEqual(
            chosen("ui/demo/modeling.py"),
            (
                [],
                {
                    "barca-compile": [
                        ("test", "group_names_matching_local_variables_do_not_change_hashes")
                    ]
                },
            ),
        )
        for path in (
            "examples/basic/pipeline.py",
            "benchmarks/trivial/bench.sh",
            "python/tests/test_storage.py",
            "tests/integration/test_cli.sh",
        ):
            self.assertEqual(chosen(path), ([], {"barca-core": [sweep]}), path)
        self.assertEqual(chosen("README.md"), (["barca"], {"barca-core": [sweep]}))
        self.assertEqual(chosen("SKILL.md"), (["barca"], {"barca-core": [sweep]}))

    def test_a_crate_manifest_runs_the_layering_test(self):
        sel, _ = run("crates/barca-server/Cargo.toml")
        self.assertEqual(sorted(sel["whole"]), ["barca", "barca-server"])
        self.assertEqual(
            sorted(sel["readers"]["barca-core"]),
            [("test", "crates_depend_only_on_crates_below_them")],
        )

    def test_a_manual_topic_runs_the_tests_that_read_it_in_lower_crates(self):
        sel, out = run("crates/barca-cli/docs/hashing.md")
        self.assertEqual(sorted(sel["whole"]), ["barca"])
        self.assertEqual(
            {c: sorted(r) for c, r in sel["readers"].items()},
            {
                "barca-compile": [("test", "the_manual_lists_exactly_these_rules")],
                "barca-core": [("binary", "unused_input_repo_sweep")],
            },
        )
        self.assertEqual(out["rust_doc"], "")

    def test_a_whole_crate_drops_its_readers(self):
        sel, out = run("site/src/content/docs/guide.md", "crates/barca-core/src/health.rs")
        self.assertNotIn("barca-core", sel["readers"])
        self.assertIn("package(=barca-core)", out["rust_filter"])
        self.assertNotIn("unused_input_repo_sweep", out["rust_filter"])

    def test_generated_types(self):
        self.assertEqual(run("ui/src/lib/generated/Health.ts")[1]["rust_types"], "true")
        self.assertEqual(run("ui/src/lib/generated/Health.ts")[1]["rust_tests"], "none")
        self.assertEqual(run("ui/scripts/regen-types.sh")[1]["rust_types"], "true")
        self.assertEqual(run("crates/barca-server/src/routes.rs")[1]["rust_types"], "true")
        # `barca` (the CLI) has no `ts` feature and nothing with one depends on it.
        self.assertEqual(run("crates/barca-cli/src/main.rs")[1]["rust_types"], "false")
        self.assertEqual(run("ui/src/App.tsx")[1]["rust_types"], "false")

    def test_the_log_names_every_crate(self):
        sel, _ = run("crates/barca-server/src/handlers.rs")
        text = "\n".join(explain(sel, CRATES, 1))
        self.assertIn("barca-server: every test: owns crates/barca-server/src/handlers.rs", text)
        self.assertIn("barca: every test: depends on barca-server", text)
        self.assertIn("barca-engine: only test every_child_process_is_started_through", text)
        self.assertIn("barca-model: not run: no changed file can affect its tests", text)
        sel, _ = run("Cargo.lock")
        self.assertIn("the whole workspace: Cargo.lock", "\n".join(explain(sel, CRATES, 1)))


def tracked():
    out = subprocess.run(
        ["git", "-C", str(REPO), "ls-files", "-z"], check=True, capture_output=True, text=True
    ).stdout
    return [f for f in out.split("\0") if f]


INCLUDE = re.compile(r'include_(?:str|bytes)!\(\s*"([^"]+)"')
EMBED = re.compile(r'folder\s*=\s*"([^"]+)"')
PARENT = re.compile(r'"\.\.[/"]')


def leaves_its_crate(path):
    """Does this Rust source read a file outside its crate at compile time or by path?"""
    text = (REPO / path).read_text(errors="replace")
    crate = "/".join(path.split("/")[:2])
    for pattern, base in ((INCLUDE, os.path.dirname(path)), (EMBED, crate)):
        for target in pattern.findall(text):
            if not os.path.normpath(os.path.join(base, target)).startswith(crate + "/"):
                return True
    return "CARGO_MANIFEST_DIR" in text and bool(PARENT.search(text))


class Tables(unittest.TestCase):
    """READERS against the repository: a stale or missing entry fails here, in CI's `changes`."""

    def test_every_source_that_leaves_its_crate_is_listed(self):
        found = {
            f
            for f in tracked()
            if f.startswith("crates/") and f.endswith(".rs") and leaves_its_crate(f)
        }
        listed = {r["source"] for r in READERS} | set(WORKSPACE_READERS)
        self.assertEqual(
            sorted(found - listed),
            [],
            "these sources read files outside their crate: add each to READERS in "
            "scripts/ci-rust-tests.py with the paths it reads (or to WORKSPACE_READERS when a "
            "WORKSPACE rule covers them), or a change to those paths skips the test",
        )
        self.assertEqual(sorted(listed - found), [], "listed, but no longer leaves its crate")

    def test_every_reader_names_tests_that_exist(self):
        for reader in READERS:
            source = REPO / reader["source"]
            self.assertTrue(source.is_file(), reader["source"])
            text = source.read_text()
            for name in reader.get("tests", []):
                self.assertRegex(text, rf"\bfn {re.escape(name)}", f"{reader['source']}: {name}")
            if "binary" in reader:
                self.assertEqual(
                    reader["source"].split("/")[2:],
                    ["tests", reader["binary"] + ".rs"],
                    "an integration test binary is named after its file",
                )

    def test_every_reader_path_matches_a_tracked_file(self):
        files = tracked()
        for reader in READERS:
            for pattern in reader["paths"]:
                self.assertTrue(
                    any(fnmatch.fnmatchcase(f, pattern) for f in files),
                    f"{reader['source']}: nothing matches {pattern}",
                )

    @unittest.skipUnless(shutil.which("cargo"), "needs cargo")
    def test_the_real_workspace(self):
        crates = mod["workspace"]()
        # Every reader's source is in a crate, and every tracked file is known to a rule: the
        # whole-workspace fallback is for what does not exist yet.
        for reader in READERS:
            self.assertIsNotNone(mod["owner"](reader["source"], crates), reader["source"])
        unknown = [f for f in tracked() if "no rule" in (select([f], crates)["all"] or "")]
        self.assertEqual(unknown, [], "add these to a list in scripts/ci-rust-tests.py")
        # A crate's dependents include itself, and the graph has no crate outside crates/.
        for name, crate in crates.items():
            self.assertIn(name, mod["dependents"](name, crates))
            self.assertTrue(crate["dir"].startswith("crates/"), crate["dir"])
        # The bottom of the graph reaches everything; the binary only itself.
        self.assertEqual(mod["dependents"]("barca-model", crates), set(crates))
        self.assertEqual(mod["dependents"]("barca", crates), {"barca"})


if __name__ == "__main__":
    unittest.main()
