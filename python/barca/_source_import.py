"""Import user pipeline code exactly as it is on disk, never from a stale `__pycache__` .pyc.

Barca hashes source text at plan time. Python's default loader trusts a timestamp .pyc
whenever the source's mtime (whole seconds) and size match, so an edit that keeps both (an
edit within the same second, or a tool that pins mtimes: Nix, Bazel, `touch -t`,
`rsync -t`) would run the old bytecode under the new run hash (#176).

User modules (the pipeline file and every module imported from its directory tree or from
the project root) load through `SourceHashLoader`, which validates cached bytecode against a
hash of the source bytes (PEP 552 checked hash-based pycs) instead of mtime and size. Their
`__file__`, `__spec__`, `sys.modules` registration, packages and relative imports behave as
with the default loader. The stdlib, site-packages and anything outside the project
directories import exactly as before.

Stdlib only, and cheap to import: the planner's dynamic-partition evaluation uses it too.
"""

from __future__ import annotations

import _imp
import functools
import importlib.machinery as _machinery
import importlib.util
import marshal
import os
import sys
import threading
from pathlib import Path
from types import CodeType, ModuleType

# Points a cached code object's co_filename at the source's current path, as the default
# loader does (CPython-specific; skipped where absent).
_fix_co_filename = getattr(_imp, "_fix_co_filename", None)

# PEP 552 flags word: bit 0 = hash-based, bit 1 = check_source.
_CHECKED_HASH_FLAGS = (0b11).to_bytes(4, "little")
_HEADER = 16  # magic (4) + flags (4) + source hash (8)

# One source object for normal imports, legacy aliases and cached-only reads.
# Map access never holds a global lock across user code. Separate source locks
# preserve setup-once across compatibility aliases without blocking other imports.
_source_lock = threading.RLock()
_setup_locks: dict[str, threading.RLock] = {}
_pipeline_lock = threading.RLock()
_loaded_sources: dict[str, ModuleType] = {}
_LEGACY_PROBE_LIMIT = 256


def _setup_lock(path: str):
    with _source_lock:
        return _setup_locks.setdefault(path, threading.RLock())


class SourceHashLoader(_machinery.SourceFileLoader):
    """A SourceFileLoader whose bytecode cache is keyed by the source bytes, not mtime.

    A cached .pyc is used only when it is hash-based and its hash matches the source on
    disk now; anything else (a timestamp .pyc, a different hash, a corrupt file) is
    ignored, the source is compiled, and a checked hash-based .pyc replaces it. Compiling
    a large helper module costs milliseconds per worker; hashing it costs microseconds.
    """

    def exec_module(self, module):
        path = os.path.realpath(self.path)
        with _setup_lock(path):
            loaded = _loaded_sources.get(path)
            if loaded is not None and loaded is not module:
                # importlib returns the registered module after exec_module.
                sys.modules[self.name] = loaded
                return
            # Executing an existing object is an explicit importlib.reload;
            # preserve that ordinary Python operation rather than suppress it.
            _loaded_sources[path] = module
            try:
                super().exec_module(module)
            except BaseException:
                if _loaded_sources.get(path) is module:
                    del _loaded_sources[path]
                raise

    def get_code(self, fullname):
        source_path = self.get_filename(fullname)
        source = self.get_data(source_path)
        source_hash = importlib.util.source_hash(source)
        try:
            bytecode_path = importlib.util.cache_from_source(source_path)
        except NotImplementedError:  # sys.implementation.cache_tag is None
            bytecode_path = None

        if bytecode_path is not None:
            try:
                data = self.get_data(bytecode_path)
            except OSError:
                data = None
            if (
                data is not None
                and len(data) > _HEADER
                and data[:4] == importlib.util.MAGIC_NUMBER
                and int.from_bytes(data[4:8], "little") & 0b1
                and data[8:16] == source_hash
            ):
                try:
                    code = marshal.loads(memoryview(data)[_HEADER:])
                except (EOFError, ValueError, TypeError):
                    code = None
                if isinstance(code, CodeType):
                    if _fix_co_filename is not None:
                        _fix_co_filename(code, source_path)
                    return code

        code = self.source_to_code(source, source_path)
        if bytecode_path is not None and not sys.dont_write_bytecode:
            header = importlib.util.MAGIC_NUMBER + _CHECKED_HASH_FLAGS + source_hash
            # set_data writes atomically and ignores unwritable directories.
            self.set_data(bytecode_path, header + marshal.dumps(code))
        return code


_LOADERS = (
    (_machinery.ExtensionFileLoader, _machinery.EXTENSION_SUFFIXES),
    (SourceHashLoader, _machinery.SOURCE_SUFFIXES),
    (_machinery.SourcelessFileLoader, _machinery.BYTECODE_SUFFIXES),
)

# Real paths of the directories holding the pipeline files this process loaded.
_roots: list[str] = []


@functools.cache
def _excluded_prefixes() -> tuple[str, ...]:
    # A project's own .venv usually sits inside the project directory; never claim it.
    prefixes = {sys.prefix, sys.base_prefix, sys.exec_prefix, sys.base_exec_prefix}
    return tuple(os.path.realpath(p) for p in prefixes if p)


def _within(path: str, root: str) -> bool:
    return path == root or path.startswith(root.rstrip(os.sep) + os.sep)


def _is_user_dir(entry: str) -> bool:
    path = os.path.realpath(entry or os.getcwd())
    if not any(_within(path, r) for r in _roots):
        return False
    if any(_within(path, p) for p in _excluded_prefixes()):
        return False
    parts = path.split(os.sep)
    if "site-packages" in parts or "dist-packages" in parts:
        return False
    return os.path.isdir(path)


def _path_hook(entry: str):
    if _roots and _is_user_dir(entry):
        return _machinery.FileFinder(entry, *_LOADERS)
    raise ImportError("not a barca project directory")


def _claim(root: str) -> None:
    """Route imports from `root` (and below) through SourceHashLoader."""
    if root in _roots:
        return
    _roots.append(root)
    if _path_hook not in sys.path_hooks:
        sys.path_hooks.insert(0, _path_hook)
    # Drop finders the default hook already cached for directories under the new root.
    # (abspath, not realpath: this runs per step load and must stay cheap.)
    cwd = os.getcwd()
    for entry in list(sys.path_importer_cache):
        if isinstance(entry, str) and _within(os.path.abspath(entry or cwd), root):
            del sys.path_importer_cache[entry]


_project_root: str | None = None
_managed_paths: set[str] = set()


def _claim_project_root() -> None:
    """Claim the cwd the process started its first step in: the project root. Once, so a
    step that changes directory does not get that directory claimed by the next load."""
    global _project_root
    if _project_root is None:
        _project_root = os.path.realpath(os.getcwd())
        _claim(_project_root)


def load_source_module(source_file: str, mod_name: str) -> ModuleType:
    """Import `source_file` as `mod_name` via SourceHashLoader, registered in sys.modules.

    The file's directory goes on sys.path (so sibling imports work) and is claimed, so
    those imports are validated against their source too. So is the cwd, the project root:
    it is already on sys.path behind the file's directory, and the modules a step imports
    from it are part of the step's hash (crates/barca-core/src/project_modules.rs mirrors
    this search order: the file's directory, then the root).
    """
    path = os.path.realpath(source_file)
    with _pipeline_lock:
        module_dir = os.path.dirname(path)
        if module_dir not in sys.path:
            sys.path.insert(0, module_dir)
        _claim(module_dir)
        _claim_project_root()
    # Registration and setup share source ownership. No global lock spans user code.
    with _setup_lock(path):
        loaded = _loaded_sources.get(path)
        if loaded is not None:
            sys.modules[mod_name] = loaded
            return loaded
        loader = SourceHashLoader(mod_name, path)
        spec = importlib.util.spec_from_file_location(mod_name, path, loader=loader)
        if spec is None:
            raise RuntimeError(f"Could not load module spec for {path}")
        mod = importlib.util.module_from_spec(spec)
        # Register before executing so pickle and dataclasses can find the module.
        sys.modules[mod_name] = mod
        loader.exec_module(mod)
        return sys.modules[mod_name]


def load_package_module(dotted: str) -> ModuleType:
    """Import `dotted` (a module inside a package under the cwd, the project root) by name, so
    it has its parent package. The root goes on sys.path and is claimed, so the package and
    everything it imports compile from source like any pipeline file."""
    root = os.path.realpath(os.getcwd())
    if root not in sys.path:
        sys.path.insert(0, root)
    _claim(root)
    return importlib.import_module(dotted)


def _pipeline_name(path: Path) -> str | None:
    """An ordinary root/package/namespace identity, when its path has one."""
    _claim_project_root()
    assert _project_root is not None
    try:
        parts = list(path.relative_to(_project_root).with_suffix("").parts)
    except ValueError:
        return None
    if parts and parts[-1] == "__init__":
        parts.pop()
    if not parts or not all(part.isidentifier() for part in parts):
        return None
    return ".".join(parts)


def _legacy_name(path: Path) -> str:
    _claim_project_root()
    assert _project_root is not None
    try:
        parts = path.relative_to(_project_root).with_suffix("").parts
    except ValueError:
        parts = (path.stem,)
    return "_barca_" + "__".join(parts)


def activate_source_path(source_file: str) -> None:
    """Restore this task's ordinary import path, including cached module tasks."""
    _claim_project_root()
    assert _project_root is not None
    path = Path(source_file).resolve()
    directory = str(path.parent)
    relative = path.relative_to(_project_root) if path.is_relative_to(_project_root) else None
    packaged = (
        relative is not None
        and len(relative.parts) > 1
        and all(
            Path(_project_root).joinpath(*relative.parts[:i], "__init__.py").is_file()
            for i in range(1, len(relative.parts))
        )
    )
    entries = (
        [_project_root] if packaged or directory == _project_root else [directory, _project_root]
    )
    _managed_paths.add(directory)
    _managed_paths.add(_project_root)
    sys.path[:] = entries + [entry for entry in sys.path if entry not in _managed_paths]
    _claim(directory)


def _ordinary_source(name: str) -> str | None:
    """Find a module without executing its parents or consulting sys.modules."""
    paths = sys.path
    spec = None
    prefix = []
    for part in name.split("."):
        prefix.append(part)
        spec = _machinery.PathFinder.find_spec(part, paths)
        if spec is None:
            return None
        paths = (
            list(spec.submodule_search_locations)
            if spec.submodule_search_locations is not None
            else None
        )
        if paths is None and len(prefix) != len(name.split(".")):
            return None
    return os.path.realpath(spec.origin) if spec and spec.origin else None


def load_pipeline_module(source_file: str) -> ModuleType:
    """Load once with an ordinary identity; keep old aliases for durable pickle."""
    path = Path(source_file).resolve()
    with _pipeline_lock:
        activate_source_path(str(path))
        name = _pipeline_name(path)
        legacy = _legacy_name(path)
    loaded = _loaded_sources.get(str(path))
    if loaded is not None:
        # A normal import may have registered this object before finishing setup.
        # Wait for that source only; imports of other helpers remain independent.
        with _setup_locks[str(path)]:
            loaded = _loaded_sources.get(str(path))
    if loaded is None:
        if name is not None and _ordinary_source(name) == str(path):
            occupied = sys.modules.get(name)
            if occupied is not None and os.path.realpath(getattr(occupied, "__file__", "")) != str(
                path
            ):
                raise ImportError(
                    f"project module '{name}' from '{path}' conflicts with an already imported module; use an explicit qualified project import"
                )
            loaded = importlib.import_module(name)
        else:
            # Files that ordinary imports cannot name retain their path identity.
            loaded = load_source_module(str(path), legacy)
    if os.path.realpath(getattr(loaded, "__file__", "")) != str(path):
        raise ImportError(f"project module source mismatch for '{path}'")
    previous = sys.modules.get(legacy)
    ordinary_legacy = _ordinary_source(legacy)
    # Compatibility aliases cannot occupy a real ordinary module name or
    # replace another source's alias. Normal qualified identities still work;
    # ambiguous old pickles refuse in the exhaustive compatibility reader.
    if (previous is None or previous is loaded) and (
        ordinary_legacy is None or ordinary_legacy == str(path)
    ):
        sys.modules[legacy] = loaded
    return loaded


def legacy_pickle_module(name: str) -> ModuleType:
    """Recover a source without retaining its import path in the caller's scope."""
    with _pipeline_lock:
        previous_paths = sys.path.copy()
    try:
        return _recover_legacy_pickle_module(name)
    finally:
        with _pipeline_lock:
            sys.path[:] = previous_paths


def _recover_legacy_pickle_module(name: str) -> ModuleType:
    """Prove one legacy source by exhaustive bounded, path-pruned probes."""
    _claim_project_root()
    assert _project_root is not None
    suffix = name.removeprefix("_barca_")
    if not name.startswith("_barca_") or not suffix or "/" in suffix or "\\" in suffix:
        raise ImportError(
            f"legacy project identity '{name}' is unavailable; use explicit refresh after resolving the source layout"
        )
    registered = sys.modules.get(name)
    registered_file = getattr(registered, "__file__", None)
    if isinstance(registered, ModuleType) and registered_file is not None:
        path = Path(registered_file).resolve()
        if (
            not path.is_relative_to(_project_root)
            and _loaded_sources.get(str(path)) is registered
            and _legacy_name(path) == name
        ):
            # A direct path worker already loaded this exact outside-root
            # object. Keep ordinary pickle's existing registered-module
            # behavior; cold recovery cannot infer an outside-root source.
            return registered
    candidates: set[Path] = set()
    # A legitimate ordinary module may itself start with '_barca_'. Preserve
    # ordinary imports, but never guess if the same name also encodes a legacy source.
    ordinary = _ordinary_source(name)
    if ordinary is not None and Path(ordinary).is_relative_to(_project_root):
        candidates.add(Path(ordinary))
    pending = [(Path(_project_root), suffix)]
    probes = 0
    while pending:
        directory, remaining = pending.pop()
        probes += 1
        if probes > _LEGACY_PROBE_LIMIT:
            raise ImportError(
                f"legacy project identity '{name}' exceeded its recovery bound; use explicit refresh after resolving the source layout"
            )
        file = directory / (remaining + ".py")
        if file.is_file():
            file = file.resolve()
            if file.is_relative_to(_project_root) and _legacy_name(file) == name:
                candidates.add(file)
        # Every possible delimiter split, including overlapping '__' in '___'.
        for offset in range(len(remaining) - 1):
            if remaining[offset : offset + 2] != "__":
                continue
            component, tail = remaining[:offset], remaining[offset + 2 :]
            if not component or not tail or component in {".", ".."}:
                continue
            probes += 1
            if probes > _LEGACY_PROBE_LIMIT:
                raise ImportError(
                    f"legacy project identity '{name}' exceeded its recovery bound; use explicit refresh after resolving the source layout"
                )
            child = directory / component
            if child.is_dir() and child.resolve().is_relative_to(_project_root):
                pending.append((child, tail))
    if not candidates and ordinary is not None:
        # A normal external module can legitimately use this prefix. Only use
        # it after exhaustive proof that no project legacy source matches.
        return importlib.import_module(name)
    if len(candidates) != 1:
        state = "ambiguous" if candidates else "unavailable"
        raise ImportError(
            f"legacy project identity '{name}' is {state}; use explicit refresh after resolving the source layout"
        )
    return load_pipeline_module(str(next(iter(candidates))))
