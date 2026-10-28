#!/usr/bin/env python3
"""Split the Python test files into CI shards balanced by duration.

The `backends` check in `.depot/workflows/ci.yml` runs `python/tests` as several parallel
jobs. Each job asks this script for its files:

    python3 scripts/ci-shards.py --shards 4 --shard 1              # one path per line
    python3 scripts/ci-shards.py --shards 4 --shard 1 --emulators  # when CI starts them

The split is by file, so `--dist loadscope` inside a shard keeps its module and class
grouping. Every `python/tests/test_*.py` file goes to exactly one shard; `--check` verifies
that, for both maps below, and prints the estimated length of each shard:

    python3 scripts/ci-shards.py --shards 4 --check

Weights come from `scripts/ci-test-durations.json`: seconds per file, summed over its tests,
in two maps. `with_emulators` is a run with the object-store emulators up; `without_emulators`
is a run without them, where the backend tests skip (most PRs). A file missing from a map, such
as a new one, is weighted by its size at the median seconds per byte of the listed files. To
refresh a map from a run (only the files in the report are replaced):

    pytest python/tests -n 4 --dist loadscope --junitxml=/tmp/junit.xml
    python3 scripts/ci-shards.py --update-durations /tmp/junit.xml [--emulators]

Assignment is deterministic: longest file first, onto the shard whose estimated length
(its tests over the workers, but no less than its longest file) grows least, ties to the
lower total, then the lower shard. Every shard job computes the same split.
"""

import argparse
import json
import statistics
import sys
import xml.etree.ElementTree as ET
from pathlib import Path

REPO = Path(__file__).resolve().parents[1]
TESTS = REPO / "python" / "tests"
DURATIONS = REPO / "scripts" / "ci-test-durations.json"
MAPS = ("with_emulators", "without_emulators")
# Workers per shard (`pytest -n`, one per CPU of the 8-CPU shard sandbox).
WORKERS = 8
# Measured on CI: 8 workers do about 6 workers of serial test time (setup, uneven scopes).
EFFECTIVE_WORKERS = 6


def map_name(emulators: bool) -> str:
    return MAPS[0] if emulators else MAPS[1]


def test_files() -> list[str]:
    return sorted(p.relative_to(REPO).as_posix() for p in TESTS.glob("test_*.py"))


def load_all() -> dict[str, dict[str, float]]:
    data = json.loads(DURATIONS.read_text()) if DURATIONS.exists() else {}
    return {m: data.get(m, {}) for m in MAPS}


def load_durations(emulators: bool) -> dict[str, float]:
    return load_all()[map_name(emulators)]


def weights(files: list[str], durations: dict[str, float]) -> dict[str, float]:
    known = {f: durations[f] for f in files if f in durations}
    rates = [
        known[f] / size for f in known if known[f] > 0 and (size := (REPO / f).stat().st_size) > 0
    ]
    rate = statistics.median(rates) if rates else 1e-4
    return {f: known.get(f, (REPO / f).stat().st_size * rate) for f in files}


def split(shards: int, emulators: bool = False) -> list[list[str]]:
    files = test_files()
    weight = weights(files, load_durations(emulators))
    totals = [0.0] * shards
    longest = [0.0] * shards
    assigned: list[list[str]] = [[] for _ in range(shards)]

    def after(i: int, w: float) -> float:
        return estimate(totals[i] + w, max(longest[i], w))

    for f in sorted(files, key=lambda f: (-weight[f], f)):
        w = weight[f]
        i = min(range(shards), key=lambda i: (after(i, w), totals[i], i))
        totals[i] += w
        longest[i] = max(longest[i], w)
        assigned[i].append(f)
    return [sorted(a) for a in assigned]


def estimate(total: float, longest: float) -> float:
    """A shard's length: its tests spread over the workers, but no shorter than its longest
    file, whose tests one worker runs in turn (`--dist loadscope`)."""
    return max(total / EFFECTIVE_WORKERS, longest)


def check_one(shards: int, emulators: bool) -> list[str]:
    files = test_files()
    assigned = split(shards, emulators)
    seen: dict[str, int] = {}
    problems = []
    for i, part in enumerate(assigned, 1):
        if not part:
            problems.append(f"shard {i} is empty")
        for f in part:
            if f in seen:
                problems.append(f"{f} is in shards {seen[f]} and {i}")
            seen[f] = i
    problems += [f"{f} is in no shard" for f in sorted(set(files) - set(seen))]
    problems += [f"{f} is not a test file" for f in sorted(set(seen) - set(files))]
    durations = load_durations(emulators)
    weight = weights(files, durations)
    print(f"{map_name(emulators)}:", file=sys.stderr)
    for i, part in enumerate(assigned, 1):
        total = sum(weight[f] for f in part)
        longest = max((weight[f] for f in part), default=0.0)
        print(
            f"  shard {i}: {len(part)} files, {total:.0f}s of tests, "
            f"about {estimate(total, longest):.0f}s with -n {WORKERS}",
            file=sys.stderr,
        )
    unknown = [f for f in files if f not in durations]
    if unknown:
        print(f"  weighted by size (no recorded duration): {', '.join(unknown)}", file=sys.stderr)
    return [f"{map_name(emulators)}: {p}" for p in problems]


def check(shards: int) -> int:
    problems = check_one(shards, False) + check_one(shards, True)
    for p in problems:
        print(f"error: {p}", file=sys.stderr)
    n = len(test_files())
    print(
        f"{n} test files, each in exactly one of {shards} shards"
        if not problems
        else "shard split is wrong",
        file=sys.stderr,
    )
    return 1 if problems else 0


def update_durations(junit: Path, emulators: bool) -> int:
    per_file: dict[str, float] = {}
    for case in ET.parse(junit).getroot().iter("testcase"):
        # pytest writes `python.tests.test_x` or `python.tests.test_x.TestClass`.
        parts = case.get("classname", "").split(".")
        module = next((p for p in parts if p.startswith("test_")), None)
        if module is None:
            continue
        f = f"python/tests/{module}.py"
        per_file[f] = per_file.get(f, 0.0) + float(case.get("time", 0) or 0)
    files = set(test_files())
    data = load_all()
    name = map_name(emulators)
    data[name].update({f: round(t, 1) for f, t in per_file.items() if f in files})
    data = {m: dict(sorted((f, t) for f, t in data[m].items() if f in files)) for m in MAPS}
    DURATIONS.write_text(json.dumps(data, indent=1) + "\n")
    print(f"updated {len(per_file)} durations in {name}", file=sys.stderr)
    return 0


def main() -> int:
    ap = argparse.ArgumentParser(description=__doc__.split("\n\n")[0])
    ap.add_argument("--shards", type=int, help="number of shards")
    ap.add_argument("--shard", type=int, help="print the files of this shard (1-based)")
    ap.add_argument(
        "--emulators", action="store_true", help="use (or update) the with_emulators map"
    )
    ap.add_argument("--check", action="store_true", help="verify the split and print it")
    ap.add_argument(
        "--update-durations",
        type=Path,
        metavar="JUNIT_XML",
        help="update a duration map from a pytest --junitxml report",
    )
    args = ap.parse_args()
    if args.update_durations:
        return update_durations(args.update_durations, args.emulators)
    if not args.shards or args.shards < 1:
        ap.error("--shards N is required")
    if args.check:
        return check(args.shards)
    if args.shard is None or not 1 <= args.shard <= args.shards:
        ap.error(f"--shard must be between 1 and {args.shards}")
    print("\n".join(split(args.shards, args.emulators)[args.shard - 1]))
    return 0


if __name__ == "__main__":
    sys.exit(main())
