"""SQL selects the real prediction's current identities without deleting history (#288)."""

import json
import os
import sqlite3
import subprocess
from contextlib import closing

import pytest
from barca.api import _find_binary

pytest.importorskip("duckdb")


def cli(root, *args, pool=None, extra_env=None):
    env = {
        key: value
        for key, value in os.environ.items()
        if not key.startswith(("BARCA_", "AWS_", "AZURE_", "GOOGLE_", "FSSPEC_"))
    }
    if pool is not None:
        env["BARCA_POOL_SIZE"] = str(pool)
    env.update(extra_env or {})
    return subprocess.run(
        [_find_binary(), *args, "--json"],
        cwd=root,
        env=env,
        text=True,
        capture_output=True,
        timeout=45,
        check=False,
    )


def ok(result):
    assert result.returncode == 0, result.stderr
    return json.loads(result.stdout)


def pipeline(root, keys, *, file="p.py", value=1):
    source = root / file
    source.parent.mkdir(parents=True, exist_ok=True)
    source.write_text(f"""from barca import asset, partitions
from pathlib import Path
Path('imported').touch()
@asset(partitions={{'k': partitions({keys!r})}})
def part(k): return [{{'k': k, 'value': {value}}}]
""")
    (root / "barca.toml").touch()
    return source


def records(root):
    with closing(sqlite3.connect(root / ".barca/metadata.db")) as db:
        return db.execute("select * from materializations order by id").fetchall()


@pytest.mark.parametrize("pool", [1, 2, None])
@pytest.mark.parametrize("file", ["p.py", "dir[old]/p[old].py"])
def test_current_membership_cached_chunks_and_stale_results_preserve_removed_history(
    tmp_path, pool, file
):
    pipeline(tmp_path, ["a", "b", "c", "d", "e"], file=file)
    assert ok(cli(tmp_path, "get", "part", pool=pool))["steps_executed"] == 5
    before = records(tmp_path)
    removed = [row for row in before if row[1].endswith(("[k=b]", "[k=d]"))]
    assert len(removed) == 2

    pipeline(tmp_path, ["a", "c", "e", "new"], file=file)
    (tmp_path / "imported").unlink()
    query = "select partition,k,value from part order by k"
    result = ok(cli(tmp_path, "sql", query, pool=pool))
    assert result["rows"] == [
        {"partition": f"k={key}", "k": key, "value": 1} for key in ("a", "c", "e")
    ]
    assert records(tmp_path) == before
    assert not (tmp_path / "imported").exists(), "SQL imported the pipeline"
    assert ok(cli(tmp_path, "get", "part", pool=pool))["steps_executed"] == 1

    pipeline(tmp_path, ["a", "c", "e", "new"], file=file, value=17)
    (tmp_path / "imported").unlink()
    stale = cli(tmp_path, "sql", query, pool=pool)
    assert [row["k"] for row in ok(stale)["rows"]] == ["a", "c", "e", "new"]
    assert all(row["value"] == 1 for row in ok(stale)["rows"])
    assert "stale" in stale.stderr
    assert not (tmp_path / "imported").exists()
    assert ok(cli(tmp_path, "get", "part", pool=pool))["steps_executed"] == 4
    assert all(row["value"] == 17 for row in ok(cli(tmp_path, "sql", query, pool=pool))["rows"])
    assert [row for row in records(tmp_path) if row[1].endswith(("[k=b]", "[k=d]"))] == removed


@pytest.mark.parametrize("pool", [1, 2, None])
def test_derived_membership_known_cached_source_vs_unknown_changed_source(tmp_path, pool):
    source = tmp_path / "p.py"
    (tmp_path / "barca.toml").touch()

    def write(keys):
        source.write_text(f"""from barca import asset, partitions_from
from pathlib import Path
Path('imported').touch()
@asset()
def keys(): return {keys!r}
@asset(partitions={{'k': partitions_from(keys)}})
def part(k): return [{{'k':k}}]
@asset()
def unrelated(): return [{{'value':42}}]
""")

    write(["a", "b", "c", "d", "e"])
    ok(cli(tmp_path, "get", ".", pool=pool))
    write(["a", "c", "e", "new"])
    before = records(tmp_path)
    (tmp_path / "imported").unlink()
    unknown = cli(tmp_path, "sql", "select * from part", pool=pool)
    assert unknown.returncode == 2, unknown.stderr
    assert "partition keys" in unknown.stderr.lower() and "source materialization" in unknown.stderr
    assert "keys" in unknown.stderr and "barca get part" in unknown.stderr
    assert ok(cli(tmp_path, "sql", "select * from unrelated", pool=pool))["rows"] == [{"value": 42}]
    assert records(tmp_path) == before and not (tmp_path / "imported").exists()

    ok(cli(tmp_path, "get", "keys", pool=pool))
    (tmp_path / "imported").unlink()
    known = ok(cli(tmp_path, "sql", "select partition,k from part order by k", pool=pool))
    assert known["rows"] == [{"partition": f"k={key}", "k": key} for key in ("a", "c", "e")]
    assert not (tmp_path / "imported").exists()
    assert [row for row in records(tmp_path) if row[1].endswith(("[k=b]", "[k=d]"))] == [
        row for row in before if row[1].endswith(("[k=b]", "[k=d]"))
    ]
    ok(cli(tmp_path, "get", "part", pool=pool))
    assert [
        row["k"]
        for row in ok(cli(tmp_path, "sql", "select k from part order by k", pool=pool))["rows"]
    ] == ["a", "c", "e", "new"]


def test_static_zero_current_keys_has_no_schema_and_preserves_history(tmp_path):
    pipeline(tmp_path, ["a", "b"])
    ok(cli(tmp_path, "get", "part", pool=1))
    before = records(tmp_path)
    pipeline(tmp_path, [])
    (tmp_path / "imported").unlink()
    result = cli(tmp_path, "sql", "select * from part", pool=1)
    assert result.returncode == 2, result.stderr
    assert "no result yet" in result.stderr
    assert "source materialization" not in result.stderr
    assert ok(cli(tmp_path, "sql", "select 42 as value", pool=1))["rows"] == [{"value": 42}]
    assert records(tmp_path) == before and not (tmp_path / "imported").exists()


def test_removed_remote_partitions_never_reach_actual_sql_downloads(tmp_path):
    pipeline(tmp_path, ["a", "b", "c", "d", "e"])
    ok(cli(tmp_path, "get", "part", pool=2))
    sources = {}
    removed = set()
    with closing(sqlite3.connect(tmp_path / ".barca/metadata.db")) as db:
        for ident, node, path in db.execute(
            "select id,node_id,artifact_path from materializations"
        ).fetchall():
            uri = "memory://bucket/proj/default/artifacts/" + os.path.basename(path)
            sources[uri] = (tmp_path / path).read_text()
            if node.endswith(("[k=b]", "[k=d]")):
                removed.add(uri)
            db.execute("update materializations set artifact_path=? where id=?", (uri, ident))
        db.commit()
    pipeline(tmp_path, ["a", "c", "e"])
    (tmp_path / "barca.toml").write_text('[remote]\nuri="memory://bucket/proj"\nstate="off"\n')
    before = records(tmp_path)
    (tmp_path / "sources.json").write_text(json.dumps(sources))
    shim = tmp_path / "python-startup"
    shim.mkdir()
    (shim / "sitecustomize.py").write_text("""import json
from pathlib import Path
from barca import _storage
import fsspec
fs=fsspec.filesystem('memory')
for uri,content in json.loads(Path('sources.json').read_text()).items():
    fs.pipe_file(uri,content.encode())
class Counted:
    def info(self,uri): return fs.info(uri)
    def get_file(self,uri,dest):
        with Path('downloaded').open('a') as out: out.write(uri+'\\n')
        return fs.get_file(uri,dest)
_storage._fs_cache['memory']=Counted()
""")
    extra = {"PYTHONPATH": str(shim) + os.pathsep + os.environ.get("PYTHONPATH", "")}
    result = ok(cli(tmp_path, "sql", "select k from part order by k", pool=2, extra_env=extra))
    assert result["rows"] == [{"k": key} for key in ("a", "c", "e")]
    downloaded = set((tmp_path / "downloaded").read_text().splitlines())
    assert downloaded == set(sources) - removed
    assert len(downloaded) == 3 and downloaded.isdisjoint(removed)
    assert records(tmp_path) == before


def test_declared_view_name_keeps_current_membership_and_canonical_history(tmp_path):
    file = "dir[old]/p[old].py"

    def write(keys):
        source = pipeline(tmp_path, keys, file=file)
        source.write_text(source.read_text().replace("@asset(", "@asset(name='current_view', "))

    write(["a", "b", "c"])
    assert ok(cli(tmp_path, "get", "current_view"))["steps_executed"] == 3
    before = records(tmp_path)
    assert {row[1] for row in before} == {f"current_view[k={key}]" for key in ("a", "b", "c")}
    removed = [row for row in before if row[1] == "current_view[k=b]"]
    write(["a", "c", "new"])
    (tmp_path / "imported").unlink()
    result = ok(cli(tmp_path, "sql", "select partition,k from current_view order by k"))
    assert result["rows"] == [{"partition": f"k={key}", "k": key} for key in ("a", "c")]
    assert records(tmp_path) == before
    assert not (tmp_path / "imported").exists()
    assert ok(cli(tmp_path, "get", "current_view"))["steps_executed"] == 1
    result = ok(cli(tmp_path, "sql", "select partition,k from current_view order by k"))
    assert result["rows"] == [{"partition": f"k={key}", "k": key} for key in ("a", "c", "new")]
    assert [row for row in records(tmp_path) if row[1] == "current_view[k=b]"] == removed
