#!/usr/bin/env python3
"""Print the release notes for one version from CHANGELOG.md.

CHANGELOG.md is the source of release notes. The release workflow publishes the body of the
version's `## <version> (<date>)` section as the GitHub release body, followed by an install
line. A missing or empty section is an error (exit 1), so a tag cannot ship without notes.

    python3 scripts/changelog-section.py 0.26.1           # the section body
    python3 scripts/changelog-section.py v0.26.1 --install   # plus the "## Install" block
"""

import argparse
import re
import sys
from pathlib import Path

CHANGELOG = Path(__file__).resolve().parents[1] / "CHANGELOG.md"
INSTALL = "## Install\n\n```bash\nuv add barca\n```\n"


def section(text: str, version: str) -> str | None:
    heading = re.compile(rf"^## {re.escape(version)}(?: \(\d{{4}}-\d{{2}}-\d{{2}}\))?\s*$")
    lines = text.splitlines()
    for i, line in enumerate(lines):
        if heading.match(line):
            body = []
            for rest in lines[i + 1 :]:
                if rest.startswith("## "):
                    break
                body.append(rest)
            return "\n".join(body).strip() or None
    return None


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    parser.add_argument("version", help="the version, with or without a leading v")
    parser.add_argument("--install", action="store_true", help="append the install block")
    parser.add_argument("--changelog", type=Path, default=CHANGELOG, help="path to CHANGELOG.md")
    args = parser.parse_args()
    version = args.version.removeprefix("v")
    body = section(args.changelog.read_text(), version)
    if body is None:
        print(
            f"{args.changelog.name} has no non-empty '## {version} (<date>)' section; "
            "add the release notes there before tagging",
            file=sys.stderr,
        )
        return 1
    out = body + "\n"
    if args.install:
        out += "\n" + INSTALL
    sys.stdout.write(out)
    return 0


if __name__ == "__main__":
    sys.exit(main())
