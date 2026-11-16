#!/usr/bin/env python3
"""Select CI from PR metadata or the checked-out merge-group commit.

A pull request runs static checks only; building and testing happen in the merge queue
(#945). Within a tier, the parts a diff cannot affect are skipped:

    tier=<fast|merge|ui|ui-merge|site|site-merge|full>
    rust=<true|false>     clippy and the Rust tests (merge tier; always true in full)
    site=<true|false>     the docs site build (merge groups and full only)
    models=<true|false>   the TLA+ specification models

With `--rust-inputs`, read changed paths from stdin instead and exit 0 when one of them is an
input of the Rust build or tests, 1 when none is (scripts/check-fast.sh uses this).
"""

import json
import os
import subprocess
import sys

# A diff only of these is a docs-site change (the site, the changelog its Release Notes page
# renders, and the site's deploy workflow).
SITE_PREFIXES = ("site/",)
SITE_FILES = {"CHANGELOG.md", ".github/workflows/site.yml"}
# Site pages a Python test executes (python/tests/test_docs_examples.py): changing one runs the
# normal tiers. Every other site page the tests read is read by the Rust tests, which the
# site-merge tier runs.
SITE_PYTHON_TESTED = {"site/src/content/docs/recipes/ordering-only-deps.md"}
# What the docs site build reads: the site and the changelog (site/.../releases.mdx imports it).
SITE_INPUTS = ("site/", "CHANGELOG.md", ".github/workflows/site.yml")
# What the TLA+ model check reads.
MODEL_INPUTS = ("specs/models/",)
# Changing CI itself runs every optional part.
CI_FILES = (".depot/", "scripts/ci-tier.py", "scripts/select-python-tests.py")

# Paths that are not inputs of the Rust build or the Rust tests. Everything else is: the crates
# and Cargo files, python/ (the tests run its workers, and compare python/barca with Rust),
# README.md and SKILL.md, site/src/content, specs/interfaces, ui/ (embedded, and the generated
# wire types), and the *.sh scripts under tests/integration and benchmarks
# (crates/barca-core/tests/unused_input_repo_sweep.rs). Fails closed: a path not listed here
# runs the Rust jobs.
NOT_RUST_PREFIXES = (
    "python/tests/",
    "docs/",
    "specs/behavior/",
    "specs/models/",
    "specs/proposals/",
    "scripts/",
    ".github/",
)
NOT_RUST_FILES = {
    "AGENTS.md",
    "CLAUDE.md",
    "DIRECTION.md",
    "CHANGELOG.md",
    "specs/README.md",
    "prek.toml",
    ".gitignore",
}


def site_only(files):
    return (
        any(p.startswith(SITE_PREFIXES) for p in files)
        and all(p.startswith(SITE_PREFIXES) or p in SITE_FILES for p in files)
        and not any(p in SITE_PYTHON_TESTED for p in files)
    )


def ui_only(files):
    return any(p.startswith("ui/") for p in files) and all(
        p.startswith("ui/") or p == "CHANGELOG.md" for p in files
    )


def rust_input(path):
    if path in NOT_RUST_FILES or path.startswith(NOT_RUST_PREFIXES):
        return False
    # benchmarks/: only the shell scripts are read (the unused-input sweep).
    if path.startswith("benchmarks/"):
        return path.endswith(".sh")
    return True


def tier(event, head_ref, labels, message, files=None):
    if event not in ("pull_request", "merge_group"):
        raise ValueError(f"unsupported CI event: {event}")
    if head_ref.startswith("release/v") or "ci-full" in labels or "chore(release)" in message:
        return "full"
    if files and ui_only(files):
        return "ui" if event == "pull_request" else "ui-merge"
    if files and site_only(files):
        return "site" if event == "pull_request" else "site-merge"
    if event == "pull_request":
        return "fast"
    return "merge"


def parts(event, selected, files=None):
    """The optional parts of `selected` a diff of `files` (None: unknown) needs."""
    full = selected == "full"
    unknown = files is None
    ci = not unknown and any(p.startswith(CI_FILES) for p in files)

    def touches(prefixes):
        return full or unknown or ci or any(p.startswith(prefixes) for p in files)

    return {
        "rust": full or unknown or ci or any(rust_input(p) for p in files),
        # A build: never on a pull request (site.yml builds a preview of a PR that changes it).
        "site": (full or event == "merge_group") and touches(SITE_INPUTS),
        "models": touches(MODEL_INPUTS),
    }


def changed_files(base):
    try:
        out = subprocess.check_output(
            ["git", "diff", "--name-only", "--no-renames", "-z", f"{base}...HEAD"], text=True
        )
    except subprocess.CalledProcessError:
        return None  # Missing history must retain the broader existing gates.
    return [p for p in out.split("\0") if p]


def main():
    if sys.argv[1:] == ["--rust-inputs"]:
        paths = [line.strip() for line in sys.stdin if line.strip()]
        sys.exit(0 if any(rust_input(p) for p in paths) else 1)
    event = os.environ["GITHUB_EVENT_NAME"]
    # merge_group payloads do not reliably expose head_commit.message in the provider's
    # expression context. Checkout already points at the exact proposed main commit.
    message = (
        subprocess.check_output(["git", "log", "-1", "--format=%B"], text=True)
        if event == "merge_group"
        else ""
    )
    base = os.environ.get("CI_BASE_SHA")
    files = changed_files(base) if base else None
    selected = tier(
        event,
        os.environ.get("CI_HEAD_REF", ""),
        json.loads(os.environ.get("CI_LABELS") or "[]") or [],
        message,
        files,
    )
    print(f"tier={selected}")
    for name, value in parts(event, selected, files).items():
        print(f"{name}={str(value).lower()}")


if __name__ == "__main__":
    main()
