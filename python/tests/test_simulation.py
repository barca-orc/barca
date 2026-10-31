"""tests/simulation/sim.py: a short seed passes, and each invariant can fail.

The long runs are in `.github/workflows/simulation.yml`. CI's full tier runs this file.
"""

import subprocess
import sys
from pathlib import Path

import pytest
from barca.api import _find_binary

SIM = Path(__file__).resolve().parents[2] / "tests/simulation/sim.py"


def sim(tmp_path, *args: str) -> subprocess.CompletedProcess:
    return subprocess.run(
        [sys.executable, str(SIM), "--barca", _find_binary(), "--keep", str(tmp_path), *args],
        capture_output=True,
        text=True,
        timeout=180,
        check=False,
    )


def test_a_short_run_from_an_old_database_holds_every_invariant(tmp_path):
    out = sim(
        tmp_path, "--seed", "1", "--seconds", "15", "--settle-seconds", "60", "--seed-db-mb", "50"
    )
    assert out.returncode == 0, out.stderr


@pytest.mark.parametrize(
    ("sabotage", "says"),
    [
        ("value", "returned -1"),
        ("history", "was deleted"),
        ("integrity", "integrity_check"),
        ("size", "did not settle"),
        ("eviction", "evicted artifacts that fit"),
    ],
)
def test_an_invariant_fails_when_what_it_watches_breaks(tmp_path, sabotage, says):
    out = sim(
        tmp_path, "--seed", "1", "--seconds", "6", "--settle-seconds", "5", "--sabotage", sabotage
    )
    assert out.returncode == 1, out.stderr
    assert says in out.stderr
