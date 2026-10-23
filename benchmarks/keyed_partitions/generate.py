"""Write a sensor that decides which of N retailers changed, and a two-asset chain over them.

    python generate.py N DEST [keyed|partitioned] [CRON]

keyed (the default): one unpartitioned sensor returns {retailer: version} for every retailer,
the way one `select retailer, max(updated_at) ... group by retailer` would, and
`partitions_from(sensor)` gives `cleaned` and `scored` one key per retailer. Each key's run
hash folds in its own version only.

partitioned: the approach that works without keyed sources: the sensor is partitioned by
retailer and runs once per key on every tick.

With CRON, the sensor has `freshness=Schedule(CRON)`, for `barca serve`.

`versions.json` in DEST holds the versions; `bump.py` in DEST changes one retailer's version.
"""

import json
import pathlib
import sys

n = int(sys.argv[1])
out = pathlib.Path(sys.argv[2])
mode = sys.argv[3] if len(sys.argv) > 3 else "keyed"
cron = sys.argv[4] if len(sys.argv) > 4 else None
fresh = f'freshness=Schedule("{cron}")' if cron else ""
out.mkdir(parents=True, exist_ok=True)
(out / "versions.json").write_text(json.dumps({f"r{i:06d}": 0 for i in range(n)}))
(out / "bump.py").write_text(
    "import json, pathlib, sys\n"
    "p = pathlib.Path(__file__).with_name('versions.json')\n"
    "v = json.loads(p.read_text())\n"
    "key = sys.argv[1] if len(sys.argv) > 1 else 'r000000'\n"
    "v[key] = v.get(key, 0) + 1\n"
    "p.write_text(json.dumps(v))\n"
)

CHAIN = """

@asset(partitions={"retailer": partitions_from(versions)})
def cleaned(retailer: str, versions: int) -> dict:
    return {"retailer": retailer, "version": versions}


@asset(partitions={"retailer": partitions_from(cleaned)})
def scored(retailer: str, cleaned: dict) -> dict:
    return {**cleaned, "score": len(retailer)}
"""

if mode == "keyed":
    sensor = f"""import json
import pathlib

from barca import Schedule, asset, partitions_from, sensor

VERSIONS = pathlib.Path(__file__).with_name("versions.json")


@sensor({fresh})
def versions() -> tuple[bool, dict]:
    return True, json.loads(VERSIONS.read_text())
"""
else:
    sensor = f"""import json
import pathlib

from barca import Schedule, asset, partitions, partitions_from, sensor

VERSIONS = pathlib.Path(__file__).with_name("versions.json")
RETAILERS = [f"r{{i:06d}}" for i in range({n})]
_cache = {{}}


def _all():
    # One read per worker process, not per key: the cheapest a per-key sensor can be.
    if "v" not in _cache:
        _cache["v"] = json.loads(VERSIONS.read_text())
    return _cache["v"]


@sensor(partitions={{"retailer": partitions(RETAILERS)}}{", " + fresh if fresh else ""})
def versions(retailer: str) -> tuple[bool, int]:
    return True, _all()[retailer]
"""
(out / "pipeline.py").write_text(sensor + CHAIN)
