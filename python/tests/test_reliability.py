"""Tests for v0.1.3 reliability features: partial results, timeouts, fan-in cache."""

import textwrap

import barca
import pytest
from barca.api import BarcaError


@pytest.fixture(autouse=True)
def clean_barca_dir(tmp_path, monkeypatch):
    # The API resolves .barca relative to cwd. Give each test its own project,
    # so another pytest worker cannot delete its database or input artifacts.
    monkeypatch.chdir(tmp_path)


def write_module(tmp_path, filename, code):
    p = tmp_path / filename
    p.write_text(textwrap.dedent(code))
    return str(p)


# ─── Partial result persistence (#38) ─────────────────────────────────────────


class TestPartialResults:
    def test_partial_results_persisted_on_failure(self, tmp_path):
        """When step 2 of 2 fails, step 1's output should still be cached."""
        f = write_module(
            tmp_path,
            "partial.py",
            """
            from barca import asset

            @asset()
            def step_a():
                return {"value": 42}

            @asset(inputs={"data": step_a})
            def step_b(data):
                raise ValueError("intentional failure")
        """,
        )
        # First run should fail
        with pytest.raises(BarcaError) as exc_info:
            barca.get(f)
        assert "ValueError" in str(exc_info.value)

        # step_a's result should be cached — second run of just step_a should be instant
        f2 = write_module(
            tmp_path,
            "partial.py",
            """
            from barca import asset

            @asset()
            def step_a():
                return {"value": 42}
        """,
        )
        result = barca.api._exec(["get", "step_a", f2])
        assert result["steps_executed"] == 0  # cached from the failed run

    def test_error_message_includes_traceback(self, tmp_path):
        """Worker failure should include the Python exception info."""
        f = write_module(
            tmp_path,
            "fail.py",
            """
            from barca import asset

            @asset()
            def explode():
                return 1 / 0
        """,
        )
        with pytest.raises(BarcaError) as exc_info:
            barca.get(f)
        msg = str(exc_info.value)
        assert "ZeroDivisionError" in msg
        assert "explode" in msg


# ─── Timeout enforcement (#42) ────────────────────────────────────────────────


class TestTimeout:
    def test_timeout_kills_hung_function(self, tmp_path):
        """A function exceeding timeout_seconds should raise TimeoutError."""
        f = write_module(
            tmp_path,
            "slow.py",
            """
            import time
            from barca import asset

            @asset(timeout_seconds=1)
            def hang():
                time.sleep(60)
                return {"done": True}
        """,
        )
        with pytest.raises(BarcaError) as exc_info:
            barca.get(f)
        msg = str(exc_info.value)
        assert "TimeoutError" in msg or "timeout" in msg.lower()

    def test_fast_function_within_timeout(self, tmp_path):
        """A function completing within timeout should succeed normally."""
        f = write_module(
            tmp_path,
            "fast.py",
            """
            from barca import asset

            @asset(timeout_seconds=10)
            def quick():
                return {"done": True}
        """,
        )
        result = barca.get(f)
        assert result == {"done": True}


# ─── Fan-in cache correctness (#40) ──────────────────────────────────────────


class TestFanInCache:
    def test_collect_fan_in_delivers_full_partition_list(self, tmp_path):
        """Regression test for #97/#93: a collect()-consuming asset must run
        only after *every* partition of its upstream has materialized, and
        must receive the full list — not crash with a missing-argument
        TypeError, and not silently see just one partition's value."""
        f = write_module(
            tmp_path,
            "fanin.py",
            """
            from barca import asset, partitions, collect

            @asset(partitions={"key": partitions(["a", "b", "c"])})
            def source(key):
                return {"key": key, "v": 1}

            @asset(inputs={"data": collect(source)})
            def sink(data):
                return {"count": len(data), "keys": sorted(d["key"] for d in data)}
        """,
        )
        result = barca.get("sink", f)
        assert result == {"count": 3, "keys": ["a", "b", "c"]}

    def test_collect_fan_in_with_dynamic_partitions_from(self, tmp_path):
        """Regression test for a second manifestation of #97 found while
        fixing it: a collect()-consuming asset downstream of a
        `partitions_from(...)`-partitioned producer (partition universe
        resolved at dispatch time, not plan time) was getting silently folded
        into the still-pending producer's phase by the planner's single-stream
        phase merge, landing in the same "runs before its data exists" bug as
        the static-partitions case — just via phase merging instead of chain
        fusion. This is the exact shape documented in
        workflows/03-parametrized-assets-and-partitions.md's collect() example."""
        f = write_module(
            tmp_path,
            "fanin_dynamic.py",
            """
            from barca import asset, partitions_from, collect

            @asset()
            def tickers():
                return ["AAPL", "MSFT", "GOOG"]

            @asset(partitions={"ticker": partitions_from(tickers)})
            def fetch_prices(ticker):
                return {"ticker": ticker}

            @asset(inputs={"reports": collect(fetch_prices)})
            def aggregate(reports):
                return {"total": len(reports)}
        """,
        )
        result = barca.get("aggregate", f)
        assert result == {"total": 3}

    def test_partitioned_upstream_change_invalidates_collector(self, tmp_path):
        """Changing a partitioned upstream should invalidate the collect() consumer."""
        f = write_module(
            tmp_path,
            "fanin.py",
            """
            from barca import asset, partitions, collect

            @asset(partitions={"key": partitions(["a", "b"])})
            def source(key):
                return {"key": key, "v": 1}

            @asset(inputs={"data": collect(source)})
            def sink(data):
                return {"count": len(data)}
        """,
        )
        # First run: executes all
        r1 = barca.api._exec(["get", "sink", f])
        assert r1["steps_executed"] == 3  # 2 source + 1 sink

        # Second run: fully cached
        r2 = barca.api._exec(["get", "sink", f])
        assert r2["steps_executed"] == 0

        # Change source function → should invalidate both source and sink
        f2 = write_module(
            tmp_path,
            "fanin.py",
            """
            from barca import asset, partitions, collect

            @asset(partitions={"key": partitions(["a", "b"])})
            def source(key):
                return {"key": key, "v": 2}

            @asset(inputs={"data": collect(source)})
            def sink(data):
                return {"count": len(data)}
        """,
        )
        r3 = barca.api._exec(["get", "sink", f2])
        assert r3["steps_executed"] == 3  # all re-execute
