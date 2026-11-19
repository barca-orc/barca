#!/usr/bin/env python3
"""Select the Rust tests a change can affect.

CI's merge tier and `scripts/check-fast.sh` run the tests of the workspace crates a diff can
affect instead of the whole workspace. The full tier (a release, `ci-full`) passes `--all`.

    python3 scripts/ci-rust-tests.py --base <sha>             # diff <sha>...HEAD (or --head)
    git diff --name-only <sha> | python3 scripts/ci-rust-tests.py --files-from -
    python3 scripts/ci-rust-tests.py --all                    # the whole workspace

A changed file selects:

1. **The whole workspace**, when it matches `WORKSPACE` below (the root manifest, the lock
   file, the toolchain, cargo's configuration, the CI workflow and its actions, this script,
   the Python package every crate's tests run), and when no rule below knows the file.
2. **The crate that owns it** (the workspace package whose directory holds the file) **and
   every crate that depends on that crate**, transitively, through normal, dev and build
   dependencies. Crates and dependencies come from `cargo metadata`, never from a list here.
3. **The tests that read it from outside their crate**: `READERS` below. A reader runs only
   in its own crate (the file is not compiled into anything), and only the named tests when
   the entry names them.
4. **Nothing**, when it matches `NO_RUST_TESTS` and no reader: no Rust test reads it.

When in doubt this runs more: a `cargo metadata` or `git diff` that fails, a crate directory
that no longer exists and a path nothing here classifies each select the whole workspace.

It prints, for `$GITHUB_OUTPUT` and for check-fast.sh:

    rust_tests=all|some|none
    rust_filter=<a nextest filterset; empty when rust_tests is all or none>
    rust_packages=--workspace | -p <crate> ... | (empty)   (for `cargo test`, without nextest)
    rust_doc=--workspace | -p <crate> ... | (empty)  (library crates for `cargo test --doc`)
    rust_types=true|false     (ui/scripts/regen-types.sh: a crate with a `ts` feature, or the
                               generated files, changed)

Run the selection as `cargo nextest run --workspace -E "$rust_filter"`, not with `-p`: cargo
does not rebuild a crate nothing changed under, while `-p` resolves features for the named
packages alone and can rebuild their dependencies with another feature set. `rust_packages`
is for `cargo test`, which has no filter: it runs every test of a crate that has a reader.

It also prints to stderr which crates were selected and why, so a test that did not run can
be explained from the log. `scripts/test-ci-rust-tests.py` tests the rules and that `READERS`
still matches the test sources.
"""

import argparse
import fnmatch
import json
import subprocess
import sys
from pathlib import Path

REPO = Path(__file__).resolve().parents[1]

# A change to one of these can affect every crate's tests. `*` also matches `/`.
WORKSPACE = [
    ("Cargo.toml", "the workspace manifest"),
    ("Cargo.lock", "the lock file"),
    ("rust-toolchain", "the toolchain"),
    ("rust-toolchain.toml", "the toolchain"),
    (".cargo/*", "cargo's configuration"),
    (".depot/*", "the CI workflow and its actions"),
    ("scripts/ci-rust-tests.py", "this selection"),
    ("scripts/test-ci-rust-tests.py", "this selection's tests"),
    ("scripts/ci-tier.py", "the tier selection"),
    ("scripts/test-ci-tier.py", "the tier selection's tests"),
    # Tests in most crates start `python -m barca.<helper>` from this checkout, or compare a
    # Rust table with one of these files (see WORKSPACE_READERS).
    ("python/barca/*", "the Python package the Rust tests run"),
    ("pyproject.toml", "the Python package's manifest"),
]

# Tests that read files outside their own crate. `source` is the file holding the test (its
# crate is looked up from the manifests); `paths` are what it reads; `tests` are test function
# names and `binary` an integration test binary, each run alone, and an entry with neither runs
# the source's whole crate. Found by reading every use of CARGO_MANIFEST_DIR and every
# include_str!/include_bytes! that leaves its crate; test-ci-rust-tests.py fails when a test
# source does either and is neither here nor in WORKSPACE_READERS.
READERS = [
    # barca-core: guards that scan every crate.
    {
        "source": "crates/barca-core/src/layering_tests.rs",
        "tests": ["crates_depend_only_on_crates_below_them"],
        "paths": ["crates/*/Cargo.toml"],
    },
    {
        "source": "crates/barca-core/src/write_boundary_tests.rs",
        "tests": ["metadata_writes_go_through_an_owner"],
        "paths": ["crates/*/src/*"],
    },
    {
        "source": "crates/barca-core/src/step_status_literals_tests.rs",
        "tests": ["no_bare_step_status_literal_outside_the_vocabulary"],
        "paths": ["crates/*/src/*"],
    },
    {
        "source": "crates/barca-core/src/helper_launch_tests.rs",
        "tests": ["every_helper_launch_forwards_resolved_storage_options"],
        "paths": ["crates/barca-engine/src/io_loop/lifecycle.rs"],
    },
    # barca-core: the UI's fixtures, a specification table, and every pipeline in the
    # repository (Python files, fenced blocks in the documentation, heredocs in shell tests).
    {
        "source": "crates/barca-core/src/ui_fixture_tests.rs",
        "tests": [
            "the_ui_fixture_health_table_is_what_classify_says",
            "the_ui_fixture_weights_are_the_rollups",
        ],
        "paths": ["ui/src/lib/testdata/*"],
    },
    {
        "source": "crates/barca-core/src/sql.rs",
        "tests": ["format_of_agrees_with_the_spec_table"],
        "paths": ["specs/interfaces/artifact-formats.md"],
    },
    {
        "source": "crates/barca-core/tests/unused_input_repo_sweep.rs",
        "binary": "unused_input_repo_sweep",
        "paths": [
            "examples/*.py",
            "benchmarks/*.py",
            "benchmarks/*.sh",
            "python/*.py",
            "tests/integration/*.sh",
            "README.md",
            "SKILL.md",
            "crates/barca-cli/docs/*.md",
            "site/src/content/*.md",
            "site/src/content/*.mdx",
        ],
    },
    # barca-engine: every child process in every crate goes through helper_proc.
    {
        "source": "crates/barca-engine/src/helper_proc.rs",
        "tests": ["every_child_process_is_started_through_this_module"],
        "paths": ["crates/*/src/*", "crates/*/tests/*"],
    },
    # barca-compile: a UI demo pipeline, and tables in the manual and on the site.
    {
        "source": "crates/barca-compile/src/groups.rs",
        "tests": ["group_names_matching_local_variables_do_not_change_hashes"],
        "paths": ["ui/demo/modeling.py"],
    },
    {
        "source": "crates/barca-compile/src/definition.rs",
        "tests": ["the_manual_lists_exactly_these_rules"],
        "paths": ["crates/barca-cli/docs/hashing.md"],
    },
    {
        "source": "crates/barca-compile/src/decorator_args.rs",
        "tests": ["the_manual_and_the_site_list_exactly_these_arguments"],
        "paths": [
            "crates/barca-cli/docs/assets.md",
            "site/src/content/docs/reference/api/decorators.md",
        ],
    },
    # barca-cli: the README's command table, SKILL.md, the site's reference pages and sidebar,
    # every crate's BARCA_* variables, the server's route table. Whole crate except the last
    # two: the contract tests that read the site are many and share helpers.
    {
        "source": "crates/barca-cli/src/contract.rs",
        "paths": ["README.md", "site/*"],
    },
    {
        "source": "crates/barca-cli/src/contract.rs",
        "tests": ["every_barca_env_var_is_in_the_contract"],
        "paths": ["crates/*/src/*"],
    },
    {"source": "crates/barca-cli/src/tests.rs", "paths": ["README.md"]},
    {"source": "crates/barca-cli/src/docs.rs", "paths": ["SKILL.md"]},
    {
        "source": "crates/barca-cli/src/ui_layering_tests.rs",
        "tests": ["ui_layering_"],
        "paths": ["crates/barca-server/src/routes.rs"],
    },
    # barca-server: responses validated against the OpenAPI document.
    {
        "source": "crates/barca-server/tests/auth.rs",
        "binary": "auth",
        "paths": ["specs/interfaces/server-api.openapi.yaml"],
    },
    {
        "source": "crates/barca-server/tests/openapi.rs",
        "binary": "openapi",
        "paths": ["specs/interfaces/server-api.openapi.yaml"],
    },
]

# Test sources that leave their crate only for paths a WORKSPACE rule covers, or for a path
# that is never in a diff. Listed so the audit in test-ci-rust-tests.py is complete.
WORKSPACE_READERS = {
    "crates/barca-model/src/artifact_format.rs": "python/barca/_artifacts.py",
    "crates/barca-compile/src/load.rs": "python/ on PYTHONPATH",
    "crates/barca-core/src/backup.rs": "python/ on PYTHONPATH",
    "crates/barca-core/src/persist.rs": "python/ on PYTHONPATH",
    "crates/barca-core/src/transfer/tests.rs": ".venv/bin/python (untracked)",
    "crates/barca-engine/src/wire.rs": "python/barca/_wire.py",
    "crates/barca-server/src/sql_query.rs": "python/ and .venv/bin/python",
    "crates/barca-server/src/ui.rs": "ui/dist (untracked build output, absent in rust-tests)",
    "crates/barca-server/tests/api.rs": "python/ on PYTHONPATH",
    "crates/barca-server/tests/running.rs": "python/ on PYTHONPATH",
}

# Changing one of these runs ui/scripts/regen-types.sh, with any crate that has a `ts` feature.
TYPES = ["ui/src/lib/generated/*", "ui/scripts/regen-types.sh"]

# No Rust test reads these, except through a READERS entry. Anything in neither list selects
# the whole workspace, so a new top-level directory runs everything until it is listed here.
NO_RUST_TESTS = [
    "site/*",
    "ui/*",
    "specs/*",
    "docs/*",
    "python/tests/*",
    "examples/*",
    "benchmarks/*",
    "tests/*",
    "scripts/*",
    ".cursor/*",
    ".github/dependabot.yml",
    ".github/pull_request_template.md",
    # The release, tagging, simulation and site workflows: none is used by ci.yml, and a
    # release runs the full tier. A workflow not listed here selects the whole workspace.
    ".github/workflows/release.yml",
    ".github/workflows/release-tag.yml",
    ".github/workflows/simulation.yml",
    ".github/workflows/site.yml",
    "AGENTS.md",
    "CHANGELOG.md",
    "CLAUDE.md",
    "DIRECTION.md",
    "GLOSSARY.md",
    "LICENSE",
    "README.md",
    "SKILL.md",
    ".dockerignore",
    ".gitignore",
    "prek.toml",
    "uv.lock",
]


def matches(path, patterns):
    return any(fnmatch.fnmatchcase(path, p) for p in patterns)


def workspace():
    """The workspace from `cargo metadata`: {crate: {"dir", "deps", "lib", "ts"}}."""
    out = subprocess.run(
        ["cargo", "metadata", "--no-deps", "--format-version", "1", "--offline"],
        cwd=REPO,
        check=True,
        capture_output=True,
        text=True,
    ).stdout
    return parse_metadata(json.loads(out))


def parse_metadata(meta):
    root = Path(meta["workspace_root"])
    by_dir = {str(Path(p["manifest_path"]).parent): p["name"] for p in meta["packages"]}
    crates = {}
    for p in meta["packages"]:
        crates[p["name"]] = {
            "dir": Path(p["manifest_path"]).parent.relative_to(root).as_posix(),
            # Every kind (normal, dev, build) and target: a dev-dependency's change can fail
            # this crate's tests too. Path dependencies are matched by directory, not name.
            "deps": sorted(
                {
                    by_dir[str(Path(d["path"]))]
                    for d in p["dependencies"]
                    if d.get("path") and str(Path(d["path"])) in by_dir
                }
            ),
            "lib": any(
                kind in ("lib", "rlib", "proc-macro") for t in p["targets"] for kind in t["kind"]
            ),
            "ts": "ts" in p.get("features", {}),
        }
    return crates


def owner(path, crates):
    """The crate whose directory holds `path` (the deepest, for a crate inside a crate)."""
    best = None
    for name, c in crates.items():
        if path.startswith(c["dir"] + "/") and (
            best is None or len(c["dir"]) > len(crates[best]["dir"])
        ):
            best = name
    return best


def dependents(name, crates):
    """`name` and every crate that depends on it, transitively."""
    seen = {name}
    grew = True
    while grew:
        grew = False
        for other, c in crates.items():
            if other not in seen and seen.intersection(c["deps"]):
                seen.add(other)
                grew = True
    return seen


def select(files, crates):
    """What `files` select: {"all": reason or None, "whole": {crate: [reason]},
    "readers": {crate: {("test"|"binary", name): [file]}}, "types": [reason]}."""
    sel = {"all": None, "whole": {}, "readers": {}, "types": []}
    for path in files:
        if not path:
            continue
        rule = next((why for pattern, why in WORKSPACE if fnmatch.fnmatchcase(path, pattern)), None)
        if rule:
            sel["all"] = sel["all"] or f"{path} ({rule})"
            continue
        known = False
        if matches(path, TYPES):
            sel["types"].append(path)
            known = True
        own = owner(path, crates)
        if own is None and path.startswith("crates/"):
            # A crate that was removed or renamed, or a file beside the crates.
            sel["all"] = sel["all"] or f"{path} (in no workspace crate; no rule knows it)"
            continue
        if own:
            known = True
            for crate in sorted(dependents(own, crates)):
                why = f"owns {path}" if crate == own else f"depends on {own} ({path})"
                sel["whole"].setdefault(crate, []).append(why)
        for reader in READERS:
            if not matches(path, reader["paths"]):
                continue
            known = True
            crate = owner(reader["source"], crates)
            if crate is None:
                sel["all"] = sel["all"] or (
                    f"{path} (read by {reader['source']}, which is in no workspace crate)"
                )
            elif "tests" in reader or "binary" in reader:
                keys = [("test", t) for t in reader.get("tests", [])]
                if "binary" in reader:
                    keys.append(("binary", reader["binary"]))
                for key in keys:
                    sel["readers"].setdefault(crate, {}).setdefault(key, []).append(path)
            else:
                sel["whole"].setdefault(crate, []).append(f"its tests read {path}")
        if not known and not matches(path, NO_RUST_TESTS):
            sel["all"] = sel["all"] or f"{path} (no rule knows it: scripts/ci-rust-tests.py)"
    for crate in list(sel["readers"]):
        if crate in sel["whole"]:
            del sel["readers"][crate]
    for crate, whys in sel["whole"].items():
        if crates[crate]["ts"]:
            sel["types"].append(f"{crate} has a `ts` feature")
            break
    return sel


def outputs(sel, crates):
    """The key=value lines for a selection."""
    if sel["all"]:
        return {
            "rust_tests": "all",
            "rust_packages": "--workspace",
            "rust_filter": "",
            "rust_doc": "--workspace",
            "rust_types": "true",
        }
    whole = sorted(sel["whole"])
    partial = sorted(sel["readers"])
    packages = sorted(set(whole) | set(partial))
    clauses = [f"package(={c})" for c in whole]
    for crate in partial:
        tests = " | ".join(
            f"test(~{name})" if kind == "test" else f"binary(={name})"
            for kind, name in sorted(sel["readers"][crate])
        )
        clauses.append(f"(package(={crate}) & ({tests}))")
    return {
        "rust_tests": "some" if packages else "none",
        "rust_packages": " ".join(f"-p {c}" for c in packages),
        "rust_filter": " | ".join(clauses),
        "rust_doc": " ".join(f"-p {c}" for c in whole if crates[c]["lib"]),
        "rust_types": "true" if sel["types"] else "false",
    }


def some(items, limit=3):
    items = list(dict.fromkeys(items))
    more = f" and {len(items) - limit} more" if len(items) > limit else ""
    return ", ".join(items[:limit]) + more


def explain(sel, crates, count):
    """The reasons, one line per crate, for the log."""
    lines = [f"rust tests: {count} changed file(s)"]
    if sel["all"]:
        lines.append(f"  the whole workspace: {sel['all']}")
        return lines
    for crate in sorted(crates):
        if crate in sel["whole"]:
            lines.append(f"  {crate}: every test: {some(sel['whole'][crate])}")
        elif crate in sel["readers"]:
            for (kind, name), files in sorted(sel["readers"][crate].items()):
                lines.append(f"  {crate}: only {kind} {name}: reads {some(files)}")
        else:
            lines.append(f"  {crate}: not run: no changed file can affect its tests")
    types = some(sel["types"]) if sel["types"] else "not run"
    lines.append(f"  generated UI types (ui/scripts/regen-types.sh): {types}")
    return lines


def main():
    ap = argparse.ArgumentParser(description=__doc__.split("\n\n")[0])
    group = ap.add_mutually_exclusive_group(required=True)
    group.add_argument("--base", help="the commit the change is compared with")
    group.add_argument("--files-from", metavar="FILE", help="changed paths, one a line (- stdin)")
    group.add_argument("--all", action="store_true", help="select the whole workspace")
    ap.add_argument("--head", default="HEAD", help="the change's commit (default HEAD)")
    args = ap.parse_args()

    files, sel, crates = [], None, {}
    if args.all:
        sel = {"all": "--all (the full tier, or asked for)"}
    else:
        try:
            if args.base:
                files = (
                    subprocess.run(
                        ["git", "-C", str(REPO), "diff", "--name-only", "--no-renames", "-z"]
                        + [f"{args.base}...{args.head}"],
                        check=True,
                        capture_output=True,
                        text=True,
                    )
                    .stdout.rstrip("\0")
                    .split("\0")
                )
            else:
                source = sys.stdin if args.files_from == "-" else open(args.files_from)
                files = source.read().splitlines()
            files = [f for f in files if f]
            crates = workspace()
            sel = select(files, crates)
        except (OSError, subprocess.CalledProcessError) as e:
            detail = (getattr(e, "stderr", None) or str(e)).strip()
            sel = {"all": f"the selection could not be computed ({detail})"}
    print("\n".join(explain(sel, crates, len(files))), file=sys.stderr)
    for key, value in outputs(sel, crates).items():
        print(f"{key}={value}")
    return 0


if __name__ == "__main__":
    sys.exit(main())
