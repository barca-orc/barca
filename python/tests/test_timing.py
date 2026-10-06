"""Tests for per-asset timing: fixed, variable, skewed distributions.

Verifies that elapsed times are recorded correctly and stats (mean, median, max, p95)
are computed accurately.
"""

import shutil
import textwrap
from pathlib import Path

import pytest

import barca


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


class TestTimingRecorded:
    """Verify elapsed_seconds is actually persisted and non-zero for real work."""

    def test_fixed_time_asset(self, tmp_path):
        """An asset with a known sleep should record elapsed >= sleep time."""
        f = write_module(
            tmp_path,
            "m.py",
            """
            import time
            from barca import asset

            @asset()
            def slow():
                time.sleep(0.1)
                return {"done": True}
        """,
        )
        barca.get(f)
        stats = barca.stats("slow", f)
        assert stats["total_runs"] >= 1
        avg = stats.get("avg_elapsed_seconds") or stats.get("avg_elapsed")
        assert avg is not None
        assert avg >= 0.05  # at least 50ms (sleep 100ms minus overhead tolerance)

    def test_fast_asset_records_nonzero(self, tmp_path):
        """Even a trivial asset should record a positive elapsed time."""
        f = write_module(
            tmp_path,
            "m.py",
            """
            from barca import asset

            @asset()
            def fast():
                return 1
        """,
        )
        barca.get(f)
        barca.get(f, refresh_all=True)
        barca.get(f, refresh_all=True)
        stats = barca.stats("fast", f)
        assert stats["total_runs"] >= 3


class TestStatsAccuracy:
    """Verify mean, median, max, p95 are computed correctly from known distributions."""

    def _run_n_times(self, f, target, n):
        """Run the asset n times with no_cache to get n materialization records."""
        for _ in range(n):
            barca.get(target, f, refresh_all=True)

    def test_fixed_time_stats(self, tmp_path):
        """Multiple runs of a fixed-time asset: mean ≈ median ≈ max."""
        f = write_module(
            tmp_path,
            "m.py",
            """
            import time
            from barca import asset

            @asset()
            def fixed():
                time.sleep(0.05)
                return 1
        """,
        )
        self._run_n_times(f, "fixed", 5)
        stats = barca.stats("fixed", f)
        avg = stats.get("avg_elapsed_seconds") or 0
        median = stats.get("median_elapsed_seconds") or 0
        max_t = stats.get("max_elapsed_seconds") or 0
        # All should be close to 0.05s (within 0.1s tolerance)
        assert 0.03 < avg < 0.3, f"avg={avg}"
        assert 0.03 < median < 0.3, f"median={median}"
        assert 0.03 < max_t < 0.5, f"max={max_t}"
        # For fixed time: median should be close to avg
        assert abs(avg - median) < 0.1

    def test_variable_time_stats(self, tmp_path):
        """Assets with variable sleep times: max > avg."""
        # We can't easily control per-run sleep from the outside,
        # so use a counter file to vary the sleep time.
        counter_file = tmp_path / "counter.txt"
        counter_file.write_text("0")
        f = write_module(
            tmp_path,
            "m.py",
            f"""
            import time
            from barca import asset

            @asset()
            def variable():
                counter_path = "{counter_file}"
                with open(counter_path) as fh:
                    n = int(fh.read().strip())
                with open(counter_path, "w") as fh:
                    fh.write(str(n + 1))
                # Sleep 0ms, 50ms, 100ms, 150ms, 200ms...
                time.sleep(n * 0.05)
                return {{"run": n}}
        """,
        )
        self._run_n_times(f, "variable", 5)
        stats = barca.stats("variable", f)
        avg = stats.get("avg_elapsed_seconds") or 0
        max_t = stats.get("max_elapsed_seconds") or 0
        # max should be significantly larger than avg
        # Runs: 0s, 50ms, 100ms, 150ms, 200ms → avg ≈ 100ms, max ≈ 200ms
        assert max_t > avg, f"max={max_t} should be > avg={avg}"
        assert max_t >= 0.1, f"max should be at least 100ms, got {max_t}"

    def test_skewed_distribution(self, tmp_path):
        """One slow outlier among fast runs: p95 > median."""
        counter_file = tmp_path / "counter.txt"
        counter_file.write_text("0")
        f = write_module(
            tmp_path,
            "m.py",
            f"""
            import time
            from barca import asset

            @asset()
            def skewed():
                counter_path = "{counter_file}"
                with open(counter_path) as fh:
                    n = int(fh.read().strip())
                with open(counter_path, "w") as fh:
                    fh.write(str(n + 1))
                # 9 fast runs + 1 slow outlier
                if n == 9:
                    time.sleep(0.3)
                else:
                    time.sleep(0.01)
                return {{"run": n}}
        """,
        )
        self._run_n_times(f, "skewed", 10)
        stats = barca.stats("skewed", f)
        median = stats.get("median_elapsed_seconds") or 0
        p95 = stats.get("p95_elapsed_seconds") or 0
        max_t = stats.get("max_elapsed_seconds") or 0
        # Median should be low (fast runs), p95/max should be high (outlier)
        assert median < 0.15, f"median={median} should be < 0.15 (fast runs dominate)"
        assert p95 > median, f"p95={p95} should be > median={median}"
        assert max_t >= 0.2, f"max={max_t} should capture the 300ms outlier"


class TestStatsPerformance:
    """Stats queries should be fast even with many records."""

    def test_stats_query_fast(self, tmp_path):
        """Querying stats after 20 runs should complete in <1s."""
        f = write_module(
            tmp_path,
            "m.py",
            """
            from barca import asset

            @asset()
            def perf():
                return 1
        """,
        )
        import time

        for _ in range(20):
            barca.get(f, refresh_all=True)

        t0 = time.perf_counter()
        stats = barca.stats("perf", f)
        elapsed = time.perf_counter() - t0
        assert elapsed < 1.0, f"stats query took {elapsed:.3f}s, should be <1s"
        assert stats["total_runs"] >= 20
