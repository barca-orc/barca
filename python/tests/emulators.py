"""When a test that needs an object-store emulator may skip, and when it must fail.

The backend tests run against local emulators (MinIO, fake-gcs-server, Azurite). On a laptop
without Docker they skip, so the rest of the suite still runs. On CI the `backends` job starts
the emulators and says so by setting the variables below (.depot/workflows/ci.yml); there an
unreachable emulator is a failure, because a skip would report green for tests that never ran.

Every "emulator not reachable" decision goes through `require`.
"""

import os

import pytest

# Backend name (as the test modules spell it) -> the variable that declares its emulator
# provided. Setting it, to the default endpoint or another, turns a skip into a failure.
PROVIDED_BY = {
    "s3": "BARCA_TEST_S3_ENDPOINT",
    "gcs": "BARCA_TEST_GCS_ENDPOINT",
    "abfs": "BARCA_TEST_AZURITE_HOST",
    "azure": "BARCA_TEST_AZURITE_HOST",
}


def variable(backend: str) -> str:
    """The declaring variable of a backend, or of a variant of one (`azure_fsspec`)."""
    return PROVIDED_BY[backend.split("_")[0]]


def provided(backend: str) -> bool:
    """Whether the environment declares this backend's emulator to be running."""
    return bool(os.environ.get(variable(backend)))


def require(backend: str, reachable: bool, endpoint: str = "") -> None:
    """Return if the emulator answers. Otherwise fail when it is declared provided, else skip."""
    if reachable:
        return
    where = f" at {endpoint}" if endpoint else ""
    declared_by = variable(backend)
    if provided(backend):
        pytest.fail(
            f"{backend} emulator not reachable{where}, but {declared_by} is set, which declares "
            "it provided (CI sets it). Start the emulator, or unset the variable to skip.",
            pytrace=False,
        )
    pytest.skip(
        f"{backend} emulator not reachable{where} (set {declared_by} to make this a failure)"
    )
