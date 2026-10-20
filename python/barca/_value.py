"""Artifact values for `barca get -o value` — invoked by Rust as `python -m barca._value`.

Reads each artifact with the reader `barca.get()` uses (`_artifacts.deserialize`) and turns it
into JSON: a table (parquet, CSV, or a pickled DataFrame) becomes a list of row objects, dates
and times become ISO strings, NaN and infinities become null, and an object JSON has no type for
becomes its `repr`. Unpickling runs the pickled classes' code, exactly as `barca.get()` does.

Protocol: one JSON document on stdin, {"artifacts": [{"path", "format"}, ...]}; one JSON array
on stdout with {"value": ...} or {"error": "..."} per artifact, in order.
"""

import base64
import datetime
import decimal
import json
import math
import sys
from typing import Any

from barca import _artifacts


def _frame_type(fmt: str) -> "str | None":
    """pyarrow keeps a table's column types without pandas; fall back to the default reader."""
    if fmt not in ("parquet", "csv"):
        return None
    try:
        import pyarrow  # noqa: F401
    except ImportError:
        return None
    return "pyarrow"


def _plain(value: Any) -> Any:
    """`value` as plain JSON data."""
    if value is None or isinstance(value, (bool, int, str)):
        return value
    if isinstance(value, float):
        return value if math.isfinite(value) else None
    if isinstance(value, dict):
        return {str(k): _plain(v) for k, v in value.items()}
    if isinstance(value, (list, tuple, set, frozenset)):
        return [_plain(v) for v in value]
    if isinstance(value, (datetime.datetime, datetime.date, datetime.time)):
        return value.isoformat()
    if isinstance(value, datetime.timedelta):
        return value.total_seconds()
    if isinstance(value, decimal.Decimal):
        return str(value)
    if isinstance(value, bytes):
        return base64.b64encode(value).decode("ascii")
    # Tables: pyarrow, polars, pandas (checked by method, so none of them is imported here).
    if hasattr(value, "to_pylist"):
        return _plain(value.to_pylist())
    if hasattr(value, "to_dicts"):
        return _plain(value.to_dicts())
    if hasattr(value, "to_dict") and hasattr(value, "columns"):
        return _plain(value.to_dict(orient="records"))
    if hasattr(value, "isoformat"):  # pandas Timestamp
        return value.isoformat()
    if hasattr(value, "tolist"):  # numpy arrays and scalars, pandas Series
        return _plain(value.tolist())
    if hasattr(value, "item"):
        return _plain(value.item())
    return repr(value)


def value(path: str, fmt: str) -> dict:
    """One artifact as {"value": ...}, or {"error": ...}. Never raises."""
    try:
        return {"value": _plain(_artifacts.deserialize(path, fmt, frame_type=_frame_type(fmt)))}
    except Exception as e:  # noqa: BLE001 — reported per artifact, never fails the command
        first = str(e).splitlines()[0] if str(e) else ""
        return {"error": f"{type(e).__name__}: {first}".rstrip(": ")}


def main() -> None:
    request = json.load(sys.stdin)
    answers = [value(a["path"], a["format"]) for a in request.get("artifacts", [])]
    json.dump(answers, sys.stdout)


if __name__ == "__main__":
    main()
