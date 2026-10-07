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
from types import CodeType, ModuleType

# Points a cached code object's co_filename at the source's current path, as the default
# loader does (CPython-specific; skipped where absent).
_fix_co_filename = getattr(_imp, "_fix_co_filename", None)

# PEP 552 flags word: bit 0 = hash-based, bit 1 = check_source.
_CHECKED_HASH_FLAGS = (0b11).to_bytes(4, "little")
_HEADER = 16  # magic (4) + flags (4) + source hash (8)


class SourceHashLoader(_machinery.SourceFileLoader):
    """A SourceFileLoader whose bytecode cache is keyed by the source bytes, not mtime.

    A cached .pyc is used only when it is hash-based and its hash matches the source on
    disk now; anything else (a timestamp .pyc, a different hash, a corrupt file) is
    ignored, the source is compiled, and a checked hash-based .pyc replaces it. Compiling
    a large helper module costs milliseconds per worker; hashing it costs microseconds.
    """

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


_project_root_claimed = False


def _claim_project_root() -> None:
    """Claim the cwd the process started its first step in: the project root. Once, so a
    step that changes directory does not get that directory claimed by the next load."""
    global _project_root_claimed
    if not _project_root_claimed:
        _project_root_claimed = True
        _claim(os.path.realpath(os.getcwd()))


def load_source_module(source_file: str, mod_name: str) -> ModuleType:
    """Import `source_file` as `mod_name` via SourceHashLoader, registered in sys.modules.

    The file's directory goes on sys.path (so sibling imports work) and is claimed, so
    those imports are validated against their source too. So is the cwd, the project root:
    it is already on sys.path behind the file's directory, and the modules a step imports
    from it are part of the step's hash (crates/barca-core/src/project_modules.rs mirrors
    this search order: the file's directory, then the root).
    """
    path = os.path.realpath(source_file)
    module_dir = os.path.dirname(path)
    if module_dir not in sys.path:
        sys.path.insert(0, module_dir)
    _claim(module_dir)
    _claim_project_root()
    loader = SourceHashLoader(mod_name, path)
    spec = importlib.util.spec_from_file_location(mod_name, path, loader=loader)
    if spec is None:
        raise RuntimeError(f"Could not load module spec for {path}")
    mod = importlib.util.module_from_spec(spec)
    # Register before executing so pickle and dataclasses can find the module.
    sys.modules[mod_name] = mod
    loader.exec_module(mod)
    return mod


def load_package_module(dotted: str) -> ModuleType:
    """Import `dotted` (a module inside a package under the cwd, the project root) by name, so
    it has its parent package. The root goes on sys.path and is claimed, so the package and
    everything it imports compile from source like any pipeline file."""
    root = os.path.realpath(os.getcwd())
    if root not in sys.path:
        sys.path.insert(0, root)
    _claim(root)
    return importlib.import_module(dotted)
