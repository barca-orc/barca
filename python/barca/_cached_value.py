"""Read cached values without invoking pickle reconstruction or project imports."""

import errno
import os
import pickle
import pickletools
import shutil
import tempfile
from contextlib import contextmanager
from pathlib import Path
from typing import Any

from barca import _artifacts


class UnsupportedValue(ValueError):
    """The representation requires executable pickle reconstruction."""


# Even a reducer in an already imported standard-library module can execute project code.
# Accept only pickle's data opcodes; never resolve globals or call reconstruction hooks.
_EXECUTABLE = frozenset(
    {
        "GLOBAL",
        "STACK_GLOBAL",
        "REDUCE",
        "BUILD",
        "NEWOBJ",
        "NEWOBJ_EX",
        "INST",
        "OBJ",
        "EXT1",
        "EXT2",
        "EXT4",
        "PERSID",
        "BINPERSID",
    }
)


def deserialize(path: str, fmt: str, *, frame_type: str | None = None) -> Any:
    if fmt != "pickle":
        return _artifacts.deserialize(path, fmt, frame_type=frame_type)
    data = Path(path).read_bytes()
    for opcode, _, _ in pickletools.genops(data):
        if opcode.name in _EXECUTABLE:
            raise UnsupportedValue(
                "cached-only value access cannot reconstruct this pickle without executing code "
                f"({opcode.name}); request its artifact path, or store JSON/parquet/CSV instead"
            )
    return pickle.loads(data)


@contextmanager
def retain_reference(path: str):
    """Keep a CLI reference alive throughout a local Python value read."""
    source = Path(path)
    # Cached-only CLI pointers live in .barca/sql-cache/cached-*/. Pin them in
    # process-owned staging, which GC retains while this Python process lives.
    cache = source.parent.parent
    if cache.name != "sql-cache" or cache.parent.name != ".barca":
        raise ValueError("cached-only returned an invalid artifact reference")
    staging = cache.parent / "staging" / str(os.getpid())
    staging.mkdir(parents=True, exist_ok=True)
    with tempfile.TemporaryDirectory(prefix="cached-", dir=staging) as directory:
        retained = Path(directory) / source.name
        try:
            os.link(source, retained)
        except OSError as exc:
            if exc.errno != errno.EXDEV:
                raise
            with source.open("rb") as reader, retained.open("xb") as writer:
                shutil.copyfileobj(reader, writer)
        yield str(retained)
