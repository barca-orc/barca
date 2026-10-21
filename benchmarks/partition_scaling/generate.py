"""Write a three-level partition chain over N keys: fetch(key) -> normalize(key) -> score(key).

With FAIL_HALF=1 in the environment, fetch raises for every odd key, so half the keys fail and
their normalize and score keys are skipped (#461).
"""

import os
import pathlib
import sys

n = int(sys.argv[1])
out = pathlib.Path(sys.argv[2])
fail = (
    '    if int(key[1:]) % 2:\n        raise ValueError(f"{key} fails on purpose")\n'
    if os.environ.get("FAIL_HALF") == "1"
    else ""
)
out.mkdir(parents=True, exist_ok=True)
(out / "assets.py").write_text(f'''from barca import asset, partitions

KEYS = [f"k{{i:05d}}" for i in range({n})]


@asset(partitions={{"key": partitions(KEYS)}})
def fetch(key: str) -> dict:
{fail}    return {{"key": key, "v": len(key)}}


@asset(inputs={{"data": fetch}}, partitions={{"key": partitions(KEYS)}})
def normalize(data: dict, key: str) -> dict:
    return {{"key": key, "v": data["v"] * 2}}


@asset(inputs={{"data": normalize}}, partitions={{"key": partitions(KEYS)}})
def score(data: dict, key: str) -> dict:
    return {{"key": key, "v": data["v"] + 1}}
''')
