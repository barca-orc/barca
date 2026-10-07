"""The skip-or-fail rule for tests that need an object-store emulator (python/tests/emulators.py).

A skip must never hide a failure on CI: where the environment declares an emulator provided,
a test that cannot reach it fails.
"""

import pytest

from . import emulators


@pytest.fixture(autouse=True)
def _no_declared_emulators(monkeypatch):
    for variable in set(emulators.PROVIDED_BY.values()):
        monkeypatch.delenv(variable, raising=False)


@pytest.mark.parametrize("backend", sorted(emulators.PROVIDED_BY))
def test_a_reachable_emulator_is_used_whatever_the_environment_says(backend, monkeypatch):
    emulators.require(backend, True)
    monkeypatch.setenv(emulators.PROVIDED_BY[backend], "http://localhost:1")
    emulators.require(backend, True)


@pytest.mark.parametrize("backend", sorted(emulators.PROVIDED_BY))
def test_an_unreachable_emulator_nobody_declared_is_skipped(backend):
    with pytest.raises(pytest.skip.Exception, match="emulator not reachable"):
        emulators.require(backend, False, "http://localhost:1")


@pytest.mark.parametrize("backend", sorted(emulators.PROVIDED_BY))
def test_an_unreachable_emulator_declared_provided_is_a_failure(backend, monkeypatch):
    variable = emulators.PROVIDED_BY[backend]
    monkeypatch.setenv(variable, "http://localhost:1")
    with pytest.raises(pytest.fail.Exception, match=f"{variable} is set"):
        emulators.require(backend, False, "http://localhost:1")


def test_a_variant_of_a_backend_follows_its_backend(monkeypatch):
    monkeypatch.setenv("BARCA_TEST_AZURITE_HOST", "127.0.0.1:1")
    with pytest.raises(pytest.fail.Exception, match="BARCA_TEST_AZURITE_HOST is set"):
        emulators.require("azure_fsspec", False)


def test_one_declared_emulator_does_not_turn_the_others_into_failures(monkeypatch):
    monkeypatch.setenv("BARCA_TEST_S3_ENDPOINT", "http://localhost:9100")
    with pytest.raises(pytest.skip.Exception):
        emulators.require("gcs", False)


def test_ci_declares_every_emulator_it_starts():
    """The `backends` job must set the three variables, or its skips stay silent."""
    from pathlib import Path

    workflow = Path(__file__).resolve().parents[2] / ".depot" / "workflows" / "ci.yml"
    if not workflow.exists():
        pytest.skip("not a source checkout")
    text = workflow.read_text()
    for variable in sorted(set(emulators.PROVIDED_BY.values())):
        assert f"{variable}:" in text, f"ci.yml does not set {variable} for the backends job"
