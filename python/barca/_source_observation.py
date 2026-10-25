"""Compare producer-owned JSON snapshots without copying the full source into Rust."""

import hashlib
import json
import math
from pathlib import Path


def _same(left, right):
    if type(left) is not type(right):
        return False
    if isinstance(left, dict):
        return left.keys() == right.keys() and all(_same(v, right[k]) for k, v in left.items())
    if isinstance(left, list):
        return len(left) == len(right) and all(_same(a, b) for a, b in zip(left, right))
    if isinstance(left, float) and left == right == 0:
        return math.copysign(1, left) == math.copysign(1, right)
    return left == right


def _membership(keys):
    digest = hashlib.sha256()
    for key in keys:
        raw = key.encode("utf-8")
        digest.update(len(raw).to_bytes(8, "big"))
        digest.update(raw)
    return digest.hexdigest()


def observe(value, previous):
    """The previous snapshot is evidence only when its bytes match the recorded digest."""
    keyed = isinstance(value, dict)
    keys = list(value) if keyed else [str(k) for k in value if type(k) in (str, int, float)]
    summary = {"count": len(keys), "keyed": keyed, "membership_hash": _membership(keys)}
    if not keyed or not previous or not previous.get("content_hash"):
        return summary
    try:
        raw = Path(previous["path"]).read_bytes()
        if hashlib.sha256(raw).hexdigest() != previous["content_hash"]:
            return summary
        old = json.loads(raw)
    except (OSError, ValueError, TypeError):
        return summary
    if not isinstance(old, dict):
        return summary
    changed = {k: v for k, v in value.items() if k not in old or not _same(old[k], v)}
    summary.update(
        previous_hash=previous["content_hash"],
        changed=changed,
        changed_order=list(changed),
        removed=[k for k in old if k not in value],
    )
    return summary
