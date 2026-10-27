#!/usr/bin/env python3
"""Apply a release's version bump to the working tree (scripts/release.sh runs this).

Edits exactly what a release commit changes: `version` in crates/*/Cargo.toml, the barca,
barca-core and barca-server entries in Cargo.lock, `version` in pyproject.toml, the barca entry
in uv.lock, `__version__` in python/barca/__init__.py, and CHANGELOG.md, where the `## Pending`
notes become `## X.Y.Z (YYYY-MM-DD)` under a new empty `## Pending`.

    python3 scripts/bump-version.py 0.35.0                  # date: today (UTC)
    python3 scripts/bump-version.py 0.35.0 --date 2026-10-27
    python3 scripts/bump-version.py --check 0.35.0          # validate only, edit nothing
    python3 scripts/bump-version.py 0.35.0 --root ../other  # another checkout

Exits 1 (and edits nothing) when the current versions disagree, the new version is not
greater, `## Pending` is empty, or a pre-1.0 patch bump carries `### Breaking` notes.
"""

import argparse
import datetime
import re
import sys
from pathlib import Path

ROOT = Path(__file__).resolve().parents[1]  # --root overrides
CRATES = ["barca-cli", "barca-core", "barca-server"]
LOCKED = {"barca", "barca-core", "barca-server"}  # Cargo.lock package names
HEADINGS = {"Breaking", "Features", "Bug Fixes", "Performance", "Removed", "Documentation"}
SEMVER = re.compile(r"^(\d+)\.(\d+)\.(\d+)$")


def parse(version: str) -> tuple[int, int, int]:
    match = SEMVER.match(version)
    if not match:
        raise SystemExit(f"error: {version!r} is not X.Y.Z")
    return tuple(int(part) for part in match.groups())  # type: ignore[return-value]


def sub_once(pattern: str, repl: str, text: str, what: str) -> str:
    new, count = re.subn(pattern, repl, text, count=1, flags=re.MULTILINE)
    if count != 1:
        raise SystemExit(f"error: could not find the version in {what}")
    return new


def pending(text: str) -> tuple[int, int]:
    """Start and end offsets of the `## Pending` section body."""
    match = re.search(r"^## Pending[ \t]*\n", text, flags=re.MULTILINE)
    if not match:
        raise SystemExit("error: CHANGELOG.md has no `## Pending` heading")
    nxt = re.search(r"^## ", text[match.end() :], flags=re.MULTILINE)
    end = match.end() + nxt.start() if nxt else len(text)
    return match.end(), end


def current_version() -> str:
    text = (ROOT / "pyproject.toml").read_text()
    match = re.search(r'^version = "([^"]+)"', text, flags=re.MULTILINE)
    if not match:
        raise SystemExit("error: no version in pyproject.toml")
    return match.group(1)


def check(new: str) -> list[str]:
    """Validate the bump; return warnings. Raises SystemExit on a refusal."""
    old = current_version()
    old_v, new_v = parse(old), parse(new)
    if new_v <= old_v:
        raise SystemExit(f"error: {new} is not greater than the current version {old}")
    warnings = []
    changelog = (ROOT / "CHANGELOG.md").read_text()
    start, end = pending(changelog)
    body = changelog[start:end]
    if not body.strip():
        raise SystemExit("error: `## Pending` in CHANGELOG.md is empty: nothing to release")
    headings = re.findall(r"^### (.+?)\s*$", body, flags=re.MULTILINE)
    for heading in headings:
        if heading not in HEADINGS:
            warnings.append(
                f"`### {heading}` under Pending is not one of {', '.join(sorted(HEADINGS))};"
                " it ships in the release notes as is"
            )
    breaking = "Breaking" in headings
    is_patch = new_v[:2] == old_v[:2]
    is_minor = new_v[0] == old_v[0] and new_v[1] > old_v[1]
    if old_v[0] == 0:
        if breaking and is_patch:
            raise SystemExit(
                f"error: Pending has `### Breaking` notes, so pre-1.0 this is a minor release"
                f" ({old_v[0]}.{old_v[1] + 1}.0), not {new}"
            )
    elif breaking and not new_v[0] > old_v[0]:
        raise SystemExit(
            "error: Pending has `### Breaking` notes; from 1.0 that needs a major bump"
        )
    expected = {
        (old_v[0], old_v[1], old_v[2] + 1),
        (old_v[0], old_v[1] + 1, 0),
        (old_v[0] + 1, 0, 0),
    }
    if new_v not in expected:
        warnings.append(f"{new} skips versions after {old}")
    elif is_minor and new_v[2] != 0:
        warnings.append(f"a minor release normally ends in .0, not {new}")
    for path in [*(f"crates/{c}/Cargo.toml" for c in CRATES), "python/barca/__init__.py"]:
        found = re.search(r'^(?:version|__version__) = "([^"]+)"', (ROOT / path).read_text(), re.M)
        if not found or found.group(1) != old:
            raise SystemExit(
                f"error: {path} has version {found.group(1) if found else '?'}, pyproject.toml {old};"
                " fix the mismatch first (scripts/check-version-sync.sh)"
            )
    return warnings


def apply(new: str, date: str) -> None:
    old = current_version()
    edits: dict[Path, str] = {}
    for crate in CRATES:
        path = ROOT / "crates" / crate / "Cargo.toml"
        edits[path] = sub_once(
            r'^version = "[^"]+"', f'version = "{new}"', path.read_text(), str(path)
        )
    path = ROOT / "pyproject.toml"
    edits[path] = sub_once(r'^version = "[^"]+"', f'version = "{new}"', path.read_text(), str(path))
    path = ROOT / "python" / "barca" / "__init__.py"
    edits[path] = sub_once(
        r'^__version__ = "[^"]+"', f'__version__ = "{new}"', path.read_text(), str(path)
    )

    # Lock files: rewrite the version line of each named [[package]] entry.
    def relock(path: Path, names: set[str]) -> str:
        blocks = path.read_text().split("\n[[package]]\n")
        seen = set()
        for i, block in enumerate(blocks):
            name = re.match(r'name = "([^"]+)"\nversion = "([^"]+)"\n', block)
            if i and name and name.group(1) in names:
                if name.group(2) != old:
                    raise SystemExit(
                        f"error: {path.name} has {name.group(1)} {name.group(2)}, not {old}"
                    )
                blocks[i] = block.replace(f'version = "{old}"\n', f'version = "{new}"\n', 1)
                seen.add(name.group(1))
        if seen != names:
            raise SystemExit(
                f"error: {path.name} lacks entries for {', '.join(sorted(names - seen))}"
            )
        return "\n[[package]]\n".join(blocks)

    edits[ROOT / "Cargo.lock"] = relock(ROOT / "Cargo.lock", LOCKED)
    edits[ROOT / "uv.lock"] = relock(ROOT / "uv.lock", {"barca"})

    path = ROOT / "CHANGELOG.md"
    text = path.read_text()
    start, _ = pending(text)
    edits[path] = text[:start] + f"\n## {new} ({date})\n" + text[start:]

    for path, text in edits.items():
        path.write_text(text)


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    parser.add_argument("version", help="the new version, X.Y.Z (a leading v is accepted)")
    parser.add_argument("--date", help="release date, YYYY-MM-DD (default: today, UTC)")
    parser.add_argument("--check", action="store_true", help="validate only; edit nothing")
    parser.add_argument("--root", type=Path, help="repository to read and edit (default: this one)")
    args = parser.parse_args()
    if args.root:
        global ROOT
        ROOT = args.root.resolve()
    new = args.version.removeprefix("v")
    parse(new)
    date = args.date or datetime.datetime.now(datetime.UTC).date().isoformat()
    if not re.fullmatch(r"\d{4}-\d{2}-\d{2}", date):
        raise SystemExit(f"error: --date {date!r} is not YYYY-MM-DD")
    for warning in check(new):
        print(f"warning: {warning}", file=sys.stderr)
    if not args.check:
        apply(new, date)
        print(f"bumped {current_version()} ({date})", file=sys.stderr)
    return 0


if __name__ == "__main__":
    sys.exit(main())
