"""Test only: pause one of barca's helper processes at a named point until the test lets go.

Put this directory on PYTHONPATH and set ``BARCA_TEST_HOLD=<point>:<dir>``. The first time a
barca Python process reaches ``<point>`` it writes ``<dir>/<point>.started`` (holding its pid)
and waits there until ``<dir>/release`` exists. A test can then act at a moment it controls:
wait for the marker, send a signal, look at what is on disk. Nothing in the package knows
about this; it works by wrapping functions of ``barca._storage`` from outside.

Points:

- ``put``     the transfer helper is about to upload an artifact (``_storage.put_file``)
- ``get``     the transfer helper is about to download one (``_storage.get_file``); its temp
              file exists already
- ``copied``  a copy into a directory store has written its temp file and not yet renamed it
- ``push``    the state helper is about to push the metadata DB (``python -m barca._state push``)
- ``pull``    the state helper is about to pull it (``python -m barca._state pull``)
"""

import os
import sys
import time
from pathlib import Path

_spec = os.environ.get("BARCA_TEST_HOLD", "")


def _hold(point: str, directory: Path) -> None:
    marker = directory / f"{point}.started"
    try:
        fd = os.open(marker, os.O_CREAT | os.O_EXCL | os.O_WRONLY)
    except FileExistsError:
        return  # only the first arrival waits
    os.write(fd, str(os.getpid()).encode())
    os.close(fd)
    while not (directory / "release").exists():
        time.sleep(0.02)


def _install(point: str, directory: Path) -> None:
    from barca import _storage

    def wrap(name: str, when=lambda *a: True) -> None:
        real = getattr(_storage, name)

        def held(*args, **kwargs):
            if when(*args):
                _hold(point, directory)
            return real(*args, **kwargs)

        setattr(_storage, name, held)

    if point == "put":
        wrap("put_file")
    elif point == "get":
        wrap("get_file")
    elif point == "copied":
        import types

        real_copy = _storage.shutil.copyfile

        def copy_then_hold(src, dst, *args, **kwargs):
            out = real_copy(src, dst, *args, **kwargs)
            _hold(point, directory)
            return out

        # Only `_storage`'s view of shutil: the rest of the process is untouched.
        _storage.shutil = types.SimpleNamespace(copyfile=copy_then_hold)
    elif point in ("push", "pull"):
        # `_state.push` and `_state.pull` resolve their target through `_storage` first,
        # whatever the backend.
        wrap("local_path_of", lambda *a: sys.argv[1:2] == [point])


if ":" in _spec:
    _point, _, _dir = _spec.partition(":")
    try:
        _install(_point, Path(_dir))
    except ImportError:
        pass  # a Python process that is not barca's
