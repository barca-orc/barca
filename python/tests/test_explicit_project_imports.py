"""Actual import history, source identity and durable pickle contracts (#295)."""

import json
import os
import subprocess
import sys

import pytest
from barca.api import _find_binary

SCRUB = ("BARCA_", "AWS_", "AZURE_", "GOOGLE_", "FSSPEC_")


def env(pool=None):
    values = {k: v for k, v in os.environ.items() if not k.startswith(SCRUB)}
    if pool is not None:
        values["BARCA_POOL_SIZE"] = str(pool)
    return values


def cli(root, *args, pool=None):
    return subprocess.run(
        [_find_binary(), *args],
        cwd=root,
        env=env(pool),
        capture_output=True,
        text=True,
        timeout=45,
        check=False,
    )


def write(root, name, source):
    file = root / name
    file.parent.mkdir(parents=True, exist_ok=True)
    file.write_text(source)
    (root / "barca.toml").touch()
    return file


def success(result):
    assert result.returncode == 0, result.stderr
    return json.loads(result.stdout.strip().splitlines()[-1])


@pytest.mark.parametrize("pool", [1, 2, None])
@pytest.mark.parametrize("other_has_helper", [True, False])
def test_conflicting_actual_imports_fail_before_user_code(tmp_path, pool, other_has_helper):
    marker = "from pathlib import Path\nPath('imported').touch()\n"
    pipeline = (
        marker
        + "from barca import asset\nfrom helpers import value\n@asset()\ndef result(): return value()\n"
    )
    write(tmp_path, "a/p.py", pipeline)
    write(tmp_path, "b/p.py", pipeline)
    write(tmp_path, "a/helpers.py", "def value(): return 11\n")
    if other_has_helper:
        write(tmp_path, "b/helpers.py", "def value(): return 22\n")
    for command in ("plan", "get"):
        result = cli(tmp_path, command, ".", pool=pool)
        assert result.returncode == 2, result.stderr
        assert "helpers" in result.stderr and "qualified" in result.stderr
        assert "a/helpers.py" in result.stderr
        assert not (tmp_path / "imported").exists()
        assert not (tmp_path / ".barca/default/metadata.db").exists()


def test_explicit_node_input_off_path_is_refused_before_imports(tmp_path):
    write(
        tmp_path,
        "a/p.py",
        "from pathlib import Path\nPath('imported').touch()\nfrom barca import asset\nfrom seed import seed\n@asset(inputs={'x': seed})\ndef result(x): return x\n",
    )
    write(tmp_path, "b/seed.py", "from barca import asset\n@asset()\ndef seed(): return 11\n")
    result = cli(tmp_path, "plan", ".")
    assert result.returncode == 2 and "off-path" in result.stderr, result.stderr
    assert "b.seed" in result.stderr and "b/seed.py" in result.stderr
    assert not (tmp_path / "imported").exists()


def test_unrelated_off_path_stem_does_not_outlaw_stdlib(tmp_path):
    write(
        tmp_path,
        "p.py",
        "import json\nfrom barca import asset\n@asset()\ndef result(): return json.loads('41')\n",
    )
    write(
        tmp_path, "pipelines/json.py", "from barca import asset\n@asset()\ndef other(): return 7\n"
    )
    result = success(cli(tmp_path, "get", "result", "."))
    assert result["final_output"] == 41


@pytest.mark.parametrize("pool", [1, 2, None])
def test_qualified_helpers_cold_warm_edit_keep_selective_hashes(tmp_path, pool):
    for directory, value in (("a", 11), ("b", 22)):
        write(tmp_path, f"{directory}/helpers.py", f"def value(): return {value}\n")
        write(
            tmp_path,
            f"{directory}/p.py",
            f"from barca import asset\nfrom {directory}.helpers import value\n@asset()\ndef {directory}(): return value()\n",
        )
    cold = success(cli(tmp_path, "get", "a", ".", pool=pool))
    assert cold["final_output"] == 11
    assert success(cli(tmp_path, "get", "b", ".", pool=pool))["final_output"] == 22
    assert success(cli(tmp_path, "get", "a", ".", pool=pool))["steps_executed"] == 0
    write(tmp_path, "b/helpers.py", "def value(): return 33\n")
    assert success(cli(tmp_path, "get", "a", ".", pool=pool))["steps_executed"] == 0
    edited = success(cli(tmp_path, "get", "b", ".", pool=pool))
    assert edited["final_output"] == 33 and edited["steps_executed"] == 1


@pytest.mark.parametrize("pool", [1, 2, None])
@pytest.mark.parametrize("prefix", ["", "ns/", "pkg/"])
def test_pipeline_imports_share_identity_and_setup_once(tmp_path, pool, prefix):
    if prefix == "pkg/":
        write(tmp_path, "pkg/__init__.py", "")
    module = (prefix + "p").replace("/", ".")
    source = """from barca import asset
from dataclasses import dataclass
from pathlib import Path
import os
with Path('setup').open('a') as f: f.write(str(os.getpid())+'\\n')
@dataclass
class Record:
    value: int
@asset(serializer='pickle')
def seed(): return Record(11)
"""
    write(tmp_path, prefix + "p.py", source)
    write(
        tmp_path,
        "c.py",
        f"from barca import asset\nfrom {module} import seed, Record\n@asset(inputs={{'x': seed}})\ndef result(x): return {{'same': type(x) is Record, 'module': type(x).__module__}}\n",
    )
    expected = {"same": True, "module": module}
    assert success(cli(tmp_path, "get", "result", ".", pool=pool))["final_output"] == expected
    assert success(cli(tmp_path, "get", "result", ".", pool=pool))["steps_executed"] == 0
    refreshed = success(
        cli(tmp_path, "get", "result", ".", "--refresh", "result", "--no-cascade", pool=pool)
    )
    assert refreshed["final_output"] == expected and refreshed["steps_executed"] == 1
    pids = (tmp_path / "setup").read_text().splitlines()
    assert len(pids) == len(set(pids)), "setup repeated within one worker process"


LEGACY_SOURCE = """from pathlib import Path
from dataclasses import dataclass
import os
with Path('setup').open('a') as f: f.write(str(os.getpid())+'\\n')
@dataclass
class Record:
    value: int
"""


def legacy_artifact(root, relative, old_name):
    file = write(root, relative, LEGACY_SOURCE)
    # This invokes the historical source-import entrypoint/name explicitly.
    # The separate actual old-CLI verification establishes producer provenance.
    script = "from barca._source_import import load_source_module\nimport pickle,sys\nm=load_source_module(sys.argv[1],sys.argv[2])\nwith open('old.pkl','wb') as f: pickle.dump(m.Record(17),f,protocol=5)\n"
    result = subprocess.run(
        [sys.executable, "-c", script, str(file), old_name],
        cwd=root,
        env=env(),
        capture_output=True,
        text=True,
        check=False,
    )
    assert result.returncode == 0, result.stderr
    return root / "old.pkl"


@pytest.mark.parametrize(
    "relative,old_name,canonical",
    [
        ("p.py", "_barca_p", "p"),
        ("a/b/p.py", "_barca_a__b__p", "a.b.p"),
        ("a__b/p.py", "_barca_a__b__p", "a__b.p"),
        ("a/b__p.py", "_barca_a__b__p", "a.b__p"),
    ],
)
def test_cold_legacy_pickle_api_and_concurrent_reads_share_module(
    tmp_path, relative, old_name, canonical
):
    artifact = legacy_artifact(tmp_path, relative, old_name)
    script = """from concurrent.futures import ThreadPoolExecutor
from barca.api import _read_output
from barca._source_import import load_pipeline_module
import json,sys
ref={'_barca_artifact':{'path':'old.pkl','format':'pickle'}}
with ThreadPoolExecutor(max_workers=8) as ex: values=list(ex.map(lambda _: _read_output(ref),range(16)))
m=load_pipeline_module(sys.argv[1])
print(json.dumps({'values':[x.value for x in values],'same':all(type(x) is m.Record for x in values),'module':m.Record.__module__}))
"""
    result = subprocess.run(
        [sys.executable, "-c", script, str(tmp_path / relative)],
        cwd=tmp_path,
        env=env(),
        capture_output=True,
        text=True,
        check=False,
    )
    got = success(result)
    assert got == {"values": [17] * 16, "same": True, "module": canonical}
    assert artifact.exists()
    pids = (tmp_path / "setup").read_text().splitlines()
    assert len(pids) == len(set(pids)), "concurrent or later loading repeated setup"


def test_ambiguous_legacy_encoding_preserves_artifact_without_setup(tmp_path):
    artifact = legacy_artifact(tmp_path, "a__b/p.py", "_barca_a__b__p")
    original = artifact.read_bytes()
    write(tmp_path, "a/b__p.py", LEGACY_SOURCE)
    before = (tmp_path / "setup").read_bytes()
    script = "from barca._artifacts import deserialize\ndeserialize('old.pkl','pickle')\n"
    result = subprocess.run(
        [sys.executable, "-c", script],
        cwd=tmp_path,
        env=env(),
        capture_output=True,
        text=True,
        check=False,
    )
    assert result.returncode != 0 and "ambiguous" in result.stderr
    assert "refresh" in result.stderr
    assert artifact.read_bytes() == original and (tmp_path / "setup").read_bytes() == before


def test_legacy_exhaustion_never_accepts_partial_unique_candidate(tmp_path):
    artifact = legacy_artifact(tmp_path, "a__b__p.py", "_barca_a__b__p")
    original = artifact.read_bytes()
    before = (tmp_path / "setup").read_bytes()
    script = "from barca import _source_import\n_source_import._LEGACY_PROBE_LIMIT=2\nfrom barca._artifacts import deserialize\ndeserialize('old.pkl','pickle')\n"
    result = subprocess.run(
        [sys.executable, "-c", script],
        cwd=tmp_path,
        env=env(),
        capture_output=True,
        text=True,
        check=False,
    )
    assert result.returncode != 0 and "recovery bound" in result.stderr
    assert "refresh" in result.stderr
    assert artifact.read_bytes() == original and (tmp_path / "setup").read_bytes() == before


def test_missing_legacy_source_preserves_artifact(tmp_path):
    artifact = legacy_artifact(tmp_path, "p.py", "_barca_p")
    original = artifact.read_bytes()
    (tmp_path / "p.py").unlink()
    result = subprocess.run(
        [
            sys.executable,
            "-c",
            "from barca._artifacts import deserialize;deserialize('old.pkl','pickle')",
        ],
        cwd=tmp_path,
        env=env(),
        capture_output=True,
        text=True,
        check=False,
    )
    assert result.returncode != 0 and "unavailable" in result.stderr and "refresh" in result.stderr
    assert artifact.read_bytes() == original


def test_ordinary_module_with_legacy_prefix_keeps_python_identity(tmp_path):
    write(tmp_path, "_barca_custom.py", LEGACY_SOURCE)
    script = "from barca._source_import import load_pipeline_module\nfrom barca._artifacts import serialize,deserialize\nm=load_pipeline_module('_barca_custom.py')\nserialize(m.Record(19),'new.pkl','pickle')\nassert type(deserialize('new.pkl','pickle')) is m.Record\nassert m.Record.__module__=='_barca_custom'\n"
    result = subprocess.run(
        [sys.executable, "-c", script],
        cwd=tmp_path,
        env=env(),
        capture_output=True,
        text=True,
        check=False,
    )
    assert result.returncode == 0, result.stderr


def test_unambiguous_sibling_helper_and_explicit_reload_remain_python(tmp_path):
    write(tmp_path, "sub/helpers.py", "def value(): return 11\n")
    write(
        tmp_path,
        "sub/p.py",
        "from barca import asset\nfrom helpers import value\n@asset()\ndef result(): return value()\n",
    )
    assert success(cli(tmp_path, "get", "result", "."))["final_output"] == 11
    script = """from barca._source_import import load_pipeline_module
from pathlib import Path
import importlib
m=load_pipeline_module('sub/p.py')
import helpers
Path('sub/helpers.py').write_text('def value(): return 22\\n')
importlib.reload(helpers)
assert helpers.value()==22
"""
    result = subprocess.run(
        [sys.executable, "-c", script],
        cwd=tmp_path,
        env=env(),
        capture_output=True,
        text=True,
        check=False,
    )
    assert result.returncode == 0, result.stderr


def test_qualified_pipeline_duckdb_setup_shared_with_consumer(tmp_path):
    write(
        tmp_path,
        "p.py",
        """from barca import asset
from barca import _duckdb
import duckdb
con=_duckdb.connection()
con.execute('CREATE TABLE setup_once AS SELECT 7 AS n')
@asset()
def seed() -> duckdb.DuckDBPyRelation: return con.sql('SELECT * FROM setup_once')
""",
    )
    write(
        tmp_path,
        "c.py",
        """from barca import asset
from p import seed,con
import duckdb
@asset(inputs={'x': seed})
def result(x: duckdb.DuckDBPyRelation):
    return {'n':x.fetchall()[0][0], 'setup':con.sql('SELECT * FROM setup_once').fetchall()[0][0]}
""",
    )
    assert success(cli(tmp_path, "get", "result", ".", pool=1))["final_output"] == {
        "n": 7,
        "setup": 7,
    }
    assert success(
        cli(tmp_path, "get", "result", ".", "--refresh", "result", "--no-cascade", pool=1)
    )["final_output"] == {"n": 7, "setup": 7}


@pytest.mark.parametrize("wrong_alias", ["object", "source"])
def test_outside_root_registered_legacy_alias_requires_identity_proof(tmp_path, wrong_alias):
    reader = tmp_path / "reader"
    reader.mkdir()
    source = write(tmp_path, "p.py", LEGACY_SOURCE)
    other = write(tmp_path, "q.py", LEGACY_SOURCE)
    script = """from barca._source_import import load_source_module
from barca._artifacts import deserialize
from types import SimpleNamespace
import pickle,sys
p=load_source_module(sys.argv[1],'_barca_p')
with open('old.pkl','wb') as f: pickle.dump(p.Record(17),f)
original=open('old.pkl','rb').read()
assert deserialize('old.pkl','pickle').value==17
if sys.argv[3]=='object':
    sys.modules['_barca_p']=SimpleNamespace(__file__=p.__file__,Record=p.Record)
else:
    q=load_source_module(sys.argv[2],'_barca_q')
    sys.modules['_barca_p']=q
try:
    deserialize('old.pkl','pickle')
except ImportError as e:
    assert 'unavailable' in str(e) and 'refresh' in str(e),str(e)
else:
    raise AssertionError('mismatched legacy alias was accepted')
assert open('old.pkl','rb').read()==original
"""
    result = subprocess.run(
        [sys.executable, "-c", script, str(source), str(other), wrong_alias],
        cwd=reader,
        env=env(),
        capture_output=True,
        text=True,
        check=False,
    )
    assert result.returncode == 0, result.stderr


def test_registered_inside_root_legacy_alias_does_not_bypass_uniqueness(tmp_path):
    source = write(tmp_path, "a__b/p.py", LEGACY_SOURCE)
    write(tmp_path, "a/b__p.py", LEGACY_SOURCE)
    script = """from barca._source_import import load_source_module
from barca._artifacts import deserialize
import pickle,sys
p=load_source_module(sys.argv[1],'_barca_a__b__p')
with open('old.pkl','wb') as f: pickle.dump(p.Record(17),f)
try:
    deserialize('old.pkl','pickle')
except ImportError as e:
    assert 'ambiguous' in str(e),str(e)
else:
    raise AssertionError('registered alias bypassed exhaustive proof')
"""
    result = subprocess.run(
        [sys.executable, "-c", script, str(source)],
        cwd=tmp_path,
        env=env(),
        capture_output=True,
        text=True,
        check=False,
    )
    assert result.returncode == 0, result.stderr


def test_valid_qualified_pipelines_survive_ambiguous_legacy_alias(tmp_path):
    for relative, name, value in [("a__b/p.py", "one", 11), ("a/b__p.py", "two", 22)]:
        write(
            tmp_path, relative, f"from barca import asset\n@asset()\ndef {name}(): return {value}\n"
        )
    result = success(cli(tmp_path, "get", ".", pool=1))
    assert result["steps_executed"] == 2
    assert success(cli(tmp_path, "get", "one", ".", pool=1))["final_output"] == 11
    assert success(cli(tmp_path, "get", "two", ".", pool=1))["final_output"] == 22


def test_import_setup_thread_can_import_another_project_helper(tmp_path):
    write(tmp_path, "sub/helpers.py", "value = 31\n")
    write(
        tmp_path,
        "sub/p.py",
        """from threading import Thread
from barca import asset
values=[]
def setup():
    from helpers import value
    values.append(value)
thread=Thread(target=setup,daemon=True)
thread.start()
thread.join(2)
assert not thread.is_alive(), 'helper import deadlocked during setup'
@asset()
def result(): return values[0]
""",
    )
    assert success(cli(tmp_path, "get", "result", ".", pool=1))["final_output"] == 31


def test_daemon_restores_consumer_path_after_cold_legacy_input(tmp_path):
    artifact = legacy_artifact(tmp_path, "producer/p.py", "_barca_producer__p")
    write(tmp_path, "consumer/helpers.py", "value = 31\n")
    consumer = write(
        tmp_path,
        "consumer/c.py",
        """def result(x):
    from helpers import value
    from producer.p import Record
    assert type(x) is Record
    return x.value + value
""",
    )
    script = """from barca._worker import _run_daemon_step, _ArtifactLRU
from barca._artifacts import deserialize
from barca import _runtime
import sys,socket
_runtime._socket,peer=socket.socketpair()
step={'node_id':'result','kind':'asset','function_name':'result','source_file':sys.argv[1],
      'inputs':{'x':'old.pkl'}}
ok=_run_daemon_step(step,{},'outputs',_ArtifactLRU())
assert ok,peer.recv(65536)
assert deserialize('outputs/result.json','json')==48
"""
    result = subprocess.run(
        [sys.executable, "-c", script, str(consumer)],
        cwd=tmp_path,
        env=env(),
        capture_output=True,
        text=True,
        timeout=15,
        check=False,
    )
    assert result.returncode == 0, result.stderr
    assert artifact.exists()
    pids = (tmp_path / "setup").read_text().splitlines()
    assert len(pids) == len(set(pids)), "cached producer setup repeated"


def test_external_module_with_legacy_prefix_retains_normal_pickle_import(tmp_path):
    project = tmp_path / "project"
    project.mkdir()
    external = tmp_path / "external"
    external.mkdir()
    (external / "_barca_extension.py").write_text("class Record:\n    value=37\n")
    script = """import sys,pickle
sys.path.insert(0,sys.argv[1])
import _barca_extension
with open('external.pkl','wb') as f: pickle.dump(_barca_extension.Record(),f)
del sys.modules['_barca_extension']
from barca._artifacts import deserialize
assert deserialize('external.pkl','pickle').value==37
"""
    result = subprocess.run(
        [sys.executable, "-c", script, str(external)],
        cwd=project,
        env=env(),
        capture_output=True,
        text=True,
        timeout=15,
        check=False,
    )
    assert result.returncode == 0, result.stderr


def test_legacy_producer_missing_dependency_preserves_original_error(tmp_path):
    artifact = legacy_artifact(tmp_path, "p.py", "_barca_p")
    (tmp_path / "p.py").write_text("import unavailable_barca_test_dependency\n" + LEGACY_SOURCE)
    result = subprocess.run(
        [
            sys.executable,
            "-c",
            "from barca._artifacts import deserialize;deserialize('old.pkl','pickle')",
        ],
        cwd=tmp_path,
        env=env(),
        capture_output=True,
        text=True,
        timeout=15,
        check=False,
    )
    assert result.returncode != 0
    assert "No module named 'unavailable_barca_test_dependency'" in result.stderr
    assert "legacy project identity" not in result.stderr
    assert artifact.exists()


@pytest.mark.parametrize("protocol", [2, 3, 4, 5])
def test_legacy_nested_class_uses_ordinary_pickle_qualified_name_resolution(tmp_path, protocol):
    source = write(
        tmp_path,
        "p.py",
        """from pathlib import Path
import os
with Path('setup').open('a') as f: f.write(str(os.getpid())+'\\n')
class Outer:
    class Record:
        value=17
""",
    )
    writer = """from barca._source_import import load_source_module
import pickle,sys
p=load_source_module(sys.argv[1],'_barca_p')
with open('old.pkl','wb') as f: pickle.dump(p.Outer.Record(),f,protocol=int(sys.argv[2]))
"""
    result = subprocess.run(
        [sys.executable, "-c", writer, str(source), str(protocol)],
        cwd=tmp_path,
        env=env(),
        capture_output=True,
        text=True,
        timeout=15,
        check=False,
    )
    assert result.returncode == 0, result.stderr
    original = (tmp_path / "old.pkl").read_bytes()
    assert b"_barca_p" in original
    reader = """from barca.api import _read_output
from barca._source_import import load_pipeline_module
x=_read_output({'_barca_artifact':{'path':'old.pkl','format':'pickle'}})
p=load_pipeline_module('p.py')
assert x.value==17 and type(x) is p.Outer.Record
assert type(x).__module__=='p' and type(x).__qualname__=='Outer.Record'
"""
    result = subprocess.run(
        [sys.executable, "-c", reader],
        cwd=tmp_path,
        env=env(),
        capture_output=True,
        text=True,
        timeout=15,
        check=False,
    )
    assert result.returncode == 0, result.stderr
    assert (tmp_path / "old.pkl").read_bytes() == original
    pids = (tmp_path / "setup").read_text().splitlines()
    assert len(pids) == len(set(pids)), "nested-class recovery repeated setup"


def test_import_setup_thread_can_read_an_unrelated_legacy_producer(tmp_path):
    artifact = legacy_artifact(tmp_path, "p.py", "_barca_p")
    original = artifact.read_bytes()
    write(tmp_path, "consumer/helpers.py", "value=3\n")
    write(
        tmp_path,
        "consumer/pipeline.py",
        """from threading import Thread
from barca import asset
from barca.api import _read_output
values=[]
def setup():
    values.append(_read_output({'_barca_artifact':{'path':'old.pkl','format':'pickle'}}))
thread=Thread(target=setup,daemon=True)
thread.start()
thread.join(2)
assert not thread.is_alive(), 'unrelated legacy producer recovery deadlocked during setup'
from helpers import value
assert value==3
@asset()
def result():
    from p import Record
    assert type(values[0]) is Record
    return values[0].value
""",
    )
    assert success(cli(tmp_path, "get", "result", ".", pool=1))["final_output"] == 17
    assert artifact.read_bytes() == original
    pids = (tmp_path / "setup").read_text().splitlines()
    assert len(pids) == len(set(pids)), "threaded legacy recovery repeated setup"


def test_concurrent_explicit_path_loads_never_return_a_partial_alias_object(tmp_path):
    source = write(
        tmp_path,
        "p.py",
        """from pathlib import Path
import os,time
with Path('setup').open('a') as f: f.write(str(os.getpid())+'\\n')
time.sleep(0.05)
class Record:
    value=17
""",
    )
    script = """from concurrent.futures import ThreadPoolExecutor
from barca._source_import import load_source_module
import sys
with ThreadPoolExecutor(max_workers=8) as ex:
    values=list(ex.map(lambda _:load_source_module(sys.argv[1],'_barca_p'),range(16)))
assert all(value.Record.value==17 and value is values[0] for value in values)
"""
    result = subprocess.run(
        [sys.executable, "-c", script, str(source)],
        cwd=tmp_path,
        env=env(),
        capture_output=True,
        text=True,
        timeout=15,
        check=False,
    )
    assert result.returncode == 0, result.stderr
    assert len((tmp_path / "setup").read_text().splitlines()) == 1
