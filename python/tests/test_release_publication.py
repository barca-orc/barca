"""Publication must wait for the official platform wheel, not metadata alone."""

import importlib.util
from pathlib import Path
import urllib.error

import pytest

SPEC = importlib.util.spec_from_file_location(
    "wait_pypi_wheel", Path(__file__).parents[2] / "scripts" / "wait-pypi-wheel.py"
)
HELPER = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(HELPER)
WHEEL = "barca-0.21.0-py3-none-musllinux_1_2_aarch64.whl"


@pytest.fixture
def clock(monkeypatch):
    now = [0]
    monkeypatch.setattr(HELPER, "monotonic", lambda: now[0])
    monkeypatch.setattr(HELPER, "sleep", lambda duration: now.__setitem__(0, now[0] + duration))
    monkeypatch.setattr(HELPER, "MAX_WAIT", 4)
    return now


def release(filename=WHEEL, yanked=False, version="0.21.0"):
    return {"info": {"version": version}, "urls": [{"filename": filename, "yanked": yanked}]}


def test_uploaded_metadata_does_not_beat_simple_index_propagation(monkeypatch, clock):
    requests = []

    def fetch(url, timeout):
        requests.append((url, timeout))
        if url.endswith("/json"):
            return release()
        return {"files": [] if clock[0] == 0 else [{"filename": WHEEL, "yanked": False}]}

    monkeypatch.setattr(HELPER, "fetch_json", fetch)
    assert HELPER.wait_for_wheel("0.21.0", "aarch64", "musl") == WHEEL
    assert clock[0] == 2 and len(requests) == 4
    assert all(0 < timeout <= 4 for _, timeout in requests)
    assert requests[-1][0] == "https://pypi.org/simple/barca/"


@pytest.mark.parametrize(
    "filename",
    [
        "barca-0.21.0-py3-none-manylinux_2_17_aarch64.whl",
        "barca-0.21.0-py3-none-musllinux_1_2_x86_64.whl",
        "barca-0.20.1-py3-none-musllinux_1_2_aarch64.whl",
        "barca-0.21.0.tar.gz",
    ],
)
def test_missing_exact_platform_never_accepts_another_artifact(monkeypatch, clock, filename):
    monkeypatch.setattr(HELPER, "fetch_json", lambda *_: release(filename))
    with pytest.raises(TimeoutError, match="exceeded 4s"):
        HELPER.wait_for_wheel("0.21.0", "aarch64", "musl")
    assert clock[0] == 4


@pytest.mark.parametrize("surface", ["metadata", "index"])
def test_yanked_wheels_fail_without_retry(monkeypatch, clock, surface):
    def fetch(url, _timeout):
        if url.endswith("/json"):
            return release(yanked=surface == "metadata")
        return {"files": [{"filename": WHEEL, "yanked": True}]}

    monkeypatch.setattr(HELPER, "fetch_json", fetch)
    with pytest.raises(ValueError, match="yanked"):
        HELPER.wait_for_wheel("0.21.0", "aarch64", "musl")
    assert clock[0] == 0


def test_wrong_version_metadata_is_not_a_transient_propagation_race(monkeypatch, clock):
    monkeypatch.setattr(HELPER, "fetch_json", lambda *_: release(version="0.20.1"))
    with pytest.raises(ValueError, match="exact tagged version"):
        HELPER.wait_for_wheel("0.21.0", "aarch64", "musl")
    assert clock[0] == 0


@pytest.mark.parametrize("code,retry", [(403, False), (404, True), (503, True)])
def test_only_transient_availability_errors_retry(monkeypatch, clock, code, retry):
    def fetch(*_args):
        raise urllib.error.HTTPError("https://pypi.org", code, "unavailable", {}, None)

    monkeypatch.setattr(HELPER, "fetch_json", fetch)
    expected = TimeoutError if retry else urllib.error.HTTPError
    with pytest.raises(expected):
        HELPER.wait_for_wheel("0.21.0", "aarch64", "musl")
    assert clock[0] == (4 if retry else 0)


def test_trickling_response_cannot_outlive_the_availability_budget(monkeypatch, clock):
    class SlowResponse:
        def __enter__(self):
            return self

        def __exit__(self, *_):
            pass

        def read(self, _amount):
            clock[0] += 3
            return b"{}"

        read1 = read

    monkeypatch.setattr(HELPER.urllib.request, "urlopen", lambda *_args, **_kwargs: SlowResponse())
    with pytest.raises(TimeoutError, match="availability deadline"):
        HELPER.fetch_json("https://pypi.org/simple/barca/", 2)
