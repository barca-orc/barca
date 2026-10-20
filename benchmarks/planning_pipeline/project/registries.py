"""Registry sources, each loaded from the first of four tiers that has it:

1. a CSV whose path is in `<NAME>_REGISTRY_CSV`;
2. the registry API of another application, when `REGISTRY_API_URL` and `REGISTRY_API_KEY`
   are set and `REGISTRY_API` is not `0`. Here the API is a stub: a local JSON file
   `{"version": n, "registries": {name: [rows]}}` named by a `file:` URL. The response is cached
   once per worker process, and written to `REGISTRY_CACHE_DIR` as JSONL when that is set;
3. a published parquet under the source root, if it exists;
4. an empty relation with the registry's columns.

None of these variables is declared to barca, so switching tier does not change a run hash:
operators force a re-read with `barca run <validators> assets.py --refresh <registries>`.
"""

import json
import os
from pathlib import Path

import engine
import pyarrow as pa
import schemas
import sources
from settings import Settings

_API_CACHE: dict = {}

_ARROW = {"BIGINT": pa.int64(), "DOUBLE": pa.float64(), "VARCHAR": pa.string()}


def _typed(rows: list[dict]):
    schema = pa.schema([(c, _ARROW[t]) for c, t in schemas.REGISTRY.items()])
    table = pa.Table.from_pylist([{c: r.get(c) for c in schema.names} for r in rows], schema)
    return engine.con().from_arrow(table)


def _api_payload() -> dict | None:
    if "payload" in _API_CACHE:
        return _API_CACHE["payload"]
    url, key = Settings.REGISTRY_API_URL, Settings.REGISTRY_API_KEY
    if not url or not key or os.environ.get("REGISTRY_API", "1") == "0":
        payload = None
    else:
        path = url.removeprefix("file://").removeprefix("file:")
        if not Path(path).is_absolute():
            path = str(Settings.PROJECT_DIR / path)
        payload = json.loads(Path(path).read_text())
        cache_dir = os.environ.get("REGISTRY_CACHE_DIR")
        if cache_dir:
            Path(cache_dir).mkdir(parents=True, exist_ok=True)
            for name, rows in payload.get("registries", {}).items():
                out = Path(cache_dir) / f"{name}-v{payload.get('version', 0)}.jsonl"
                out.write_text("".join(json.dumps(r) + "\n" for r in rows))
    _API_CACHE["payload"] = payload
    return payload


def load(name: str):
    csv = os.environ.get(f"{name.upper()}_REGISTRY_CSV")
    if csv:
        if not Path(csv).is_absolute():
            csv = str(Settings.PROJECT_DIR / csv)
        return engine.con().read_csv(csv, columns=schemas.REGISTRY, header=True)
    payload = _api_payload()
    if payload is not None and name in payload.get("registries", {}):
        return _typed(payload["registries"][name])
    published = sources.uri(f"registries/{name}.parquet")
    if sources.exists(published):
        return sources.read(f"registries/{name}.parquet")
    return engine.empty(schemas.REGISTRY)
