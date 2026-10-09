"""Declared identities agree across materialization and read-only inspection (#288)."""

import json
import subprocess

import pytest

from barca.api import _find_binary


def call(root, *args):
    result = subprocess.run(
        [_find_binary(), *args, "--json"], cwd=root, text=True, capture_output=True, timeout=30
    )
    assert result.returncode == 0, result.stderr
    return json.loads(result.stdout)


@pytest.mark.parametrize("partitioned", [False, True])
def test_declared_name_is_used_by_status_and_sql_without_reimport(tmp_path, partitioned):
    pytest.importorskip("duckdb")
    (tmp_path / "barca.toml").write_text("")
    partition_arg = ', partitions={"key": partitions(["a", "b"])}' if partitioned else ""
    signature = "key: str" if partitioned else ""
    value = '{"key": key, "value": 42}' if partitioned else '{"value": 42}'
    (tmp_path / "pipeline.py").write_text(
        "from pathlib import Path\n"
        "from barca import asset, partitions\n"
        "Path('imported').touch()\n"
        f"@asset(name='orders_clean'{partition_arg})\n"
        f"def clean({signature}) -> list:\n"
        f"    return [{value}]\n"
    )
    materialized = call(tmp_path, "get", "orders_clean")
    assert materialized["steps_executed"] == (2 if partitioned else 1)
    listed = call(tmp_path, "list", "pipeline.py")
    assert "orders_clean" in json.dumps(listed)
    stats = call(tmp_path, "stats", "orders_clean")
    assert "orders_clean" in json.dumps(stats)
    (tmp_path / "imported").unlink()
    status = call(tmp_path, "status", "pipeline.py")
    assert len(status["nodes"]) == 1
    assert status["nodes"][0]["id"] == "orders_clean"
    assert status["nodes"][0]["name"] == "orders_clean"
    assert not (tmp_path / "imported").exists()
    rows = call(tmp_path, "sql", "SELECT value FROM orders_clean")
    assert rows["rows"] == [{"value": 42}] * (2 if partitioned else 1)
    assert not (tmp_path / "imported").exists()
    warm = call(tmp_path, "get", "orders_clean")
    assert warm["steps_executed"] == 0
