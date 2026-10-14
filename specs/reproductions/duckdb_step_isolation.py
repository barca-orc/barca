"""Reproduce #302 through the worker without a coordinator or object store.

Run: PYTHONPATH=python python specs/reproductions/duckdb_step_isolation.py
Requires DuckDB; pyarrow is not needed for the JSON-only worker probes.
This is diagnostic evidence of intentionally shared state, not an isolation guarantee.
"""

import json
import tempfile
from pathlib import Path
from unittest.mock import patch

import duckdb
from barca import _artifacts, _runtime, _worker

SOURCE = """
import duckdb
import barca

# This is currently a documented, supported once-per-worker setup pattern.
barca.duckdb_connection().execute("create macro twice(x) as x * 2")


def producer():
    duckdb.sql("create view picked as select 165.5::double as amount")
    return True


def consumer():
    picked = duckdb.sql("select 80.0::double as amount")
    return duckdb.sql("select sum(amount) from picked").fetchone()[0]


def failure():
    duckdb.sql("create view failed_picked as select 7 as x")
    raise ValueError("failed after catalog mutation")


def after_failure():
    return duckdb.sql("select x from failed_picked").fetchone()[0]


def configured():
    return duckdb.sql("select twice(3)").fetchone()[0]
"""


def main():
    previous = duckdb.default_connection()
    owned = duckdb.connect()
    duckdb.set_default_connection(owned)
    reports = []
    errors = []
    try:
        with tempfile.TemporaryDirectory(prefix="barca-duckdb-isolation-") as directory:
            root = Path(directory)
            source = root / "pipeline.py"
            source.write_text(SOURCE)
            modules = {}
            lru = _worker._ArtifactLRU()

            def run(name):
                step = {
                    "node_id": f"pipeline.py:{name}",
                    "function_name": name,
                    "source_file": str(source),
                    "kind": "task",
                    "run_hash": "probe",
                }
                with (
                    patch.object(_worker, "_emit", lambda kind, **kw: reports.append((kind, kw))),
                    patch.object(_runtime, "emit_step_error", lambda **kw: errors.append(kw)),
                    patch.object(_worker, "_ctrl_c_does_nothing"),
                    patch.object(_worker, "_ctrl_c_interrupts_the_step"),
                ):
                    ok = _worker._run_daemon_step(step, modules, str(root / "arts"), lru)
                if not ok:
                    return None
                artifact = reports[-1][1]["artifact"]
                return _artifacts.deserialize(artifact["path"], artifact["format"])

            before = run("consumer")
            assert before == 80.0
            assert run("producer") is True
            after = run("consumer")
            assert after == 165.5, "the current worker no longer demonstrates shared catalog state"
            assert run("producer") is None
            assert "already exists" in errors[-1]["message"]
            assert run("failure") is None
            assert run("after_failure") == 7
            assert run("configured") == 6
            print(
                json.dumps(
                    {
                        "duckdb": duckdb.__version__,
                        "consumer_before": before,
                        "consumer_after": after,
                        "repeated_producer": "view already exists",
                        "failed_step_view_survives": True,
                        "import_setup_macro_works": True,
                    },
                    sort_keys=True,
                )
            )

            # Rollback is insufficient: it isolates catalog changes but not SET.
            owned.execute("begin")
            original_threads = owned.sql("select current_setting('threads')").fetchone()[0]
            changed_threads = 1 if original_threads != 1 else 2
            owned.execute(f"set threads = {changed_threads}")
            owned.execute("create table rolled_back as select 1 x")
            owned.execute("rollback")
            assert owned.sql("select current_setting('threads')").fetchone()[0] == changed_threads
            assert owned.sql("select twice(3)").fetchone()[0] == 6
            try:
                owned.sql("select * from rolled_back")
            except duckdb.CatalogException:
                pass
            else:
                raise AssertionError("transaction catalog rollback unexpectedly failed")

            fresh = duckdb.connect()
            try:
                duckdb.set_default_connection(fresh)
                assert run("configured") is None  # cached module cannot recreate its macro
                assert "twice" in errors[-1]["message"]
                lazy = fresh.sql("select 42 answer")
                fresh.close()
                try:
                    lazy.fetchall()
                except duckdb.ConnectionException:
                    pass
                else:
                    raise AssertionError("lazy result remained usable after connection close")
            finally:
                fresh.close()
            print(
                json.dumps(
                    {
                        "rollback_leaves_session_setting": True,
                        "fresh_connection_loses_cached_module_setup": True,
                        "lazy_relation_requires_live_connection": True,
                    },
                    sort_keys=True,
                )
            )
    finally:
        duckdb.set_default_connection(previous)
        owned.close()


if __name__ == "__main__":
    main()
