"""Doctor reports prerequisites without importing pipelines or running steps (#388)."""

import json
import os
import shutil
import subprocess
import sys
from pathlib import Path

from barca import __version__
from barca.api import _find_binary


def cli(root, *args, code=0, binary=None, **overrides):
    env = {k: v for k, v in os.environ.items() if not k.startswith("BARCA_")}
    proc = subprocess.run(
        [binary or _find_binary(), "doctor", *args],
        cwd=root,
        env={**env, **overrides},
        capture_output=True,
        text=True,
        timeout=45,
    )
    assert proc.returncode == code, proc.stderr
    return proc


def checks(proc):
    report = json.loads(proc.stdout)
    return report, {check["name"]: check for check in report["checks"]}


def test_local_profile_checks_are_static_and_create_no_metadata(tmp_path):
    (tmp_path / "pipeline.py").write_text("""
from pathlib import Path
from barca import asset, partitions
Path("IMPORTED").touch()
@asset(partitions={"k": partitions(Path("missing-keys").read_text().split())})
def value(k):
    Path("RAN").touch()
    return k
""")
    report, by_name = checks(cli(tmp_path, "--json", BARCA_REMOTE="off", BARCA_ENV="local"))
    assert report["ok"]
    assert all(check["ok"] for check in report["checks"])
    assert Path(by_name["python"]["data"]["executable"]).resolve() == Path(sys.executable).resolve()
    assert by_name["python"]["data"]["barca_version"] == __version__
    assert by_name["project_root"]["data"]["source"] == "cwd"
    assert by_name["environment"]["data"]["name"] == "local"
    assert by_name["state"]["data"]["mode"] == "remote_off"
    assert by_name["discovery"]["data"]["files"] == ["pipeline.py"]
    assert not (tmp_path / "IMPORTED").exists()
    assert not (tmp_path / "RAN").exists()
    assert not list((tmp_path / ".barca").rglob("metadata.db"))


def test_bad_config_and_source_are_reported_as_failed_checks(tmp_path):
    (tmp_path / "barca.toml").write_text("[remote\n")
    report, by_name = checks(cli(tmp_path, "--json", code=3))
    assert not report["ok"] and not by_name["configuration"]["ok"]
    (tmp_path / "barca.toml").write_text("")
    (tmp_path / "bad.py").write_text("from barca import asset\n@asset()\ndef bad(:\n")
    report, by_name = checks(cli(tmp_path, "--json", code=3))
    assert not by_name["source:bad.py"]["ok"]
    assert "bad.py" in by_name["source:bad.py"]["detail"]


def test_remote_off_ignores_remote_profile_and_finds_parent_root(tmp_path):
    (tmp_path / "barca.toml").write_text('[remote]\nuri = "s3://unavailable-bucket"\n')
    (tmp_path / "pipeline.py").write_text(
        "from barca import asset\n@asset()\ndef value(): return 42\n"
    )
    child = tmp_path / "sub"
    child.mkdir()
    report, by_name = checks(cli(child, "../pipeline.py", "--json", BARCA_REMOTE="off"))
    assert report["ok"]
    assert by_name["project_root"]["data"]["path"] == str(tmp_path)
    assert by_name["project_root"]["data"]["source"] == "barca.toml"
    assert by_name["store:artifacts"]["data"]["enabled"] is False
    assert by_name["discovery"]["data"]["files"] == ["pipeline.py"]


def test_unwritable_cache_and_corrupt_metadata_are_preserved(tmp_path):
    (tmp_path / ".barca").write_text("obstruction")
    _, by_name = checks(cli(tmp_path, "--json", code=3))
    assert not by_name["local_cache"]["ok"]
    (tmp_path / ".barca").unlink()
    (tmp_path / ".barca").mkdir()
    metadata = tmp_path / ".barca" / "metadata.db"
    metadata.write_bytes(b"not a sqlite database")
    _, by_name = checks(cli(tmp_path, "--json", code=3))
    assert not by_name["metadata"]["ok"]
    assert metadata.read_bytes() == b"not a sqlite database"


def test_shared_directory_store_is_read_only_and_identity_is_visible(tmp_path):
    (tmp_path / "pipeline.py").write_text(
        "from barca import asset\n@asset()\ndef value(): return 42\n"
    )
    store = tmp_path / "store"
    store.mkdir()
    state = store / "state.db"
    state.write_bytes(b"remote content is not downloaded")
    report, by_name = checks(
        cli(tmp_path, "--json", BARCA_REMOTE_URI=str(store), BARCA_STATE_URI=str(state))
    )
    assert report["ok"]
    assert by_name["state"]["data"]["mode"] == "shared"
    assert by_name["store:artifacts"]["data"]["identity"]["provider"] == "local"
    assert by_name["store:state"]["ok"]
    assert state.read_bytes() == b"remote content is not downloaded"
    assert list(store.iterdir()) == [state]
    pretty = cli(tmp_path, "--pretty")
    assert all(line.startswith("[ok] ") for line in pretty.stdout.splitlines())
    assert len(pretty.stdout.splitlines()) == len(checks(cli(tmp_path, "--json"))[0]["checks"])


def test_healthy_metadata_is_checked_without_mutating_it(tmp_path):
    import hashlib

    (tmp_path / "pipeline.py").write_text(
        "from barca import asset\n@asset()\ndef value(): return 42\n"
    )
    env = {k: v for k, v in os.environ.items() if not k.startswith("BARCA_")}
    subprocess.run(
        [_find_binary(), "get", "value", "--json"],
        cwd=tmp_path,
        env=env,
        check=True,
        capture_output=True,
        timeout=30,
    )
    metadata = tmp_path / ".barca" / "metadata.db"
    before = hashlib.sha256(metadata.read_bytes()).hexdigest()
    _, by_name = checks(cli(tmp_path, "--json"))
    assert by_name["metadata"]["ok"]
    assert "valid" in by_name["metadata"]["detail"]
    assert hashlib.sha256(metadata.read_bytes()).hexdigest() == before


def test_malformed_or_incomplete_probe_cannot_report_success(tmp_path):
    import stat

    fake_bin = tmp_path / "bin"
    fake_bin.mkdir()
    # An installed wheel selects Python beside the executable before consulting PATH.
    # Put the binary beside our fake interpreter so this control works for both installs.
    fake_binary = fake_bin / "barca"
    try:
        os.link(Path(_find_binary()).resolve(), fake_binary)
    except OSError:
        shutil.copy2(_find_binary(), fake_binary)
    fake_python = fake_bin / "python3"
    store = tmp_path / "store"
    store.mkdir()
    for payload in [
        "not json",
        json.dumps(
            {
                "python": {"executable": "fake", "version": "3.13", "barca_version": __version__},
                "stores": [],
            }
        ),
    ]:
        fake_python.write_text("#!/bin/sh\nprintf '%s\\n' '" + payload + "'\n")
        fake_python.chmod(fake_python.stat().st_mode | stat.S_IXUSR)
        report, by_name = checks(
            cli(
                tmp_path,
                "--json",
                code=3,
                binary=str(fake_binary),
                PATH=str(fake_bin) + os.pathsep + os.environ["PATH"],
                BARCA_REMOTE_URI=str(store),
            )
        )
        assert not report["ok"]
        assert not by_name["python"]["ok"]
        assert not by_name["store:artifacts"]["ok"]
        assert "Python helper failed" in by_name["store:artifacts"]["detail"]


def test_config_diagnostics_do_not_echo_secret_source_lines(tmp_path):
    (tmp_path / "barca.toml").write_text('[remote]\nuri = "private-source-secret" trailing\n')
    proc = cli(tmp_path, "--json", code=3)
    assert "private-source-secret" not in proc.stdout + proc.stderr
    assert "configuration" in proc.stdout
