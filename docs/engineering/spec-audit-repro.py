"""Read-only audit probes; all pipeline edits and state live in temporary directories.

Run from the repository root with its Python helpers importable and a matching 0.33.0
binary: python docs/engineering/spec-audit-repro.py --binary /absolute/path/to/barca
Outputs observations, not a passing conformance claim. Does not start a server.
"""

import argparse
import json
import os
import signal
import sqlite3
import subprocess
import tempfile
import time
from pathlib import Path

ROOT = Path(__file__).resolve().parents[2]


def invoke(binary, project, env, *args):
    result = subprocess.run(
        [binary, *args],
        check=False,
        cwd=project,
        env=env,
        capture_output=True,
        text=True,
        timeout=30,
    )
    if result.returncode:
        raise RuntimeError(result.stderr[-3000:])
    return json.loads(result.stdout)


def historical(binary, env):
    with tempfile.TemporaryDirectory(prefix="barca-audit-history-") as directory:
        project = Path(directory)
        (project / "pipeline.py").write_text("""from pathlib import Path
from barca import asset, sensor, partitions_from

@sensor()
def source():
    return {"a": int(Path("value").read_text())}

@asset(partitions={"key": partitions_from(source)})
def doubled(key, source):
    return source * 2
""")
        values = []
        for value in (1, 2, 1, 1):
            (project / "value").write_text(str(value))
            result = invoke(
                binary, project, env, "get", "doubled", "pipeline.py", "-o", "value", "--json"
            )
            values.append(result["final_output"])
        return {
            "inputs": [1, 2, 1, 1],
            "observed": values,
            "expected": [{"key=a": 2}, {"key=a": 4}, {"key=a": 2}, {"key=a": 2}],
        }


def log_cap(binary, env):
    with tempfile.TemporaryDirectory(prefix="barca-audit-logs-") as directory:
        project = Path(directory)
        (project / "pipeline.py").write_text("""from barca import task

@task()
def noisy():
    print("x" * (17 * 1024 * 1024))
    return 1
""")
        invoke(binary, project, env, "run", "noisy", "pipeline.py", "--json")
        databases = list((project / ".barca").rglob("metadata.db"))
        if len(databases) != 1:
            raise RuntimeError(f"expected one metadata DB, found {databases}")
        with sqlite3.connect(databases[0]) as conn:
            longest = conn.execute("SELECT MAX(length(line)) FROM logs").fetchone()[0]
        return {"declared_cap_bytes": 16 * 1024 * 1024, "longest_recorded_line_bytes": longest}


def preview(binary, env):
    with tempfile.TemporaryDirectory(prefix="barca-audit-preview-") as directory:
        project = Path(directory)
        (project / "pipeline.py").write_text("""from pathlib import Path
from barca import asset, partitions

Path("import-side-effect").write_text("executed at import")
KEYS = ["a"]

@asset(partitions={"key": partitions(KEYS)})
def result(key):
    Path("node-body").write_text("executed node")
    return key
""")
        invoke(binary, project, env, "get", "result", "pipeline.py", "--dry-run", "--json")
        return {
            "import_side_effect": (project / "import-side-effect").exists(),
            "node_body_executed": (project / "node-body").exists(),
        }


def source_snapshot(binary, env):
    with tempfile.TemporaryDirectory(prefix="barca-audit-snapshot-") as directory:
        project = Path(directory)
        (project / "helper.py").write_text("def value():\n    return 1\n")
        (project / "pipeline.py").write_text("""from pathlib import Path
import time
from barca import asset, sensor

@sensor()
def gate():
    Path("started").touch()
    for _ in range(1000):
        if Path("go").exists():
            return True
        time.sleep(0.01)
    raise TimeoutError("audit probe was not released")

@asset(inputs={"_gate": gate})
def result(_gate):
    from helper import value
    return value()
""")
        child = subprocess.Popen(
            [binary, "get", "result", "pipeline.py", "-o", "value", "--json"],
            cwd=project,
            env=env,
            stdout=subprocess.PIPE,
            stderr=subprocess.PIPE,
            text=True,
        )
        try:
            deadline = time.monotonic() + 15
            while not (project / "started").exists():
                if child.poll() is not None or time.monotonic() > deadline:
                    raise RuntimeError("sensor did not reach the audit barrier")
                time.sleep(0.01)
            (project / "helper.py").write_text("def value():\n    return 2\n")
            (project / "go").touch()
            stdout, stderr = child.communicate(timeout=30)
            if child.returncode:
                raise RuntimeError(stderr[-3000:])
            result = json.loads(stdout)
            return {"definition_at_run_start": 1, "observed_result": result["final_output"]}
        finally:
            (project / "go").touch()
            if child.poll() is None:
                child.terminate()
                try:
                    child.communicate(timeout=5)
                except subprocess.TimeoutExpired:
                    child.kill()
                    child.communicate(timeout=5)


def process_cleanup(binary, env):
    with tempfile.TemporaryDirectory(prefix="barca-audit-process-") as directory:
        project = Path(directory)
        (project / "pipeline.py").write_text("""from pathlib import Path
import subprocess
import sys
from barca import task

@task()
def launch():
    child = subprocess.Popen(
        [sys.executable, "-c", "import time; tag='barca-audit-descendant'; time.sleep(60)"],
        stdin=subprocess.DEVNULL, stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL,
    )
    Path("child.pid").write_text(str(child.pid))
    return child.pid
""")
        pid = None

        def owned_process():
            if pid is None:
                return False
            state = subprocess.run(
                ["ps", "-o", "stat=,args=", "-p", str(pid)],
                capture_output=True,
                text=True,
                check=False,
            ).stdout.strip()
            return "barca-audit-descendant" in state and not state.startswith("Z")

        try:
            invoke(binary, project, env, "run", "launch", "pipeline.py", "--json")
            pid = int((project / "child.pid").read_text())
            survived = owned_process()
        finally:
            if pid is None and (project / "child.pid").exists():
                pid = int((project / "child.pid").read_text())
            if owned_process():
                os.kill(pid, signal.SIGTERM)
                deadline = time.monotonic() + 2
                while owned_process() and time.monotonic() < deadline:
                    time.sleep(0.01)
                if owned_process():
                    os.kill(pid, signal.SIGKILL)
                    time.sleep(0.05)
            if owned_process():
                raise RuntimeError("audit descendant cleanup failed")
        return {"ordinary_child_survived_successful_run": survived, "audit_cleaned_child": True}


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--binary", type=Path, required=True)
    args = parser.parse_args()
    binary = str(args.binary.resolve())
    env = {**os.environ, "PYTHONPATH": str(ROOT / "python"), "BARCA_POOL_SIZE": "1"}
    env["PATH"] = str(Path(binary).parent) + os.pathsep + env.get("PATH", "")
    for name in (
        "BARCA_SERVER",
        "BARCA_REMOTE_URI",
        "BARCA_ENV",
        "BARCA_OUTPUT",
        "BARCA_REQUIRE_ENV",
    ):
        env.pop(name, None)
    version = subprocess.check_output([binary, "--version"], text=True).strip()
    print(
        json.dumps(
            {
                "binary": version,
                "historical_selection": historical(binary, env),
                "log_cap": log_cap(binary, env),
                "preview": preview(binary, env),
                "source_snapshot": source_snapshot(binary, env),
                "process_cleanup": process_cleanup(binary, env),
            },
            indent=2,
        )
    )


if __name__ == "__main__":
    main()
