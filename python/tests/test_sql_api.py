"""Python SQL wrapper preserves the CLI result and routing options."""

import barca
from barca import api


def test_sql_defaults(monkeypatch):
    result = {"columns": ["n"], "rows": [{"n": 1}], "total": 1, "truncated": False}
    calls = []
    monkeypatch.setattr(api, "_exec", lambda args: calls.append(args) or result)
    assert barca.sql("select 1 as n") is result
    assert calls == [["sql", "select 1 as n", "--json", "--limit", "100"]]


def test_sql_options(monkeypatch):
    calls = []
    monkeypatch.setattr(api, "_exec", lambda args: calls.append(args) or {})
    barca.sql("select * from orders", "pipeline.py", all=True, env="dev", server=True)
    assert calls == [
        [
            "sql",
            "select * from orders",
            "pipeline.py",
            "--json",
            "--all",
            "--env",
            "dev",
            "--server",
        ]
    ]
