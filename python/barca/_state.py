"""Metadata backups in the store, and store checks for ``barca status`` (RFC-0009).

Invoked by the Rust coordinator as ``python -m barca._state <verb> ...`` so the fsspec extras
(and their credential chains) remain the only cloud-auth surface. This module never opens the
database: it moves and lists opaque files. Every verb prints one JSON object on stdout; a
failure is one ``error: ...`` line on stderr and exit 1.

  backup <db> <sidecar> <dir> <keep>
      Upload the backup ``<db>`` (named ``<name>.db``) and its ``<sidecar>`` (``<name>.json``)
      into the store directory ``<dir>``, then remove every backup in ``<dir>`` but the
      ``<keep>`` newest and the newest of every other writer (the ``host`` and ``project`` of
      a sidecar) taken in the last week, at most ``<keep>`` of those, with their sidecars. The database goes first: a sidecar is never ahead of
      its database. ``others`` lists the backups another writer took after this writer's
      previous one, newest first: two machines backing up to one store.
      -> {"uploaded": name, "removed": [names], "others": [{"name", "host", "project"}]}
  list <dir>
      The backups in ``<dir>``, newest first. A directory that does not exist holds none.
      -> {"backups": [{"name", "size_bytes", "sidecar": {...} | null}]}
  fetch <uri> <local>
      Download ``<uri>`` to ``<local>`` through a temp file renamed into place. -> {}
  stat
      Reads {"paths": [uri, ...]} on stdin. For each: whether the object exists, its size,
      and, for a store that is a plain directory, its SHA-256 (an object store has no content
      checksum this can compare). -> {"objects": [{"exists", "size_bytes", "sha256"}]}

Only names of the backup form (``YYYY-MM-DDTHH-MM-SS.mmmZ-<8 hex>.db``, or 0.26.0's
``YYYY-MM-DDTHH-MM-SSZ.db``) are ever listed or removed, newest first; the shared-history object of releases before 0.26.0 (``state/metadata.db``) is ignored.
"""

import hashlib
import json
import os
import re
import sys
from pathlib import Path

from barca import _storage

BACKUP_NAME = re.compile(r"^\d{4}-\d{2}-\d{2}T\d{2}-\d{2}-\d{2}(Z|\.\d{3}Z-[0-9a-f]{8})\.db$")


# How long the newest backup of another writer is kept beyond the `keep` newest.
OTHER_WRITER_DAYS = 7


def _age_days(name: str) -> float:
    """Days since the backup ``name`` was taken, from the UTC time its name starts with."""
    import datetime

    try:
        taken = datetime.datetime.strptime(name[:19], "%Y-%m-%dT%H-%M-%S").replace(
            tzinfo=datetime.UTC
        )
    except ValueError:
        return float("inf")
    now = datetime.datetime.now(datetime.UTC)
    return (now - taken).total_seconds() / 86400


def _order_key(name: str) -> str:
    """What backups sort by, oldest first: a to-the-second name counts as millisecond .000."""
    if len(name) == 23 and name.endswith("Z.db"):
        return name[:19] + ".000Z"
    return name


def _sha256(path: "Path | str") -> str:
    h = hashlib.sha256()
    with open(path, "rb") as f:
        for chunk in iter(lambda: f.read(1 << 20), b""):
            h.update(chunk)
    return h.hexdigest()


def _sidecar_name(name: str) -> str:
    return name[: -len(".db")] + ".json"


def _join(base: str, name: str) -> str:
    return str(_storage.join(base, name))


def _names(dir_uri: str) -> "list[tuple[str, int]]":
    """(name, size) of every backup in the store directory, newest first."""
    local = _storage.local_path_of(dir_uri)
    found: list[tuple[str, int]] = []
    if local is not None:
        if not local.is_dir():
            return []
        for entry in local.iterdir():
            if BACKUP_NAME.match(entry.name) and entry.is_file():
                found.append((entry.name, entry.stat().st_size))
    else:
        fs = _storage.get_fs(dir_uri)
        try:
            listed = fs.ls(dir_uri, detail=True)
        except FileNotFoundError:
            return []
        for info in listed:
            name = str(info.get("name", "")).rstrip("/").rsplit("/", 1)[-1]
            if BACKUP_NAME.match(name) and info.get("type", "file") != "directory":
                found.append((name, int(info.get("size") or 0)))
    found.sort(key=lambda f: _order_key(f[0]), reverse=True)
    return found


def _read_sidecar(dir_uri: str, name: str) -> "dict | None":
    uri = _join(dir_uri, _sidecar_name(name))
    local = _storage.local_path_of(uri)
    try:
        if local is not None:
            text = local.read_text()
        else:
            with _storage.get_fs(uri).open(uri, "rb") as f:
                text = f.read().decode()
        parsed = json.loads(text)
        return parsed if isinstance(parsed, dict) else None
    except (FileNotFoundError, ValueError, UnicodeDecodeError):
        return None


def _remove(uri: str) -> None:
    local = _storage.local_path_of(uri)
    try:
        if local is not None:
            local.unlink()
        else:
            _storage.get_fs(uri).rm(uri)
    except FileNotFoundError:
        pass


def _writer(sidecar: "dict | None") -> "tuple[str, str | None] | None":
    """Who took a backup: (host, project), or None for a sidecar from before 0.29."""
    if not sidecar or not isinstance(sidecar.get("host"), str):
        return None
    project = sidecar.get("project")
    return (sidecar["host"], project if isinstance(project, str) else None)


def backup(db: str, sidecar: str, dir_uri: str, keep: int) -> dict:
    name = Path(db).name
    if not BACKUP_NAME.match(name):
        raise ValueError(f"not a backup name: {name}")
    _storage.put_file(db, _join(dir_uri, name))
    _storage.put_file(sidecar, _join(dir_uri, _sidecar_name(name)))
    try:
        ours = _writer(json.loads(Path(sidecar).read_text()))
    except (OSError, ValueError):
        ours = None
    names = [n for n, _ in _names(dir_uri)]
    # Sidecars are read only as far as needed: one or two on a store only this writer uses.
    writers: dict = {name: ours}

    def writer_of(n: str):
        if n not in writers:
            writers[n] = _writer(_read_sidecar(dir_uri, n))
        return writers[n]

    # Backups other writers took after this writer's previous one, which ends the scan; none
    # when there is no previous one (a machine taking over) or an older barca's is in the way.
    others = []
    newer = []
    for n in names:
        if n == name:
            continue
        w = writer_of(n)
        if w is None or ours is None:
            break
        if w == ours:
            others = newer
            break
        newer.append({"name": n, "host": w[0], "project": w[1]})
    removed = []
    extra = 0
    for i in range(keep, len(names)):
        old = names[i]
        w = writer_of(old)
        # The newest backup of another writer stays, so its machine can still restore it: one
        # taken in the last week, and at most `keep` of them (a container whose host name
        # changes at every start is a new writer each time).
        if (
            extra < keep
            and w is not None
            and w != ours
            and _age_days(old) < OTHER_WRITER_DAYS
            and not any(writer_of(n) == w for n in names[:i])
        ):
            extra += 1
            continue
        _remove(_join(dir_uri, old))
        _remove(_join(dir_uri, _sidecar_name(old)))
        removed.append(old)
    return {"uploaded": name, "removed": removed, "others": others}


def list_backups(dir_uri: str) -> dict:
    return {
        "backups": [
            {"name": name, "size_bytes": size, "sidecar": _read_sidecar(dir_uri, name)}
            for name, size in _names(dir_uri)
        ]
    }


def fetch(uri: str, local: str) -> dict:
    dest = Path(local)
    with _storage.staged_beside(dest) as tmp:
        _storage.get_file(uri, tmp)
        os.replace(tmp, dest)
    return {}


def stat(paths: "list[str]") -> dict:
    objects = []
    for uri in paths:
        local = _storage.local_path_of(uri)
        if local is not None:
            if local.is_file():
                objects.append(
                    {"exists": True, "size_bytes": local.stat().st_size, "sha256": _sha256(local)}
                )
            else:
                objects.append({"exists": False, "size_bytes": None, "sha256": None})
            continue
        fs = _storage.get_fs(uri)
        try:
            info = fs.info(uri)
        except FileNotFoundError:
            objects.append({"exists": False, "size_bytes": None, "sha256": None})
            continue
        objects.append({"exists": True, "size_bytes": int(info.get("size") or 0), "sha256": None})
    return {"objects": objects}


def main(argv: "list[str] | None" = None) -> int:
    argv = sys.argv[1:] if argv is None else argv
    verb = argv[0] if argv else ""
    if verb == "backup" and len(argv) == 5:
        result = backup(argv[1], argv[2], argv[3], int(argv[4]))
    elif verb == "list" and len(argv) == 2:
        result = list_backups(argv[1])
    elif verb == "fetch" and len(argv) == 3:
        result = fetch(argv[1], argv[2])
    elif verb == "stat" and len(argv) == 1:
        result = stat(json.load(sys.stdin)["paths"])
    else:
        print(
            "usage: python -m barca._state backup <db> <sidecar> <dir> <keep>\n"
            "       python -m barca._state list <dir>\n"
            "       python -m barca._state fetch <uri> <local>\n"
            '       python -m barca._state stat < {"paths": [...]}',
            file=sys.stderr,
        )
        return 2
    print(json.dumps(result))
    return 0


def _stop(signum, frame) -> None:
    """Asked to stop mid-transfer: unwind, so the temp file being written is removed."""
    raise SystemExit(128 + signum)


if __name__ == "__main__":
    import signal

    from barca import _lifeline

    # The coordinator starts this helper out of reach of the terminal's Ctrl-C, which is its
    # own to act on, and stops it with SIGTERM (crates/barca-core/src/helper_proc.rs). Should
    # the coordinator be killed, the lifeline ends this process.
    signal.signal(signal.SIGTERM, _stop)
    _lifeline.watch()
    try:
        sys.exit(main())
    except Exception as exc:
        uri = sys.argv[3] if len(sys.argv) > 3 and sys.argv[1] in ("backup",) else ""
        if not uri and len(sys.argv) > 2:
            uri = sys.argv[2]
        print(f"error: {_storage.describe_error(uri, exc)}", file=sys.stderr)
        sys.exit(1)
