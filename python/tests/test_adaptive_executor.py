"""Tests for the adaptive pull-queue executor pieces visible from Python.

Covers worker self-timing (CPU/wall/RSS riding on completion messages), the
tier-1 artifact LRU, and the persisted cost estimates that seed cross-run
batch sizing.
"""

import contextlib
import shutil
import sqlite3
import textwrap
import time
from pathlib import Path

import pytest

import barca
from barca._artifacts import serialize
from barca._worker import _ArtifactLRU, _load_collected_artifacts, _peak_rss_bytes


@pytest.fixture(autouse=True)
def clean_barca_dir():
    barca_dir = Path(".barca")
    if barca_dir.exists():
        shutil.rmtree(barca_dir)
    yield
    if barca_dir.exists():
        shutil.rmtree(barca_dir)


def write_module(tmp_path, filename, code):
    p = tmp_path / filename
    p.write_text(textwrap.dedent(code))
    return str(p)


def _query(sql):
    """One-shot query with the connection closed afterwards — a lingering
    sqlite3 handle would hold a lock that blocks the next barca run."""
    with contextlib.closing(sqlite3.connect(".barca/metadata.db")) as conn:
        return conn.execute(sql).fetchall()


# ─── Unit: measurement helpers ───────────────────────────────────────────────


class TestPeakRss:
    def test_returns_positive_bytes(self):
        rss = _peak_rss_bytes()
        # A running CPython interpreter needs at least a few MB.
        assert rss > 1024 * 1024


class TestArtifactLRU:
    def test_miss_returns_none(self):
        lru = _ArtifactLRU()
        assert lru.get("/nowhere/x.json") is None

    def test_hit_returns_equal_value(self):
        lru = _ArtifactLRU()
        lru.put("/a.json", {"k": [1, 2, 3]})
        assert lru.get("/a.json") == {"k": [1, 2, 3]}

    def test_hit_is_isolated_from_mutation(self):
        # A task mutating its input must never poison a later task's view.
        lru = _ArtifactLRU()
        original = {"rows": [1, 2, 3]}
        lru.put("/a.json", original)
        first = lru.get("/a.json")
        first["rows"].append(999)
        assert lru.get("/a.json") == {"rows": [1, 2, 3]}

    def test_put_copies_value(self):
        lru = _ArtifactLRU()
        value = {"rows": [1]}
        lru.put("/a.json", value)
        value["rows"].append(2)  # caller mutates after put
        assert lru.get("/a.json") == {"rows": [1]}

    def test_eviction_drops_least_recent(self):
        lru = _ArtifactLRU(max_entries=2)
        lru.put("/a", 1)
        lru.put("/b", 2)
        assert lru.get("/a") == 1  # touch /a → /b is now least recent
        lru.put("/c", 3)
        assert lru.get("/b") is None
        assert lru.get("/a") == 1
        assert lru.get("/c") == 3

    def test_uncopyable_value_is_skipped_not_fatal(self):
        lru = _ArtifactLRU()

        class Uncopyable:
            def __deepcopy__(self, memo):
                raise RuntimeError("no copies")

        lru.put("/a", Uncopyable())
        # put failed silently → miss, caller falls through to the store.
        assert lru.get("/a") is None


class TestLoadCollectedArtifacts:
    """`_load_collected_artifacts` — the concurrent loader for a collect()
    fan-in param's artifact list (see #93's follow-up: sequential loading of
    N partition artifacts was needlessly slow for I/O-bound reads)."""

    def _write_artifacts(self, tmp_path, values):
        """Write each value to its own json artifact, return the refs in order."""
        artifacts = []
        for i, value in enumerate(values):
            path = tmp_path / f"a{i}.json"
            serialize(value, path, "json")
            artifacts.append({"path": str(path), "format": "json"})
        return artifacts

    def test_empty_list_returns_empty(self):
        assert _load_collected_artifacts([]) == []

    def test_loads_values_in_declared_order(self, tmp_path):
        # The thread pool's completion order is not guaranteed to match
        # submission order — the result list must still match `artifacts`'
        # declared order regardless of which thread finishes first.
        values = [{"n": i} for i in range(10)]
        artifacts = self._write_artifacts(tmp_path, values)
        result = _load_collected_artifacts(artifacts)
        assert result == values

    def test_populates_lru_on_miss(self, tmp_path):
        lru = _ArtifactLRU()
        artifacts = self._write_artifacts(tmp_path, [{"n": 1}, {"n": 2}])
        _load_collected_artifacts(artifacts, lru)
        assert lru.get(artifacts[0]["path"]) == {"n": 1}
        assert lru.get(artifacts[1]["path"]) == {"n": 2}

    def test_lru_hit_skips_storage_entirely(self, tmp_path):
        # Path that was never written to disk — a storage read would raise
        # FileNotFoundError. An LRU hit must return the cached value without
        # ever touching storage.
        lru = _ArtifactLRU()
        ghost_path = str(tmp_path / "never_written.json")
        lru.put(ghost_path, {"cached": True})
        result = _load_collected_artifacts([{"path": ghost_path, "format": "json"}], lru)
        assert result == [{"cached": True}]

    def test_missing_artifact_raises_file_not_found(self, tmp_path):
        artifacts = self._write_artifacts(tmp_path, [{"n": 1}])
        artifacts.append({"path": str(tmp_path / "missing.json"), "format": "json"})
        with pytest.raises(FileNotFoundError, match="missing.json"):
            _load_collected_artifacts(artifacts)

    def test_missing_artifact_message_names_param_without_doubling(self, tmp_path):
        # Regression test: an earlier version wrapped _load_collected_artifacts'
        # own "Input artifact not found: ..." in another "not found" at the
        # call site, producing a doubled "not found ... not found" message.
        # With `param=`, the full message is raised once, at the source.
        missing_path = str(tmp_path / "missing.json")
        with pytest.raises(FileNotFoundError) as exc_info:
            _load_collected_artifacts([{"path": missing_path, "format": "json"}], param="reports")
        message = str(exc_info.value)
        assert message == f"Input artifact for parameter 'reports' not found: {missing_path}"
        assert message.count("not found") == 1

    def test_cache_misses_load_concurrently(self, tmp_path, monkeypatch):
        # Each artifact's deserialize takes ~50ms. Sequential loading of 8
        # would take ~400ms; a thread pool should collapse this toward the
        # single-artifact cost. Generous upper bound to avoid CI flakiness
        # while still clearly failing if concurrency regresses to sequential.
        import barca._worker as worker_mod

        real_deserialize = worker_mod.deserialize

        def _slow_deserialize(path, fmt, **kwargs):
            time.sleep(0.05)
            return real_deserialize(path, fmt, **kwargs)

        monkeypatch.setattr(worker_mod, "deserialize", _slow_deserialize)

        artifacts = self._write_artifacts(tmp_path, [{"n": i} for i in range(8)])
        t0 = time.perf_counter()
        result = _load_collected_artifacts(artifacts)
        elapsed = time.perf_counter() - t0

        assert result == [{"n": i} for i in range(8)]
        assert elapsed < 0.05 * 8 * 0.5, (
            f"took {elapsed:.3f}s — expected well under the sequential bound "
            "(8 * 50ms), concurrency may have regressed"
        )


# ─── Integration: timing + estimates persisted through a real run ───────────


class TestTimingPersisted:
    def test_cpu_and_rss_recorded_in_materializations(self, tmp_path):
        f = write_module(
            tmp_path,
            "m.py",
            """
            from barca import asset

            @asset()
            def busy():
                total = 0
                for i in range(200_000):
                    total += i * i
                return {"total": total}
        """,
        )
        barca.get(f)
        rows = _query(
            "SELECT cpu_seconds, max_rss_bytes, elapsed_seconds FROM materializations "
            "WHERE node_id LIKE '%:busy' AND status = 'success'"
        )
        assert rows
        cpu, rss, wall = rows[0]
        assert cpu is not None and cpu >= 0.0
        assert rss is not None and rss > 1024 * 1024
        assert wall is not None and wall > 0.0

    def test_cost_estimate_persisted_and_reused(self, tmp_path):
        f = write_module(
            tmp_path,
            "m.py",
            """
            import time
            from barca import asset

            @asset()
            def steady():
                time.sleep(0.05)
                return 1
        """,
        )
        barca.get(f)
        rows = _query(
            "SELECT estimate_seconds, samples FROM cost_estimates WHERE node_id LIKE '%:steady'"
        )
        assert rows
        estimate, samples = rows[0]
        assert samples >= 1
        # First observation IS the estimate; a 50ms sleep must land well
        # under the 30s cold default and above zero.
        assert 0.0 < estimate < 5.0

        # Second (uncached) run folds a new observation into the same row.
        barca.get(f, refresh_all=True)
        rows2 = _query(
            "SELECT estimate_seconds, samples FROM cost_estimates WHERE node_id LIKE '%:steady'"
        )
        assert rows2
        assert rows2[0][1] >= samples + 1


class TestManyTinyPartitions:
    def test_fan_out_completes_correctly(self, tmp_path):
        """Batch-pulled tiny partitions must all materialize exactly once.

        (Fan-in via collect() is exercised in test_reliability.py — this
        guards the lease/batch machinery: no partition lost, none run twice.)
        """
        f = write_module(
            tmp_path,
            "m.py",
            """
            from barca import asset, partitions

            @asset(partitions={"i": partitions([str(n) for n in range(24)])})
            def shard(i):
                return {"i": int(i)}
        """,
        )
        barca.get(f)
        rows = _query(
            "SELECT COUNT(*), COUNT(DISTINCT node_id) FROM materializations "
            "WHERE node_id LIKE '%:shard[%' AND status = 'success'"
        )
        total_rows, distinct_nodes = rows[0]
        assert distinct_nodes == 24, "every partition must materialize"
        assert total_rows == 24, "no partition may run twice (duplicate lease)"

    def test_mixed_duration_partitions_complete_exactly_once(self, tmp_path):
        """Dynamic task sizing: partitions of one node with very different
        costs (1ms vs 60ms) must each materialize exactly once — batch pulls
        sized from early observations must never lose or double-run the
        heavy stragglers."""
        f = write_module(
            tmp_path,
            "m.py",
            """
            import time
            from barca import asset, partitions

            KEYS = [f"p{i:02d}" for i in range(12)]

            @asset(partitions={"key": partitions(KEYS)})
            def work(key):
                # Even-indexed partitions are ~1ms, odd are 60ms.
                time.sleep(0.001 if int(key[1:]) % 2 == 0 else 0.06)
                return {"key": key}
        """,
        )
        barca.get(f)
        rows = _query(
            "SELECT COUNT(*), COUNT(DISTINCT node_id) FROM materializations "
            "WHERE node_id LIKE '%:work[%' AND status = 'success'"
        )
        total, distinct = rows[0]
        assert distinct == 12, "every partition must materialize"
        assert total == 12, "no partition may run twice"

        # The estimator must reflect the cost split: heavy partitions carry
        # larger estimates than tiny ones.
        est = dict(
            _query(
                "SELECT node_id, estimate_seconds FROM cost_estimates WHERE node_id LIKE '%:work[%'"
            )
        )
        assert len(est) == 12
        heavy = [v for k, v in est.items() if int(k.split("p")[-1].rstrip("]")) % 2 == 1]
        tiny = [v for k, v in est.items() if int(k.split("p")[-1].rstrip("]")) % 2 == 0]
        assert min(heavy) > max(tiny), (
            f"heavy estimates {sorted(heavy)} must exceed tiny {sorted(tiny)}"
        )

    def test_second_run_seeds_estimates_for_partitions(self, tmp_path):
        f = write_module(
            tmp_path,
            "m.py",
            """
            from barca import asset, partitions

            @asset(partitions={"i": partitions([str(n) for n in range(8)])})
            def shard(i):
                return {"i": int(i)}
        """,
        )
        barca.get(f)
        count = _query("SELECT COUNT(*) FROM cost_estimates WHERE node_id LIKE '%:shard[%'")[0][0]
        assert count == 8
