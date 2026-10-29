"""RFC-0015: retained metadata backups are visible outside the managed cache budget."""

import json

from .test_cache_limit import barca, gc
from .test_serve_robustness import Server


def test_gc_doctor_and_scoped_status_distinguish_backups_from_budget(tmp_path):
    (tmp_path / "barca.toml").write_text('[cache]\nmax_size = "10MB"\n')
    (tmp_path / "pipeline.py").write_text(
        "from barca import asset\n@asset()\ndef value():\n    return 42\n"
    )
    barca(tmp_path, "get", "pipeline.py", "--json")
    for rel, size in [
        ("backups/2026-10-20T00-00-00Z.db", 12_000_000),
        ("envs/test/backups/2026-10-20T00-00-00Z.db", 1000),
        ("envs/test/backups/.backup-123.tmp", 1000),
    ]:
        path = tmp_path / ".barca" / rel
        path.parent.mkdir(parents=True, exist_ok=True)
        path.write_bytes(bytes(size))
    result = gc(tmp_path)
    assert result["backup_bytes"] == 12_002_000
    assert result["size_bytes"] > result["limit_bytes"] > result["budget_before_bytes"]
    assert result["evicted_files"] == 0
    assert result["after_bytes"] == result["backup_bytes"] + result["budget_after_bytes"]
    assert next(h for h in result["held"] if h["what"] == "backups")["evictable"] is False

    doctor = json.loads(barca(tmp_path, "doctor", "--json").stdout)
    cache = next(c for c in doctor["checks"] if c["name"] == "cache_size")
    assert cache["ok"]
    usage = cache["data"]
    assert usage["size_bytes"] == usage["budget_bytes"] + usage["backup_bytes"]
    assert usage["backup_bytes"] == result["backup_bytes"]
    assert "outside the budget" in cache["detail"]

    usage = json.loads(barca(tmp_path, "status", "value", "pipeline.py", "--json").stdout)[
        "disk_usage"
    ]
    assert usage["budget_bytes"] == result["budget_after_bytes"]
    assert usage["backup_bytes"] == result["backup_bytes"]
    assert usage["size_bytes"] == usage["budget_bytes"] + usage["backup_bytes"]
    pretty = barca(tmp_path, "status", "value", "pipeline.py", "--pretty").stdout
    assert "metadata backups outside the budget" in pretty


def test_status_before_a_cache_measurement_reports_unknown_without_creating_state(tmp_path):
    (tmp_path / "barca.toml").write_text("")
    (tmp_path / "pipeline.py").write_text(
        "from barca import asset\n@asset()\ndef value():\n    return 42\n"
    )
    out = barca(tmp_path, "status", "pipeline.py", "--json")
    usage = json.loads(out.stdout)["disk_usage"]
    assert usage["size_bytes"] is None and usage["budget_bytes"] is None
    assert usage["backup_bytes"] == 0
    assert not (tmp_path / ".barca").exists()


def test_server_status_reports_server_storage_instead_of_client_storage(tmp_path):
    client, deployed = tmp_path / "client", tmp_path / "deployed"
    for root, backup_bytes in [(client, 7), (deployed, 500)]:
        root.mkdir()
        (root / "barca.toml").write_text('[cache]\nmax_size = "10MB"\n')
        (root / "pipeline.py").write_text(
            "from barca import asset\n@asset()\ndef value():\n    return 42\n"
        )
        backup = root / ".barca/backups/2026-10-20T00-00-00Z.db"
        backup.parent.mkdir(parents=True)
        backup.write_bytes(bytes(backup_bytes))
    server = Server(deployed)
    try:
        local = json.loads(barca(client, "status", "pipeline.py", "--json").stdout)
        remote = json.loads(
            barca(
                client,
                "status",
                "pipeline.py",
                "--json",
                "--server",
                f"http://127.0.0.1:{server.port}",
            ).stdout
        )
        assert local["disk_usage"]["backup_bytes"] == 7
        assert remote["disk_usage"]["backup_bytes"] == 500
        assert local["root"] != remote["root"]
    finally:
        server.stop()
