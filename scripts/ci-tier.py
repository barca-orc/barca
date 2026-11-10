#!/usr/bin/env python3
"""Select CI from PR metadata or the checked-out merge-group commit."""

import json
import os
import subprocess

# A diff only of these is a docs-site change (the site, the changelog its Release Notes page
# renders, and the site's deploy workflow).
SITE_PREFIXES = ("site/",)
SITE_FILES = {"CHANGELOG.md", ".github/workflows/site.yml"}
# Site pages a Python test executes (python/tests/test_docs_examples.py): changing one runs the
# normal tiers. Every other site page the tests read is read by the Rust tests, which the
# site-merge tier runs.
SITE_PYTHON_TESTED = {"site/src/content/docs/recipes/ordering-only-deps.md"}


def site_only(files):
    return (
        any(p.startswith(SITE_PREFIXES) for p in files)
        and all(p.startswith(SITE_PREFIXES) or p in SITE_FILES for p in files)
        and not any(p in SITE_PYTHON_TESTED for p in files)
    )


def tier(event, head_ref, labels, message, files=None):
    if event not in ("pull_request", "merge_group"):
        raise ValueError(f"unsupported CI event: {event}")
    if head_ref.startswith("release/v") or "ci-full" in labels or "chore(release)" in message:
        return "full"
    if (
        files
        and any(p.startswith("ui/") for p in files)
        and all(p.startswith("ui/") or p == "CHANGELOG.md" for p in files)
    ):
        return "ui"
    if files and site_only(files):
        return "site" if event == "pull_request" else "site-merge"
    if event == "pull_request":
        return "fast"
    if event == "merge_group":
        return "merge"
    raise ValueError(f"unsupported CI event: {event}")


def main():
    event = os.environ["GITHUB_EVENT_NAME"]
    # merge_group payloads do not reliably expose head_commit.message in the provider's
    # expression context. Checkout already points at the exact proposed main commit.
    message = (
        subprocess.check_output(["git", "log", "-1", "--format=%B"], text=True)
        if event == "merge_group"
        else ""
    )
    files = None
    base = os.environ.get("CI_BASE_SHA")
    if base:
        try:
            files = (
                subprocess.check_output(
                    ["git", "diff", "--name-only", "--no-renames", "-z", f"{base}...HEAD"],
                    text=True,
                )
                .rstrip("\0")
                .split("\0")
            )
        except subprocess.CalledProcessError:
            pass  # Missing history must retain the broader existing gates.
    print(
        tier(
            event,
            os.environ.get("CI_HEAD_REF", ""),
            json.loads(os.environ.get("CI_LABELS") or "[]") or [],
            message,
            files,
        )
    )


if __name__ == "__main__":
    main()
