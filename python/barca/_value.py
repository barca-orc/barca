"""Artifact values for `barca get -o value` — invoked by Rust as `python -m barca._value`.

Reads each artifact with the reader `barca.get()` uses (`_artifacts.deserialize`) and turns it
into JSON: a table (parquet, CSV, or a pickled DataFrame) becomes a list of row objects, dates
and times become ISO strings, NaN and infinities become null, and an object JSON has no type for
becomes its `repr`. Unpickling runs the pickled classes' code, exactly as `barca.get()` does.

Protocol: one JSON document on stdin, {"artifacts": [{"path", "format"}, ...]}; one JSON array
on stdout with {"value": ...} or {"error": "..."} per artifact, in order.

`barca get -o url` sends a sign request instead (version 1, specs/worker-protocol.md):
{"op": "sign", "version": 1, "objects": [{"uri", "expires", "filename"}, ...]}, answered with
{"url", "expires_at"} (and a "note" when the link ends sooner than asked) or {"error", "kind"}
per object, in order. A signed URL is written to
stdout only, for the coordinator; errors never contain one.
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


SIGN_VERSION = 1


def sign(obj: dict) -> dict:
    """One object as {"url", "expires_at"}, or {"error", "kind"}. Never raises."""
    from barca import _storage

    try:
        url, expires_at = _storage.sign_url(obj["uri"], obj["expires"], obj.get("filename"))
    except _storage.SignError as e:
        return {"error": str(e), "kind": e.kind}
    except Exception as e:  # noqa: BLE001 — reported per object, as the coordinator decides
        message = _storage.safe_error(f"{type(e).__name__}: {e}")
        return {"error": message, "kind": "store"}
    answer = {"url": url, "expires_at": expires_at.isoformat().replace("+00:00", "Z")}
    asked = datetime.datetime.now(datetime.timezone.utc) + datetime.timedelta(
        seconds=obj["expires"]
    )
    if expires_at < asked - datetime.timedelta(seconds=60):
        answer["note"] = (
            f"the credentials that signed it expire at {answer['expires_at']}, so the link "
            "stops working then, sooner than --expires asked"
        )
    return answer


def main() -> None:
    request = json.load(sys.stdin)
    if request.get("op") == "sign":
        if request.get("version") != SIGN_VERSION:
            raise SystemExit(
                f"barca._value: sign request version {request.get('version')!r} is not "
                f"{SIGN_VERSION}; reinstall matching barca packages"
            )
        answers = [sign(o) for o in request.get("objects", [])]
    else:
        answers = [value(a["path"], a["format"]) for a in request.get("artifacts", [])]
    json.dump(answers, sys.stdout)


if __name__ == "__main__":
    main()
