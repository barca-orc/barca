"""Extensive Python API parity tests — mirrors CLI integration tests.

Every scenario tested via bash integration tests is also tested here
via the Python API to ensure barca.get/plan return correct values.
"""

import textwrap

import barca
import pandas as pd
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


# ─── Basic execution (mirrors test_cli.sh) ───────────────────────────────────


class TestGetAll:
    """Test `barca.get(file)` — get all assets (no target)."""

    def test_trivial_asset(self, tmp_path):
        f = write_module(
            tmp_path,
            "trivial.py",
            """
            from barca import asset

            @asset()
            def hello():
                return {"msg": "hello"}
        """,
        )
        result = barca.get(f)
        assert result == {"msg": "hello"}

    def test_linear_chain(self, tmp_path):
        f = write_module(
            tmp_path,
            "chain.py",
            """
            from barca import asset

            @asset()
            def a():
                return {"value": 1}

            @asset(inputs={"data": a})
            def b(data):
                return {"value": data["value"] + 10}

            @asset(inputs={"data": b})
            def c(data):
                return {"value": data["value"] * 2}
        """,
        )
        result = barca.get(f)
        assert result == {"value": 22}

    def test_fan_out_merge(self, tmp_path):
        f = write_module(
            tmp_path,
            "fanout.py",
            """
            from barca import asset

            @asset()
            def a():
                return {"x": 1}

            @asset()
            def b():
                return {"x": 2}

            @asset()
            def c():
                return {"x": 3}

            @asset(inputs={"a": a, "b": b, "c": c})
            def merge(a, b, c):
                return {"sum": a["x"] + b["x"] + c["x"]}
        """,
        )
        result = barca.get(f)
        assert result == {"sum": 6}

    def test_aliased_inputs(self, tmp_path):
        f = write_module(
            tmp_path,
            "alias.py",
            """
            from barca import asset

            @asset()
            def raw_prices():
                return {"price": 100}

            @asset(inputs={"data": raw_prices})
            def normalized(data):
                return {"normalized_price": data["price"] / 100}
        """,
        )
        result = barca.get(f)
        assert result["normalized_price"] == 1.0

    def test_multi_file(self, tmp_path):
        f1 = write_module(
            tmp_path,
            "file1.py",
            """
            from barca import asset

            @asset()
            def from_file1():
                return {"source": "file1"}
        """,
        )
        f2 = write_module(
            tmp_path,
            "file2.py",
            """
            from barca import asset

            @asset()
            def from_file2():
                return {"source": "file2"}
        """,
        )
        # get all assets across both files
        result = barca.get(f1, f2)
        assert result is not None

    def test_sensor_unpacking(self, tmp_path):
        f = write_module(
            tmp_path,
            "sensor.py",
            """
            from barca import asset, sensor

            @sensor()
            def check_temp():
                return (True, {"temp": 72})

            @asset(inputs={"data": check_temp})
            def process(data):
                return {"value": data["temp"] * 2}
        """,
        )
        result = barca.get(f)
        assert result == {"value": 144}


# ─── Tasks ──────────────────────────────────────────────────────────────────────


class TestTask:
    """Test `@task` — always re-runs, never cached; assets may feed tasks."""

    def test_task_consumes_asset(self, tmp_path):
        f = write_module(
            tmp_path,
            "deploy.py",
            """
            from barca import asset, task

            @asset()
            def model():
                return {"weights": [1, 2, 3]}

            @task(inputs={"m": model})
            def deploy(m):
                return {"deployed": len(m["weights"])}
            """,
        )
        assert barca.run("deploy", f) == {"deployed": 3}

    def test_task_after_ordering(self, tmp_path):
        f = write_module(
            tmp_path,
            "chain.py",
            """
            from barca import task

            @task()
            def migrate():
                return {"step": "migrate"}

            @task(after=[migrate])
            def warm_cache():
                return {"step": "warm_cache"}

            @task(after=[warm_cache])
            def notify():
                return {"step": "notify"}
            """,
        )
        # The whole after-chain runs; targeting notify scopes to the subtree.
        assert barca.run("notify", f) == {"step": "notify"}

    def test_task_always_reruns(self, tmp_path):
        f = write_module(
            tmp_path,
            "rerun.py",
            """
            from barca import task

            @task()
            def t():
                return {"ok": True}
            """,
        )
        # Two consecutive runs both execute the task (never cached).
        barca.run("t", f)
        result = barca.run("t", f)
        assert result == {"ok": True}

    def test_task_cannot_feed_asset(self, tmp_path):
        f = write_module(
            tmp_path,
            "bad.py",
            """
            from barca import asset, task

            @task()
            def t():
                return {"x": 1}

            @asset(inputs={"data": t})
            def a(data):
                return data
            """,
        )
        with pytest.raises(BarcaError, match="task"):
            barca.get("a", f)


# ─── Get command ──────────────────────────────────────────────────────────────


class TestGet:
    def test_get_returns_value(self, tmp_path):
        f = write_module(
            tmp_path,
            "chain.py",
            """
            from barca import asset

            @asset()
            def a():
                return {"value": 1}

            @asset(inputs={"data": a})
            def b(data):
                return {"value": data["value"] + 10}

            @asset(inputs={"data": b})
            def c(data):
                return {"value": data["value"] * 2}
        """,
        )
        value = barca.get("c", f)
        assert value == {"value": 22}

    def test_get_sensor_pipeline(self, tmp_path):
        f = write_module(
            tmp_path,
            "sensor.py",
            """
            from barca import asset, sensor

            @sensor()
            def check():
                return (True, {"status": "ok"})

            @asset(inputs={"data": check})
            def report(data):
                return {"result": data["status"]}
        """,
        )
        value = barca.get("report", f)
        assert value == {"result": "ok"}


# ─── Caching (mirrors test_cache.sh) ─────────────────────────────────────────


class TestCaching:
    def _write_chain(self, tmp_path, a_value=1, b_op="+ 10", c_op="* 2"):
        return write_module(
            tmp_path,
            "chain.py",
            f"""
            from barca import asset

            @asset()
            def a():
                return {{"value": {a_value}}}

            @asset(inputs={{"data": a}})
            def b(data):
                return {{"value": data["value"] {b_op}}}

            @asset(inputs={{"data": b}})
            def c(data):
                return {{"value": data["value"] {c_op}}}
        """,
        )

    def test_first_get_executes_all(self, tmp_path):
        f = self._write_chain(tmp_path)
        result = barca.api._exec(["get", "c", f])
        assert result["steps_executed"] == 3

    def test_second_get_fully_cached(self, tmp_path):
        f = self._write_chain(tmp_path)
        barca.get("c", f)
        result = barca.api._exec(["get", "c", f])
        assert result["steps_executed"] == 0

    def test_cached_output_matches(self, tmp_path):
        f = self._write_chain(tmp_path)
        v1 = barca.get("c", f)
        v2 = barca.get("c", f)
        assert v1 == v2

    def test_modify_leaf_only_reruns_leaf(self, tmp_path):
        f = self._write_chain(tmp_path)
        barca.get("c", f)
        # Modify c's operation
        f2 = self._write_chain(tmp_path, c_op="* 3")
        result = barca.api._exec(["get", "c", f2])
        assert result["steps_executed"] == 1

    def test_modify_root_reruns_all(self, tmp_path):
        f = self._write_chain(tmp_path)
        barca.get("c", f)
        f2 = self._write_chain(tmp_path, a_value=99)
        result = barca.api._exec(["get", "c", f2])
        assert result["steps_executed"] == 3

    def test_modify_middle_reruns_middle_and_downstream(self, tmp_path):
        f = self._write_chain(tmp_path)
        barca.get("c", f)
        f2 = self._write_chain(tmp_path, b_op="+ 999")
        result = barca.api._exec(["get", "c", f2])
        assert result["steps_executed"] == 2

    def test_partitioned_cache(self, tmp_path):
        # Each partition key is cache-checked on its own run hash.
        f = write_module(
            tmp_path,
            "part.py",
            """
            from barca import asset, partitions

            @asset(partitions={"key": partitions(["a", "b", "c"])})
            def fetch(key):
                return {"key": key}
        """,
        )
        r1 = barca.api._exec(["get", "fetch", f])
        assert r1["steps_executed"] == 3
        r2 = barca.api._exec(["get", "fetch", f])
        assert r2["steps_executed"] == 0


# ─── Format handling ──────────────────────────────────────────────────────────


class TestFormats:
    def test_dict_returns_dict(self, tmp_path):
        f = write_module(
            tmp_path,
            "m.py",
            """
            from barca import asset

            @asset()
            def data():
                return {"key": "value"}
        """,
        )
        assert barca.get("data", f) == {"key": "value"}

    def test_set_returns_set(self, tmp_path):
        f = write_module(
            tmp_path,
            "m.py",
            """
            from barca import asset

            @asset()
            def data():
                return {1, 2, 3}
        """,
        )
        assert barca.get("data", f) == {1, 2, 3}

    def test_dataframe_returns_dataframe(self, tmp_path):
        f = write_module(
            tmp_path,
            "m.py",
            """
            import pandas as pd
            from barca import asset

            @asset()
            def data():
                return pd.DataFrame({"a": [1, 2], "b": [3, 4]})
        """,
        )
        result = barca.get("data", f)
        assert isinstance(result, pd.DataFrame)
        assert list(result.columns) == ["a", "b"]
        assert len(result) == 2

    def test_int_returns_int(self, tmp_path):
        f = write_module(
            tmp_path,
            "m.py",
            """
            from barca import asset

            @asset()
            def data():
                return 42
        """,
        )
        assert barca.get("data", f) == 42

    def test_list_returns_list(self, tmp_path):
        f = write_module(
            tmp_path,
            "m.py",
            """
            from barca import asset

            @asset()
            def data():
                return [1, 2, 3]
        """,
        )
        assert barca.get("data", f) == [1, 2, 3]

    def test_serializer_override(self, tmp_path):
        f = write_module(
            tmp_path,
            "m.py",
            """
            from barca import asset

            @asset(serializer="pickle")
            def data():
                return {"key": "value"}
        """,
        )
        # Should still return the correct value even though format is pickle
        assert barca.get("data", f) == {"key": "value"}


# ─── Subdirectory file paths ──────────────────────────────────────────────────


class TestSubdirectoryPaths:
    """Files in a/b/c.py style paths should work correctly."""

    def _write_nested(self, tmp_path, relpath, code):
        p = tmp_path / relpath
        p.parent.mkdir(parents=True, exist_ok=True)
        p.write_text(textwrap.dedent(code))
        return str(p)

    def test_get_all_from_subdirectory(self, tmp_path):
        f = self._write_nested(
            tmp_path,
            "proj/pipelines/etl.py",
            """
            from barca import asset

            @asset()
            def extract():
                return {"rows": 10}

            @asset(inputs={"data": extract})
            def transform(data):
                return {"rows": data["rows"] * 2}
        """,
        )
        result = barca.get(f)
        assert result == {"rows": 20}

    def test_get_target_from_subdirectory(self, tmp_path):
        f = self._write_nested(
            tmp_path,
            "proj/pipelines/etl.py",
            """
            from barca import asset

            @asset()
            def extract():
                return {"rows": 10}

            @asset(inputs={"data": extract})
            def transform(data):
                return {"rows": data["rows"] * 2}
        """,
        )
        result = barca.get("extract", f)
        assert result == {"rows": 10}

    def test_deeply_nested_file(self, tmp_path):
        f = self._write_nested(
            tmp_path,
            "a/b/c/d/pipeline.py",
            """
            from barca import asset

            @asset()
            def deep():
                return {"depth": 4}
        """,
        )
        result = barca.get(f)
        assert result == {"depth": 4}

    def test_get_with_cross_subdir_import(self, tmp_path):
        self._write_nested(
            tmp_path,
            "proj/utils/math.py",
            """
            def double(x):
                return x * 2
        """,
        )
        f = self._write_nested(
            tmp_path,
            "proj/pipeline.py",
            """
            from barca import asset
            from utils.math import double

            @asset()
            def result():
                return {"value": double(21)}
        """,
        )
        result = barca.get(f)
        assert result == {"value": 42}


# ─── Error cases ──────────────────────────────────────────────────────────────


class TestErrors:
    def test_nonexistent_asset(self, tmp_path):
        f = write_module(
            tmp_path,
            "m.py",
            """
            from barca import asset

            @asset()
            def real():
                return 1
        """,
        )
        with pytest.raises(BarcaError):
            barca.get("nonexistent", f)

    def test_nonexistent_file(self):
        with pytest.raises(BarcaError):
            barca.get("/tmp/definitely_does_not_exist_12345.py")

    def test_get_no_args(self):
        with pytest.raises(TypeError):
            barca.get()


# ─── Plan ─────────────────────────────────────────────────────────────────────


class TestPlan:
    def test_plan_returns_dict(self, tmp_path):
        f = write_module(
            tmp_path,
            "m.py",
            """
            from barca import asset

            @asset()
            def a():
                return 1

            @asset(inputs={"data": a})
            def b(data):
                return data + 1
        """,
        )
        result = barca.plan(f)
        assert "total_steps" in result
        assert result["total_steps"] == 2
        assert "phases" in result
