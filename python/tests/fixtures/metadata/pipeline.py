"""The pipeline behind the metadata DB fixtures (scripts/make-metadata-fixtures.py).

A sensor, an asset read from it, a task that prints to stdout and stderr, and an asset that
fails. It must parse with every release that wrote a fixture.
"""

from barca import asset, sensor, task


@sensor()
def state() -> tuple[bool, str]:
    return True, "v1"


@asset(inputs={"s": state})
def refreshed(s: str) -> dict:
    print("refreshed from", s)
    return {"s": s}


@task(inputs={"r": refreshed})
def job(r: dict) -> None:
    import sys

    print("job stdout", r["s"])
    print("job stderr", file=sys.stderr)


@asset()
def broken() -> int:
    raise ValueError("broken on purpose")
