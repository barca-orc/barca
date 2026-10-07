"""Artifact shape for `barca status` — invoked by Rust as `python -m barca._inspect`.

Reads only artifacts, never user code:

  - parquet: row count and column schema from the file footer (needs pyarrow; without it the
    shape says so in `note`). `sample` reads just the first rows.
  - json:    the value's type; for a list, its length and (for a list of objects) the column
    names with the JSON types seen; for an object, its keys.
  - pickle:  the type of the top-level object, read from the pickle opcodes WITHOUT unpickling,
    so no module is imported and no code runs. Pickles are never sampled.

A remote artifact (s3://, gs://, abfs://) is read through the fsspec filesystem the workers use
(`_storage.get_fs`): a parquet footer is a few ranged reads, so the object is never downloaded;
json and pickle need their bytes and are downloaded only up to MAX_REMOTE_BYTES. Remote
artifacts are read concurrently, and once a store has failed (credentials, network) the
artifacts not yet read report that failure instead of each waiting out the driver's retries.

Protocol: one JSON document on stdin, {"sample": N, "artifacts": [{"path", "format"}, ...]};
one JSON array on stdout with a shape object per artifact, in order. Optional
`fields: true` includes JSON object field types and list item types for the UI.
"""

import io
import json
import os
import pickletools
import sys
from concurrent.futures import ThreadPoolExecutor
from typing import IO, Any

from barca import _storage

MAX_KEYS = 100
# A remote json or pickle artifact larger than this is not downloaded to describe it.
MAX_REMOTE_BYTES = 16 * 1024 * 1024
# Read-ahead for a remote parquet file. The drivers' defaults (50 MB for s3fs) would turn a
# sample of a few rows into a download of most of the object.
REMOTE_BLOCK_BYTES = 1024 * 1024
# Remote artifacts read at once.
MAX_REMOTE_READERS = 8
# scheme -> the first failure to reach that store. Later reads report it without retrying, so
# a status over many nodes waits out the driver's retries once, not once per node.
_store_down: dict[str, str] = {}


def shape(path: str, fmt: str, sample: int = 0, fields: bool = False) -> dict:
    """Shape of one artifact. Never raises: problems are reported in `note`."""
    try:
        if _storage.is_remote(path):
            return _remote_shape(path, fmt, sample, fields)
        if not os.path.exists(path):
            return {"note": "artifact file not found"}
        if fmt == "parquet":
            return _parquet_shape(path, sample)
        if fmt == "json":
            with open(path, "rb") as f:
                return _json_shape(f, sample, fields)
        if fmt == "pickle":
            with open(path, "rb") as f:
                return {"type": _pickle_type(f)}
        return {"note": f"unknown format '{fmt}'"}
    except Exception as e:  # noqa: BLE001 — report, never fail the status command
        where = "remote artifact" if _storage.is_remote(path) else "artifact"
        return {"note": f"could not read {where}: {type(e).__name__}: {_one_line(e)}"}


def _one_line(e: BaseException) -> str:
    lines = str(e).strip().splitlines()
    return lines[0] if lines else ""


# ─── remote ───────────────────────────────────────────────────────────────────


def _remote_shape(path: str, fmt: str, sample: int, fields: bool = False) -> dict:
    """Shape of an artifact in an object store. Raises; `shape` turns that into a note."""
    if fmt not in ("parquet", "json", "pickle"):
        return {"note": f"unknown format '{fmt}'"}
    if fmt == "parquet":
        try:
            import pyarrow.parquet  # noqa: F401 — checked before any network call
        except ImportError:
            return {"note": _NO_PYARROW}
    try:
        fs = _storage.get_fs(path)
    except (ImportError, ValueError) as e:  # missing driver, bad BARCA_STORAGE_OPTIONS
        return {"note": f"could not read remote artifact: {_one_line(e)}"}
    scheme = path.split("://", 1)[0].lower()
    if scheme in _store_down:
        return {
            "note": f"could not read remote artifact: {_store_down[scheme]} "
            "(the failure an earlier artifact hit; not retried)"
        }
    try:
        if fmt == "parquet":
            # Footer (and, for a sample, the first rows) by ranged reads: no full download.
            with fs.open(path, "rb", block_size=REMOTE_BLOCK_BYTES) as f:
                return _parquet_shape(f, sample)
        size = int(fs.size(path))
        if size > MAX_REMOTE_BYTES:
            return {
                "note": f"remote {fmt} artifact too large to inspect: "
                f"{_mb(size)} MB (limit {_mb(MAX_REMOTE_BYTES)} MB)"
            }
        data = io.BytesIO(fs.cat_file(path))
    except FileNotFoundError:
        return {"note": "artifact file not found"}
    except Exception as e:
        if not type(e).__module__.startswith("pyarrow"):  # the store, not this file's content
            _store_down.setdefault(scheme, f"{type(e).__name__}: {_one_line(e)}")
        raise
    if fmt == "json":
        return _json_shape(data, sample, fields)
    return {"type": _pickle_type(data)}


def _mb(n: int) -> str:
    return f"{n / (1024 * 1024):.1f}".removesuffix(".0")


# ─── parquet ──────────────────────────────────────────────────────────────────


_NO_PYARROW = "pyarrow is not installed; install barca[parquet] to read parquet shape"


def _parquet_shape(source: "str | IO[bytes]", sample: int) -> dict:
    """`source` is a local path or an open, seekable binary file."""
    try:
        import pyarrow.parquet as pq
    except ImportError:
        return {"note": _NO_PYARROW}
    pf = pq.ParquetFile(source)
    schema = pf.schema_arrow
    out: dict[str, Any] = {
        "type": "table",
        "rows": pf.metadata.num_rows,
        "columns": [{"name": f.name, "type": str(f.type)} for f in schema],
    }
    if sample > 0:
        rows: list = []
        # One row group at a time: asked for the whole file, pyarrow (25 and later) reads ahead
        # through every row group, which for a remote artifact is the whole object.
        for group in range(pf.num_row_groups):
            for batch in pf.iter_batches(batch_size=sample, row_groups=[group]):
                rows.extend(batch.to_pylist())
                if len(rows) >= sample:
                    break
            if len(rows) >= sample:
                break
        out["sample"] = _jsonable(rows[:sample])
    return out


def _jsonable(value: Any) -> Any:
    """Round-trip through json so dates, decimals and bytes print as strings."""
    return json.loads(json.dumps(value, default=str))


# ─── json ─────────────────────────────────────────────────────────────────────


def _json_type(v: Any) -> str:
    if v is None:
        return "null"
    return type(v).__name__


def _json_shape(f: IO[bytes], sample: int, fields: bool = False) -> dict:
    value = json.load(f)
    out: dict[str, Any] = {"type": _json_type(value)}
    if isinstance(value, list):
        out["rows"] = len(value)
        if fields:
            out["item_types"] = sorted({_json_type(v) for v in value})
        if value and all(isinstance(r, dict) for r in value):
            seen: dict[str, list[str]] = {}
            for row in value:
                for k, v in row.items():
                    types = seen.setdefault(k, [])
                    t = _json_type(v)
                    if t not in types:
                        types.append(t)
            out["columns"] = [
                {"name": k, "type": " | ".join(sorted(ts, key=lambda t: t == "null"))}
                for k, ts in seen.items()
            ]
        if sample > 0:
            out["sample"] = value[:sample]
    elif isinstance(value, dict):
        keys = list(value)
        out["keys"] = keys[:MAX_KEYS]
        if fields:
            out["columns"] = [{"name": k, "type": _json_type(value[k])} for k in keys[:MAX_KEYS]]
        if len(keys) > MAX_KEYS:
            out["key_count"] = len(keys)
            if fields:
                out["note"] = f"Showing the first {MAX_KEYS} of {len(keys)} fields"
        if sample > 0:
            out["sample"] = {k: value[k] for k in keys[:sample]}
    elif sample > 0:
        out["sample"] = value
    return out


# ─── pickle ───────────────────────────────────────────────────────────────────

# Opcodes that push a value of a known builtin type.
_PUSH_TYPES = {
    "EMPTY_DICT": "dict",
    "DICT": "dict",
    "EMPTY_LIST": "list",
    "LIST": "list",
    "EMPTY_SET": "set",
    "FROZENSET": "frozenset",
    "EMPTY_TUPLE": "tuple",
    "TUPLE": "tuple",
    "TUPLE1": "tuple",
    "TUPLE2": "tuple",
    "TUPLE3": "tuple",
    "NONE": "NoneType",
    "NEWTRUE": "bool",
    "NEWFALSE": "bool",
    "INT": "int",
    "BININT": "int",
    "BININT1": "int",
    "BININT2": "int",
    "LONG": "int",
    "LONG1": "int",
    "LONG4": "int",
    "FLOAT": "float",
    "BINFLOAT": "float",
    "STRING": "str",
    "BINSTRING": "str",
    "SHORT_BINSTRING": "str",
    "UNICODE": "str",
    "BINUNICODE": "str",
    "SHORT_BINUNICODE": "str",
    "BINUNICODE8": "str",
    "BINBYTES": "bytes",
    "SHORT_BINBYTES": "bytes",
    "BINBYTES8": "bytes",
    "BYTEARRAY8": "bytearray",
}


class _Class:
    """A class reference seen in the opcode stream (GLOBAL / STACK_GLOBAL)."""

    def __init__(self, name: str):
        self.name = name


class _Str:
    def __init__(self, value: str):
        self.value = value


class _Tuple:
    def __init__(self, items: list):
        self.items = items


class _Mark:
    pass


# Opcodes that mutate the object below their operands and leave it on the stack.
_MUTATORS = {"APPEND", "APPENDS", "SETITEM", "SETITEMS", "ADDITEMS", "BUILD"}


def _label(item: Any) -> str:
    if isinstance(item, _Class):
        return item.name
    if isinstance(item, _Tuple):
        return "tuple"
    if isinstance(item, _Str):
        return "str"
    if isinstance(item, str):
        return item
    return "unknown"


def _pickle_type(f: IO[bytes]) -> str:
    """Type of the top-level object, by simulating the pickle VM's stack over type labels."""
    mark = pickletools.markobject
    stack: list = []
    memo: dict = {}
    for op, arg, _pos in pickletools.genops(f):
        name = op.name
        if name in ("PROTO", "FRAME"):
            continue
        if name == "STOP":
            return _label(stack[-1]) if stack else "unknown"
        if name == "MARK":
            stack.append(_Mark())
        elif name == "MEMOIZE":
            memo[len(memo)] = stack[-1]
        elif name in ("PUT", "BINPUT", "LONG_BINPUT"):
            memo[arg] = stack[-1]
        elif name in ("GET", "BINGET", "LONG_BINGET"):
            stack.append(memo.get(arg, "unknown"))
        elif name == "GLOBAL":
            module, _, qual = str(arg).partition(" ")
            stack.append(_Class(_qualname(module, qual)))
        elif name == "STACK_GLOBAL":
            qual, module = stack.pop(), stack.pop()
            stack.append(_Class(_qualname(_str_value(module), _str_value(qual))))
        elif name in ("SHORT_BINUNICODE", "BINUNICODE", "UNICODE", "BINUNICODE8"):
            stack.append(_Str(str(arg)))
        elif name in ("TUPLE1", "TUPLE2", "TUPLE3"):
            n = int(name[-1])
            items = stack[-n:]
            del stack[-n:]
            stack.append(_Tuple(items))
        elif name == "TUPLE":
            stack.append(_Tuple(_pop_to_mark(stack)))
        elif name == "NEWOBJ":
            stack.pop()  # args
            stack.append(_label(stack.pop()))
        elif name == "NEWOBJ_EX":
            stack.pop()  # kwargs
            stack.pop()  # args
            stack.append(_label(stack.pop()))
        elif name == "REDUCE":
            args = stack.pop()
            stack.append(_reduce_label(stack.pop(), args))
        elif name == "OBJ":
            items = _pop_to_mark(stack)
            stack.append(_label(items[0]) if items else "unknown")
        elif name == "INST":
            _pop_to_mark(stack)
            module, _, qual = str(arg).partition(" ")
            stack.append(_qualname(module, qual))
        elif name in _MUTATORS:
            if mark in op.stack_before:
                _pop_to_mark(stack)
            else:
                for _ in op.stack_before[1:]:
                    stack.pop()
        else:
            # Generic opcode: pop its operands, push its results (typed when known).
            before = op.stack_before
            if mark in before:
                _pop_to_mark(stack)
                for _ in range(before.index(mark)):
                    stack.pop()
            else:
                for _ in before:
                    stack.pop()
            for _ in op.stack_after:
                stack.append(_PUSH_TYPES.get(name, "unknown"))
    return "unknown"


def _pop_to_mark(stack: list) -> list:
    items: list = []
    while stack:
        top = stack.pop()
        if isinstance(top, _Mark):
            break
        items.append(top)
    items.reverse()
    return items


def _str_value(item: Any) -> str:
    return item.value if isinstance(item, _Str) else "?"


def _qualname(module: str, qual: str) -> str:
    return qual if module in ("builtins", "__builtin__") else f"{module}.{qual}"


def _reduce_label(fn: Any, args: Any) -> str:
    """A REDUCE builds `fn(*args)`. Reconstructor helpers (copyreg._reconstructor, numpy's
    _reconstruct, copyreg.__newobj__) take the real class as their first argument."""
    name = _label(fn)
    short = name.rsplit(".", 1)[-1]
    if (
        (short.startswith("_reconstruct") or short == "__newobj__")
        and isinstance(args, _Tuple)
        and args.items
        and isinstance(args.items[0], _Class)
    ):
        return args.items[0].name
    if (
        name == "_codecs.encode"
        and isinstance(args, _Tuple)
        and len(args.items) >= 2
        and isinstance(args.items[0], _Str)
        and _str_value(args.items[1]) == "latin1"
    ):
        return "bytes"  # bytes reconstruction in pickle protocols 0–2
    if name.startswith("numpy.") and short == "_frombuffer":
        return "numpy.ndarray"  # protocol 5 arrays
    return name


def shapes(artifacts: list[dict], sample: int = 0, fields: bool = False) -> list[dict]:
    """Shape of each artifact, in order. Remote ones are read concurrently, so a status over
    many nodes is not one network round trip after another."""
    out: list[dict | None] = [None] * len(artifacts)
    remote: list[int] = []
    for i, a in enumerate(artifacts):
        if _storage.is_remote(a["path"]):
            remote.append(i)
        else:
            out[i] = shape(a["path"], a.get("format", ""), sample, fields)
    if remote:
        # Build each filesystem once, here: the per-protocol cache is not locked.
        for scheme in {artifacts[i]["path"].split("://", 1)[0] for i in remote}:
            try:
                _storage.get_fs(f"{scheme}://")
            except Exception:  # noqa: BLE001, S110 — each shape reports it in its own note
                pass
        with ThreadPoolExecutor(max_workers=min(MAX_REMOTE_READERS, len(remote))) as pool:
            done = pool.map(
                lambda i: shape(
                    artifacts[i]["path"], artifacts[i].get("format", ""), sample, fields
                ),
                remote,
            )
            for i, s in zip(remote, done):
                out[i] = s
    return [s if s is not None else {"note": "shape was not read"} for s in out]


def main() -> None:
    req = json.load(sys.stdin)
    sample = int(req.get("sample") or 0)
    sys.stdout.write(
        json.dumps(
            shapes(req.get("artifacts", []), sample, bool(req.get("fields", False))), default=str
        )
    )


if __name__ == "__main__":
    main()
