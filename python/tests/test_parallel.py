"""End-to-end tests for the parallel() primitive.

Tests the full stack: Python parallel() → stderr protocol → Rust sub-worker
dispatch → result collection → response to parent.
"""

import textwrap

import barca
import pytest
from barca.api import BarcaError


@pytest.fixture(autouse=True)
def clean_barca_dir(tmp_path, monkeypatch):
    # The API resolves .barca relative to cwd. Give each test its own project,
    # so another pytest worker cannot delete its database or input artifacts.
    monkeypatch.chdir(tmp_path)


def write_module(tmp_path, filename, body):
    code = textwrap.dedent(body).lstrip("\n")
    p = tmp_path / filename
    p.write_text(code)
    return str(p)


# ─── Happy paths ─────────────────────────────────────────────────────────────


class TestParallelHappyPaths:
    def test_static_two_tasks(self, tmp_path):
        """Two partials, both succeed, results returned in arg order."""
        f = write_module(
            tmp_path,
            "pipeline.py",
            """
            from functools import partial
            from barca import asset, task, parallel

            @asset()
            def config() -> dict:
                return {"env": "prod"}

            @task()
            def deploy_us(cfg: dict) -> dict:
                return {"region": "us", "env": cfg["env"]}

            @task()
            def deploy_eu(cfg: dict) -> dict:
                return {"region": "eu", "env": cfg["env"]}

            @task(inputs={"cfg": config})
            def deploy_all(cfg: dict) -> list:
                return parallel(
                    partial(deploy_us, cfg),
                    partial(deploy_eu, cfg),
                )
            """,
        )
        result = barca.run("deploy_all", f)
        assert isinstance(result, list)
        assert len(result) == 2
        assert result[0]["region"] == "us"
        assert result[1]["region"] == "eu"
        assert result[0]["env"] == "prod"

    def test_parallel_results_ordering(self, tmp_path):
        """Results match argument position, not completion order."""
        f = write_module(
            tmp_path,
            "pipeline.py",
            """
            from functools import partial
            from barca import asset, task, parallel

            @task()
            def slow() -> str:
                import time; time.sleep(0.05)
                return "slow"

            @task()
            def fast() -> str:
                return "fast"

            @task()
            def run_both() -> list:
                return parallel(partial(slow), partial(fast))
            """,
        )
        result = barca.run("run_both", f)
        assert result[0] == "slow"
        assert result[1] == "fast"

    def test_parallel_single_item(self, tmp_path):
        """Single item degenerates to a single worker dispatch."""
        f = write_module(
            tmp_path,
            "pipeline.py",
            """
            from functools import partial
            from barca import task, parallel

            @task()
            def greet(name: str) -> str:
                return f"hello {name}"

            @task()
            def run_one() -> list:
                return parallel(partial(greet, "world"))
            """,
        )
        result = barca.run("run_one", f)
        assert result == ["hello world"]

    def test_parallel_with_kwargs(self, tmp_path):
        """Keyword arguments passed through to sub-tasks."""
        f = write_module(
            tmp_path,
            "pipeline.py",
            """
            from functools import partial
            from barca import task, parallel

            @task()
            def deploy(region: str, replicas: int = 1) -> dict:
                return {"region": region, "replicas": replicas}

            @task()
            def deploy_all() -> list:
                return parallel(
                    partial(deploy, "us", replicas=3),
                    partial(deploy, "eu", replicas=2),
                )
            """,
        )
        result = barca.run("deploy_all", f)
        assert result[0] == {"region": "us", "replicas": 3}
        assert result[1] == {"region": "eu", "replicas": 2}


# ─── Error paths ─────────────────────────────────────────────────────────────


class TestParallelErrors:
    def test_one_branch_fails(self, tmp_path):
        """One branch fails, other succeeds — both results returned."""
        f = write_module(
            tmp_path,
            "pipeline.py",
            """
            from functools import partial
            from barca import task, parallel, ParallelError

            @task()
            def succeed() -> str:
                return "ok"

            @task()
            def fail_task() -> str:
                raise ValueError("boom")

            @task()
            def run_mixed() -> list:
                results = parallel(partial(succeed), partial(fail_task))
                # Convert ParallelError to a serializable dict for output
                return [
                    r if not isinstance(r, ParallelError) else {"error": str(r)}
                    for r in results
                ]
            """,
        )
        result = barca.run("run_mixed", f)
        assert isinstance(result, list)
        assert len(result) == 2
        assert result[0] == "ok"
        assert "error" in result[1] or "boom" in str(result[1])

    def test_all_branches_fail(self, tmp_path):
        """All branches fail — all errors collected."""
        f = write_module(
            tmp_path,
            "pipeline.py",
            """
            from functools import partial
            from barca import task, parallel, ParallelError

            @task()
            def fail_a() -> str:
                raise ValueError("boom a")

            @task()
            def fail_b() -> str:
                raise RuntimeError("boom b")

            @task()
            def run_all_fail() -> list:
                results = parallel(partial(fail_a), partial(fail_b))
                return [
                    r if not isinstance(r, ParallelError) else {"error": str(r)}
                    for r in results
                ]
            """,
        )
        result = barca.run("run_all_fail", f)
        assert isinstance(result, list)
        assert len(result) == 2
        assert "error" in result[0] or "boom" in str(result[0])
        assert "error" in result[1] or "boom" in str(result[1])

    def test_parallel_empty(self, tmp_path):
        """parallel() with no args returns empty list."""
        f = write_module(
            tmp_path,
            "pipeline.py",
            """
            from barca import task, parallel

            @task()
            def run_empty() -> list:
                return parallel()
            """,
        )
        result = barca.run("run_empty", f)
        assert result == []


# ─── Enforcement ─────────────────────────────────────────────────────────────


class TestGetRunEnforcement:
    def test_get_rejects_task(self, tmp_path):
        """barca get on a task gives a clear error."""
        f = write_module(
            tmp_path,
            "pipeline.py",
            """
            from barca import task

            @task()
            def my_task() -> None:
                pass
            """,
        )
        with pytest.raises(BarcaError, match="task.*barca run"):
            barca.get("my_task", f)

    def test_run_rejects_asset(self, tmp_path):
        """barca run on an asset gives a clear error."""
        f = write_module(
            tmp_path,
            "pipeline.py",
            """
            from barca import asset

            @asset()
            def my_asset() -> int:
                return 42
            """,
        )
        with pytest.raises(BarcaError, match="asset.*barca get"):
            barca.run("my_asset", f)


# ─── Invariants ──────────────────────────────────────────────────────────────


class TestParallelInvariants:
    def test_same_function_different_args(self, tmp_path):
        """parallel_map over one function with different args — each gets unique result."""
        # This is THE test that catches the artifact path collision bug.
        # If all branches write to the same path, results will be wrong.
        f = write_module(
            tmp_path,
            "pipeline.py",
            """
            from functools import partial
            from barca import task, parallel

            @task()
            def work(i: int) -> dict:
                return {"i": i}

            @task()
            def fan_out() -> list:
                return parallel(*(partial(work, i) for i in range(10)))
            """,
        )
        result = barca.run("fan_out", f)
        assert isinstance(result, list)
        assert len(result) == 10
        # INVARIANT: each result has a unique value matching its input
        values = [r["i"] for r in result]
        assert values == list(range(10)), f"Got {values}, expected [0..9]"

    def test_wall_clock_parallelism(self, tmp_path):
        """4 branches x 500ms each should complete in well under sequential time."""
        import time

        f = write_module(
            tmp_path,
            "pipeline.py",
            """
            import time
            from functools import partial
            from barca import task, parallel

            @task()
            def slow(i: int) -> int:
                time.sleep(0.5)
                return i

            @task()
            def fan_out_4() -> list:
                return parallel(*(partial(slow, i) for i in range(4)))
            """,
        )
        t0 = time.time()
        result = barca.run("fan_out_4", f)
        elapsed = time.time() - t0
        # If sequential: 4 * 0.5 = 2.0s of sleeps + ~1.2s fixed engine overhead
        # (binary + DB init, parent freeze/replace, child worker spawn) ≈ 3.2s.
        # If parallel: ~0.7s parallel section + the same overhead ≈ 1.9-2.1s
        # measured on a 4-CPU container. 2.6 clears the deterministic parallel
        # band while staying well under sequential time.
        assert elapsed < 2.6, f"Took {elapsed:.2f}s — parallel is not actually parallel!"
        assert len(result) == 4
        assert set(result) == {0, 1, 2, 3}

    def test_crash_mid_batch_no_silent_none(self, tmp_path):
        """A crashing branch must surface an error, never silent None/null.

        When a parallel branch calls os._exit(), the worker process dies.
        Barca must NOT silently return None — it should either:
        - Propagate a ParallelError for that branch, or
        - Fail the entire task with a BarcaError (WorkerCrash).
        Either is acceptable; silent None is not.
        """
        f = write_module(
            tmp_path,
            "pipeline.py",
            """
            import os
            from functools import partial
            from barca import task, parallel, ParallelError

            @task()
            def crash() -> int:
                os._exit(137)

            @task()
            def ok_task(i: int) -> int:
                return i

            @task()
            def fan_out() -> list:
                results = parallel(
                    partial(ok_task, 1),
                    partial(crash),
                    partial(ok_task, 3),
                )
                # INVARIANT: every result is either a value or a ParallelError
                # Never None/null silently
                output = []
                for r in results:
                    if isinstance(r, ParallelError):
                        output.append({"error": str(r)})
                    else:
                        output.append(r)
                return output
            """,
        )
        # A crash in a parallel branch should either:
        # 1. Return results with a ParallelError for the crashed branch, or
        # 2. Raise BarcaError for the whole task (worker crash propagation)
        # INVARIANT: it must NOT silently return None for the crashed branch
        try:
            result = barca.run("fan_out", f)
            # If we get results, verify no None values
            assert len(result) == 3
            assert result[0] == 1
            assert result[2] == 3
            # The crashed branch must be an error, not None
            assert result[1] is not None, "Crashed branch returned None — silent data loss!"
            assert "error" in result[1], f"Crashed branch returned {result[1]} instead of error"
        except BarcaError as e:
            # Acceptable: the crash propagated as a hard failure
            assert "crash" in str(e).lower() or "worker" in str(e).lower(), (
                f"BarcaError raised but doesn't mention crash/worker: {e}"
            )

    def test_parallel_map_large_n(self, tmp_path):
        """100 items via parallel_map — all results correct and ordered."""
        f = write_module(
            tmp_path,
            "pipeline.py",
            """
            from functools import partial
            from barca import task, parallel

            @task()
            def square(n: int) -> int:
                return n * n

            @task()
            def compute_squares() -> list:
                return parallel(*(partial(square, i) for i in range(100)))
            """,
        )
        result = barca.run("compute_squares", f)
        assert len(result) == 100
        # INVARIANT: results are ordered and correct
        expected = [i * i for i in range(100)]
        assert result == expected, (
            f"First mismatch at index {next(i for i, (a, b) in enumerate(zip(result, expected)) if a != b)}"
        )

    def test_no_result_is_none_on_success(self, tmp_path):
        """Every successfully-executed branch returns its actual value, never None."""
        f = write_module(
            tmp_path,
            "pipeline.py",
            """
            from functools import partial
            from barca import task, parallel

            @task()
            def identity(x: int) -> int:
                return x

            @task()
            def fan_out() -> list:
                return parallel(*(partial(identity, i) for i in range(20)))
            """,
        )
        result = barca.run("fan_out", f)
        # INVARIANT: no None values when all tasks succeed
        for i, r in enumerate(result):
            assert r is not None, f"result[{i}] is None — silent data loss"
            assert r == i, f"result[{i}] = {r}, expected {i}"

    def test_nested_parallel_standalone(self, tmp_path):
        """Nested parallel() calls work (at least in standalone/sequential mode)."""
        f = write_module(
            tmp_path,
            "pipeline.py",
            """
            from functools import partial
            from barca import task, parallel

            @task()
            def inner_work(i: int) -> int:
                return i * 10

            @task()
            def outer_branch(group: int) -> list:
                return parallel(*(partial(inner_work, group * 10 + i) for i in range(3)))

            @task()
            def nested() -> list:
                return parallel(
                    partial(outer_branch, 0),
                    partial(outer_branch, 1),
                )
            """,
        )
        result = barca.run("nested", f)
        # Outer has 2 branches, each returning a list of 3
        assert len(result) == 2
        assert result[0] == [0, 10, 20]
        assert result[1] == [100, 110, 120]


# ─── Ordering-only deps ─────────────────────────────────────────────────────


class TestOrderingOnlyDeps:
    def test_underscore_prefix_skips_deserialization(self, tmp_path):
        """_-prefixed params receive None instead of the artifact value."""
        f = write_module(
            tmp_path,
            "pipeline.py",
            """
            from barca import asset, task

            @asset()
            def setup() -> dict:
                return {"large": "data"}

            @task(inputs={"_setup": setup})
            def deploy(_setup) -> str:
                assert _setup is None, f"expected None, got {_setup}"
                return "deployed"
            """,
        )
        result = barca.run("deploy", f)
        assert result == "deployed"
