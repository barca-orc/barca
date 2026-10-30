#!/usr/bin/env python3
"""Select CI from PR metadata or the checked-out merge-group commit."""

import json
import os
import subprocess


def tier(event, head_ref, labels, message):
    if event == "pull_request":
        return "full" if head_ref.startswith("release/v") or "ci-full" in labels else "fast"
    if event == "merge_group":
        return "full" if "chore(release)" in message else "merge"
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
    print(
        tier(
            event,
            os.environ.get("CI_HEAD_REF", ""),
            json.loads(os.environ.get("CI_LABELS", "[]")) or [],
            message,
        )
    )


if __name__ == "__main__":
    main()
