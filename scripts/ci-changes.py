#!/usr/bin/env python3
"""Decide which optional parts of the `backends` CI job a change needs.

Three parts of `.depot/workflows/ci.yml` run only when a change can affect them:

- **emulators**: the object-store emulators (MinIO, fake-gcs-server, Azurite) and the
  `BARCA_TEST_*` variables that make the backend conformance tests run instead of skip.
- **duckdb1**: the second run of every test file that mentions duckdb, against duckdb 1.x.
- **checkpoint**: `python/tests/test_minute_checkpoints.py`, which waits about two minutes on
  the real one-minute backup checkpoint timer (RFC-0009). Off, CI sets
  `BARCA_TEST_MINUTE_CHECKPOINT=0` and the test skips.

    python3 scripts/ci-changes.py --base <sha>          # diff <sha>...HEAD (or --head)
    python3 scripts/ci-changes.py --all                 # every part on (a release branch)
    python3 scripts/ci-changes.py --base <sha> --on checkpoint   # and this part regardless

It prints `emulators=true|false`, `duckdb1=true|false` and `checkpoint=true|false` on stdout
(for `$GITHUB_OUTPUT`) and the files that turned each one on to stderr. If the diff cannot be
computed (a base commit missing from the clone), every part is on.

A change turns a part on when it touches a path below, or:
- emulators: a test file (or test helper) that uses the emulators (`emulators` or `BARCA_TEST_`);
- duckdb1: a test file that mentions duckdb (the files the rerun selects), or a changed line
  that mentions duckdb in any Python or Rust source.
"""

import argparse
import fnmatch
import re
import subprocess
import sys
from pathlib import Path

REPO = Path(__file__).resolve().parents[1]

# Paths whose change can affect every part: dependencies and the workflow itself.
ALWAYS = [
    "pyproject.toml",
    "uv.lock",
    ".depot/workflows/ci.yml",
    "scripts/ci-changes.py",
]

EMULATORS = ALWAYS + [
    # Python side: store I/O, background transfer, metadata backups, credentials, sinks,
    # remote artifact shapes.
    "python/barca/_storage.py",
    "python/barca/_transfer.py",
    "python/barca/_artifacts.py",
    "python/barca/_state.py",
    "python/barca/_identity.py",
    "python/barca/_sinks.py",
    "python/barca/_inspect.py",
    # Rust side: the transfer client and run sync, backups and restore, the server ledger,
    # unsynced uploads, dataset sinks, store garbage collection.
    "crates/barca-core/src/transfer.rs",
    "crates/barca-core/src/store_sync.rs",
    "crates/barca-core/src/backup.rs",
    "crates/barca-core/src/backup/*",
    "crates/barca-core/src/state_validate.rs",
    "crates/barca-core/src/pending_uploads.rs",
    "crates/barca-core/src/ledger.rs",
    "crates/barca-core/src/dataset_sinks.rs",
    "crates/barca-core/src/gc/*",
    "crates/barca-cli/src/commands/state.rs",
    "crates/barca-cli/src/commands/gc.rs",
    "crates/barca-server/src/ledger.rs",
    # Test helpers that start or address the emulators.
    "python/tests/emulators.py",
    "python/tests/planning_pipeline_store.py",
    "python/tests/conftest.py",
]

DUCKDB1 = ALWAYS + [
    "python/barca/_duckdb.py",
    "python/barca/_sql.py",
    "python/barca/_sinks.py",
    "python/barca/_batch.py",
    "python/barca/_artifacts.py",
    "crates/barca-core/src/sql.rs",
    "crates/barca-core/src/sql/*",
    "crates/barca-core/src/dataset_sinks.rs",
    "crates/barca-core/src/sink_args.rs",
    "crates/barca-cli/src/commands/sql.rs",
    "python/tests/conftest.py",
]

# The minute checkpoint: the run's step recorder and its checkpoint timer, the backup it
# takes, the helper process that runs it, the store side, `barca state backups|restore`, and
# the test with the helpers it imports.
CHECKPOINT = ALWAYS + [
    "crates/barca-core/src/db.rs",
    "crates/barca-core/src/config.rs",
    "crates/barca-core/src/backup/*",
    "crates/barca-server/src/lib.rs",
    "crates/barca-server/src/runs.rs",
    "crates/barca-core/src/persist.rs",
    "crates/barca-core/src/backup.rs",
    "crates/barca-core/src/helper_proc.rs",
    "crates/barca-cli/src/commands/state.rs",
    "python/barca/_state.py",
    "python/barca/_storage.py",
    "python/tests/test_minute_checkpoints.py",
    "python/tests/test_incremental_persist.py",
    "python/tests/test_serve_robustness.py",
]

PARTS = ("emulators", "duckdb1", "checkpoint")

EMULATOR_TEST = re.compile(r"\bemulators\b|BARCA_TEST_")
DUCKDB = re.compile(r"duckdb", re.IGNORECASE)


def matches(path: str, patterns: list[str]) -> bool:
    return any(fnmatch.fnmatchcase(path, p) for p in patterns)


def is_test_source(path: str) -> bool:
    return path.startswith("python/tests/") and path.endswith(".py")


def file_mentions(path: str, pattern: re.Pattern) -> bool:
    p = REPO / path
    return p.is_file() and bool(pattern.search(p.read_text(errors="replace")))


def git(*args: str) -> str:
    return subprocess.run(
        ["git", "-C", str(REPO), *args], check=True, capture_output=True, text=True
    ).stdout


def changed_lines_mention(base: str, path: str, pattern: re.Pattern, head: str = "HEAD") -> bool:
    diff = git("diff", "--unified=0", f"{base}...{head}", "--", path)
    return any(
        line[:1] in "+-" and not line.startswith(("+++", "---")) and pattern.search(line)
        for line in diff.splitlines()
    )


def decide(base: str, files: list[str], head: str = "HEAD") -> dict[str, list[str]]:
    why: dict[str, list[str]] = {part: [] for part in PARTS}
    for f in files:
        if matches(f, EMULATORS) or (is_test_source(f) and file_mentions(f, EMULATOR_TEST)):
            why["emulators"].append(f)
        if (
            matches(f, DUCKDB1)
            or (is_test_source(f) and file_mentions(f, DUCKDB))
            or (f.endswith((".py", ".rs")) and changed_lines_mention(base, f, DUCKDB, head))
        ):
            why["duckdb1"].append(f)
        if matches(f, CHECKPOINT):
            why["checkpoint"].append(f)
    return why


def main() -> int:
    ap = argparse.ArgumentParser(description=__doc__.split("\n\n")[0])
    group = ap.add_mutually_exclusive_group(required=True)
    group.add_argument("--base", help="the commit the change is compared with")
    group.add_argument("--all", action="store_true", help="turn every part on")
    ap.add_argument("--head", default="HEAD", help="the change's commit (default HEAD)")
    ap.add_argument(
        "--on", action="append", default=[], choices=PARTS, help="turn this part on regardless"
    )
    args = ap.parse_args()

    if args.all:
        print("all parts on", file=sys.stderr)
        parts = dict.fromkeys(PARTS, True)
    else:
        try:
            files = git("diff", "--name-only", f"{args.base}...{args.head}").split()
            why = decide(args.base, files, args.head)
        except subprocess.CalledProcessError as e:
            print(
                f"cannot diff against {args.base} ({e.stderr.strip()}); all parts on",
                file=sys.stderr,
            )
            parts = dict.fromkeys(PARTS, True)
        else:
            print(f"{len(files)} changed file(s) since {args.base}", file=sys.stderr)
            for part, hits in why.items():
                print(
                    f"{part}: " + (", ".join(hits) if hits else "no relevant change"),
                    file=sys.stderr,
                )
            parts = {part: bool(hits) for part, hits in why.items()}
    for part in args.on:
        print(f"{part}: on (--on {part})", file=sys.stderr)
        parts[part] = True
    for part, on in parts.items():
        print(f"{part}={'true' if on else 'false'}")
    return 0


if __name__ == "__main__":
    sys.exit(main())
