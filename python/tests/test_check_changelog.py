"""scripts/check-changelog.py: a change may not edit the notes of a released version.

A branch cut before a release can merge with its CHANGELOG.md bullets inside the section that
release just shipped, because git applies the hunk by its context (#632's notes landed in
0.30.1). Each test builds a small git repository and runs the check against a base revision.
"""

import importlib.util
import subprocess
from pathlib import Path

import pytest

SPEC = importlib.util.spec_from_file_location(
    "check_changelog", Path(__file__).parents[2] / "scripts" / "check-changelog.py"
)
CHECK = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(CHECK)

RELEASED = """# Changelog

## Pending

## 0.1.0 (2026-10-01)

### Features

- The first feature.
"""


def git(repo: Path, *args: str) -> str:
    return subprocess.run(
        ["git", "-C", str(repo), "-c", "user.name=t", "-c", "user.email=t@example.com", *args],
        check=True,
        capture_output=True,
        text=True,
    ).stdout.strip()


def commit(repo: Path, text: str, message: str = "change") -> str:
    (repo / "CHANGELOG.md").write_text(text)
    git(repo, "add", "CHANGELOG.md")
    git(repo, "commit", "-q", "-m", message)
    return git(repo, "rev-parse", "HEAD")


@pytest.fixture
def repo(tmp_path):
    """A repository whose main has released 0.1.0 (tag v0.1.0); returns (path, base sha)."""
    git(tmp_path, "init", "-q", "-b", "main")
    base = commit(tmp_path, RELEASED, "release 0.1.0")
    git(tmp_path, "tag", "v0.1.0")
    return tmp_path, base


def run(repo: Path, base: str, capsys) -> tuple[int, str]:
    code = CHECK.main(["--repo", str(repo), "--base", base])
    return code, capsys.readouterr().err


def test_an_unchanged_released_section_passes(repo, capsys):
    path, base = repo
    commit(path, RELEASED.replace("## Pending\n", "## Pending\n\n### Features\n\n- New thing.\n"))
    code, err = run(path, base, capsys)
    assert code == 0, err


def test_a_bullet_added_to_a_released_section_fails(repo, capsys):
    path, base = repo
    commit(path, RELEASED + "- Misplaced by a stale branch (#632).\n")
    code, err = run(path, base, capsys)
    assert code == 1
    assert "'## 0.1.0' was released as v0.1.0" in err
    assert "- Misplaced by a stale branch (#632)." in err
    assert "move these bullets under ## Pending" in err
    assert "Changelog-Fix:" in err


def test_the_release_pr_section_without_a_tag_passes(repo, capsys):
    """The release PR renames ## Pending to ## 0.2.0 before v0.2.0 exists."""
    path, base = repo
    pending = "## Pending\n\n### Bug Fixes\n\n- A fix.\n"
    base = commit(path, RELEASED.replace("## Pending\n", pending))
    commit(
        path,
        RELEASED.replace("## Pending\n", "## 0.2.0 (2026-10-02)\n\n### Bug Fixes\n\n- A fix.\n"),
    )
    code, err = run(path, base, capsys)
    assert code == 0, err


def test_a_changelog_fix_trailer_allows_editing_released_notes(repo, capsys):
    path, base = repo
    commit(
        path,
        RELEASED.replace("first", "first, spelled right,"),
        "doc(changelog): fix a typo\n\nChangelog-Fix: typo in 0.1.0",
    )
    code, err = run(path, base, capsys)
    assert code == 0, err
    assert "Changelog-Fix: typo in 0.1.0" in err


def test_a_section_already_fixed_on_the_base_passes(repo, capsys):
    """Once a deliberate fix lands, later changes that leave the section alone still pass."""
    path, _ = repo
    fixed = RELEASED + "- Moved here on purpose.\n"
    base = commit(path, fixed, "fix notes\n\nChangelog-Fix: moved bullets")
    commit(path, fixed.replace("## Pending\n", "## Pending\n\n- Another change.\n"))
    code, err = run(path, base, capsys)
    assert code == 0, err
