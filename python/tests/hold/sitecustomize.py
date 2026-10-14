"""Test only: pause barca's helper processes at named points until the test lets go.

Put this directory on PYTHONPATH and set ``BARCA_TEST_HOLD=<point>[,<point>...]:<dir>``. The
first time a barca Python process reaches one of the points it writes ``<dir>/<point>.started``
(holding its pid) and waits there until ``<dir>/release`` exists. A test can then act at a
moment it controls: wait for the marker, send a signal, look at what is on disk. Later arrivals
at the same point pass, so whatever barca does next (a retry, a second push) runs normally.

With ``BARCA_TEST_STALL=1`` every arrival waits, not only the first: the store never answers,
like one that is stalled or unreachable. Every process that arrives at a point is listed, one
pid per line, in ``<dir>/<point>.pids``, so a test can check that none of them is left behind.

Nothing in the package knows about this; it works by wrapping functions of ``barca._storage``
from outside.

Points:

- ``remote-access`` any storage resolution below the configured remote root (observation)
- ``put``     the transfer helper is about to upload an artifact (``_storage.put_file``)
- ``get``     the transfer helper is about to download one (``_storage.get_file``); its temp
              file exists already
- ``uploaded`` the artifact SDK upload returned, before its receipt reaches the coordinator
- ``partial-get`` the concrete artifact stage has a flushed prefix of actual SDK bytes,
              before that first write completes; other files and downloads are untouched
- ``copied``  a copy into a directory store has written its temp file and not yet renamed it
- ``push``    the state helper is about to push the metadata DB (``python -m barca._state push``)
- ``repush``  a corrective state upload after an earlier upload reached ``pushed``
- ``pull``    the state helper is about to pull it (``python -m barca._state pull``)
- ``pushed``  the state helper has put the metadata DB in a directory store and has not said so
              yet (after the rename, before it prints the new token)
- ``worker-start``    a worker's interpreter is starting: nothing of barca is imported yet
- ``worker-connect``  a worker has imported barca and is about to connect to the coordinator
"""

import os
import sys
import time
from pathlib import Path

_spec = os.environ.get("BARCA_TEST_HOLD", "")
_stall = os.environ.get("BARCA_TEST_STALL") == "1"


def _hold(point: str, directory: Path) -> None:
    with open(directory / f"{point}.pids", "a") as pids:
        pids.write(f"{os.getpid()}\n")
    marker = directory / f"{point}.started"
    try:
        fd = os.open(marker, os.O_CREAT | os.O_EXCL | os.O_WRONLY)
    except FileExistsError:
        if not _stall:
            return  # only the first arrival waits
    else:
        os.write(fd, str(os.getpid()).encode())
        os.close(fd)
    while not (directory / "release").exists():
        time.sleep(0.02)


def _is_worker() -> bool:
    argv = getattr(sys, "orig_argv", [])
    return "barca._worker" in argv and "--daemon" in argv


def _install(point: str, directory: Path) -> None:
    if point == "worker-start":
        # Here and now: this module is imported while the interpreter starts.
        if _is_worker():
            _hold(point, directory)
        return
    if point == "worker-connect":
        if _is_worker():
            from barca import _runtime

            real_connect = _runtime.connect

            def connect_after_hold():
                _hold(point, directory)
                return real_connect()

            _runtime.connect = connect_after_hold
        return

    from barca import _storage

    def wrap(name: str, when=lambda *a: True) -> None:
        real = getattr(_storage, name)

        def held(*args, **kwargs):
            if when(*args):
                _hold(point, directory)
            return real(*args, **kwargs)

        setattr(_storage, name, held)

    # The state helper moves the history through the same functions for an object store;
    # `put` and `get` are about artifacts, so they hold the transfer helper only.
    def transferring(*args) -> bool:
        return os.path.basename(sys.argv[0]) == "_transfer.py"

    if point == "remote-access":
        root = os.environ["BARCA_REMOTE_URI"].rstrip("/")
        wrap("local_path_of", lambda path: str(path) == root or str(path).startswith(root + "/"))
    elif point == "put":
        wrap("put_file", transferring)
    elif point == "get":
        wrap("get_file", transferring)
    elif point == "uploaded":
        real_put = _storage.put_file

        def put_then_hold(*args, **kwargs):
            result = real_put(*args, **kwargs)
            if transferring():
                _hold(point, directory)
            return result

        _storage.put_file = put_then_hold
    elif point == "partial-get":
        import builtins
        import threading

        real_open, real_get = builtins.open, _storage.get_file
        lock = threading.Lock()
        selected = None
        claimed = False

        class PartialWrite:
            def __init__(self, file):
                self.file = file
                self.held = False

            def __getattr__(self, name):
                return getattr(self.file, name)

            def __enter__(self):
                self.file.__enter__()
                return self

            def __exit__(self, *args):
                return self.file.__exit__(*args)

            def write(self, data):
                if self.held or not data:
                    return self.file.write(data)
                self.held = True
                split = min(len(data), 64 * 1024)
                written = self.file.write(data[:split])
                self.file.flush()
                _hold(point, directory)
                return written + self.file.write(data[split:])

        def scoped_open(file, mode="r", *args, **kwargs):
            opened = real_open(file, mode, *args, **kwargs)
            # fsspec performs its write on another thread. Match the exact active
            # stage, never a thread-wide/global write hook or merely a .tmp suffix.
            with lock:
                target = selected
            if (
                target is not None
                and isinstance(file, (str, bytes, os.PathLike))
                and os.path.abspath(os.fsdecode(file)) == target
                and "w" in mode
                and "b" in mode
            ):
                return PartialWrite(opened)
            return opened

        def get_with_partial_stage(remote, local):
            nonlocal selected, claimed
            with lock:
                first = transferring() and not claimed
                if first:
                    claimed = True
                    selected = os.path.abspath(os.fspath(local))
            if not first:
                return real_get(remote, local)
            (directory / "partial-get.path").write_text(selected)
            try:
                return real_get(remote, local)
            finally:
                with lock:
                    selected = None

        builtins.open = scoped_open
        _storage.get_file = get_with_partial_stage
    elif point == "copied":
        import types

        real_copy = _storage.shutil.copyfile

        def copy_then_hold(src, dst, *args, **kwargs):
            out = real_copy(src, dst, *args, **kwargs)
            _hold(point, directory)
            return out

        # Only `_storage`'s view of shutil: the rest of the process is untouched.
        _storage.shutil = types.SimpleNamespace(copyfile=copy_then_hold)
    elif point == "pushed":
        from contextlib import contextmanager

        real_staged = _storage.staged_beside

        @contextmanager
        def staged_then_hold(dest):
            with real_staged(dest) as tmp:
                yield tmp
            if sys.argv[1:2] == ["push"]:
                _hold(point, directory)

        _storage.staged_beside = staged_then_hold
    elif point == "repush":
        wrap(
            "local_path_of",
            lambda *a: sys.argv[1:2] == ["push"] and (directory / "pushed.started").exists(),
        )
    elif point in ("push", "pull"):
        # `_state.push` and `_state.pull` resolve their target through `_storage` first,
        # whatever the backend. Both points can be installed at once: each wraps the other.
        wrap("local_path_of", lambda *a: sys.argv[1:2] == [point])


if ":" in _spec:
    _points, _, _dir = _spec.partition(":")
    try:
        for _point in _points.split(","):
            _install(_point, Path(_dir))
    except ImportError:
        pass  # a Python process that is not barca's
