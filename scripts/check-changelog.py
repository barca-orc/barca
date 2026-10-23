#!/usr/bin/env python3
"""Fail when a change edits the notes of a version that has already been released.

A pull request branched before a release can merge with its CHANGELOG.md bullets inside the
`## <version> (<date>)` section that release just shipped, instead of under `## Pending`: git
applies the hunk by its context lines. This check catches that. For every released section (a
`## X.Y.Z (date)` heading with a `vX.Y.Z` tag), the section must equal its copy in CHANGELOG.md
at the tag, or its copy at the base revision (a section already changed on the base is not this
change's doing). A section without a tag yet, such as the release PR's own, is not checked.

A deliberate fix to released notes (a typo, moving misplaced bullets out) passes when a commit
in BASE..HEAD carries a `Changelog-Fix: <reason>` line in its message.

    python3 scripts/check-changelog.py --base origin/main
"""

import argparse
import importlib.util
import re
import subprocess
import sys
from pathlib import Path

_SPEC = importlib.util.spec_from_file_location(
    "changelog_section", Path(__file__).resolve().parent / "changelog-section.py"
)
_SECTION = importlib.util.module_from_spec(_SPEC)
_SPEC.loader.exec_module(_SECTION)
section = _SECTION.section

HEADING = re.compile(r"^## (\d+\.\d+\.\d+) \(\d{4}-\d{2}-\d{2}\)\s*$", re.MULTILINE)
OVERRIDE = re.compile(r"^Changelog-Fix:\s*(\S.*)$", re.MULTILINE)


def git(repo: Path, *args: str) -> str | None:
    result = subprocess.run(
        ["git", "-C", str(repo), *args], check=False, capture_output=True, text=True
    )
    return result.stdout if result.returncode == 0 else None


def file_at(repo: Path, rev: str, path: str) -> str | None:
    return git(repo, "show", f"{rev}:{path}")


def override_reason(repo: Path, base: str | None) -> str | None:
    """The reason given by a `Changelog-Fix:` line in a commit message in BASE..HEAD."""
    rng = f"{base}..HEAD" if base else "HEAD^!"
    log = git(repo, "log", "--format=%B%x00", rng) or ""
    match = OVERRIDE.search(log)
    return match.group(1).strip() if match else None


def changed_sections(repo: Path, text: str, base: str | None, path: str) -> list[str]:
    """Released versions whose section in `text` matches neither the tag's copy nor the base's."""
    base_text = file_at(repo, base, path) if base else None
    changed = []
    for version in HEADING.findall(text):
        tag_text = file_at(repo, f"v{version}", path)
        if tag_text is None:
            continue  # not released yet (the release PR itself), or no tag
        current = section(text, version)
        allowed = [section(tag_text, version)]
        if base_text is not None:
            allowed.append(section(base_text, version))
        if current not in allowed:
            changed.append(version)
    return changed


def main(argv: list[str] | None = None) -> int:
    parser = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    parser.add_argument(
        "--base", help="the revision this change applies to (e.g. origin/main); its copy is allowed"
    )
    parser.add_argument("--repo", type=Path, default=Path.cwd(), help="the git repository")
    parser.add_argument("--changelog", default="CHANGELOG.md", help="path within the repository")
    args = parser.parse_args(argv)

    text = (args.repo / args.changelog).read_text()
    changed = changed_sections(args.repo, text, args.base, args.changelog)
    if not changed:
        print(f"{args.changelog}: no released section changed", file=sys.stderr)
        return 0
    reason = override_reason(args.repo, args.base)
    if reason:
        names = ", ".join(changed)
        print(
            f"{args.changelog}: released sections {names} changed (Changelog-Fix: {reason})",
            file=sys.stderr,
        )
        return 0
    base_text = file_at(args.repo, args.base, args.changelog) if args.base else None
    for version in changed:
        before = base_text or file_at(args.repo, f"v{version}", args.changelog)
        released = (section(before, version) or "").splitlines()
        now = (section(text, version) or "").splitlines()
        added = [line for line in now if line not in released]
        print(
            f"{args.changelog}: '## {version}' was released as v{version}, and this change edits it.",
            file=sys.stderr,
        )
        if added:
            print("Lines this change adds to it:", file=sys.stderr)
            for line in added:
                print(f"  {line}", file=sys.stderr)
        print(
            "If these are this change's notes, move these bullets under ## Pending.",
            file=sys.stderr,
        )
    print(
        f"To fix released notes on purpose, add a 'Changelog-Fix: <reason>' line to a commit "
        f"message (compare: git show v{changed[0]}:{args.changelog}).",
        file=sys.stderr,
    )
    return 1


if __name__ == "__main__":
    sys.exit(main())
