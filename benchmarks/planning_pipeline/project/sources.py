"""Root sources: a lazy `read_parquet` over a URI resolved from the environment.

In local mode the URI is a directory on this machine; in store and hybrid modes it is an
object-store URI and DuckDB reads it through httpfs with a secret on the default connection.
Barca never sees which.
"""

import engine
import fsspec
from settings import Settings

_REMOTE_READY = False


def uri(relative: str) -> str:
    return f"{Settings.source_root()}/{relative}"


def exists(target: str) -> bool:
    fs, path = fsspec.core.url_to_fs(target)
    return fs.exists(path)


def _prepare_remote() -> None:
    global _REMOTE_READY
    if _REMOTE_READY:
        return
    c = engine.con()
    c.execute("INSTALL httpfs")
    c.execute("LOAD httpfs")

    def q(value: str) -> str:
        return "'" + value.replace("'", "''") + "'"

    c.execute(
        f"CREATE OR REPLACE SECRET planning_store (TYPE s3, KEY_ID {q(Settings.STORE_KEY_ID)}, "
        f"SECRET {q(Settings.STORE_SECRET)}, ENDPOINT {q(Settings.STORE_ENDPOINT)}, "
        "URL_STYLE 'path', USE_SSL false)"
    )
    _REMOTE_READY = True


def read(relative: str):
    target = uri(relative)
    if not exists(target):
        raise FileNotFoundError(f"source {relative} not found at {target}")
    if "://" in target:
        _prepare_remote()
    return engine.con().read_parquet(target)
