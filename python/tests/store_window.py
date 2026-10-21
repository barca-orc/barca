"""The store check window (#593) as tests see it.

A successful startup check of the artifact store is remembered in `.barca/gc-state.json` for 5
minutes, and a run within that window that transfers nothing does not check the store again.
A test that simulates time passing, or a fresh machine that kept its `.barca/`, forgets the
remembered checks first.
"""

import json
from pathlib import Path

WINDOW_SECS = 300


def _state(root: Path) -> Path:
    return root / ".barca" / "gc-state.json"


def store_checks(root: Path) -> list[dict]:
    """The remembered store checks of the project at `root`."""
    path = _state(root)
    if not path.exists():
        return []
    return json.loads(path.read_text()).get("store_checks", [])


def age_store_checks(root: Path, seconds: int = WINDOW_SECS + 1) -> None:
    """Move every remembered store check `seconds` into the past (by default, past the window)."""
    path = _state(root)
    state = json.loads(path.read_text())
    for check in state.get("store_checks", []):
        check["at"] -= seconds
    path.write_text(json.dumps(state))


def forget_store_checks(root: Path) -> None:
    """Forget the remembered store checks: the next run checks the store, as on a new machine."""
    path = _state(root)
    if path.exists():
        state = json.loads(path.read_text())
        state.pop("store_checks", None)
        path.write_text(json.dumps(state))
