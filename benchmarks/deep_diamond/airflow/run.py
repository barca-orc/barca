"""Run Airflow DAG with LocalExecutor: deep_diamond."""

import json
import os
import shutil
import subprocess
import tempfile
import time

SCRIPT_DIR = os.path.dirname(os.path.abspath(__file__))
# BARCA_BENCH_AIRFLOW_VENV points every Airflow harness at one shared venv.
AIRFLOW_VENV = os.environ.get("BARCA_BENCH_AIRFLOW_VENV", os.path.join(SCRIPT_DIR, ".venv"))
AIRFLOW_BIN = os.path.join(AIRFLOW_VENV, "bin", "airflow")


def run():
    ah = tempfile.mkdtemp(prefix="airflow_bench_")
    dd = os.path.join(ah, "dags")
    os.makedirs(dd)
    shutil.copy(os.path.join(SCRIPT_DIR, "dag.py"), dd)
    env = {
        **os.environ,
        "AIRFLOW_HOME": ah,
        "AIRFLOW__CORE__DAGS_FOLDER": dd,
        "AIRFLOW__CORE__LOAD_EXAMPLES": "False",
        "AIRFLOW__CORE__LOAD_DEFAULT_CONNECTIONS": "False",
        "AIRFLOW__CORE__EXECUTOR": "LocalExecutor",
        "AIRFLOW__CORE__PARALLELISM": "32",
        "AIRFLOW__DATABASE__SQL_ALCHEMY_CONN": f"sqlite:///{ah}/airflow.db",
        "AIRFLOW__LOGGING__LOGGING_LEVEL": "ERROR",
    }
    subprocess.run([AIRFLOW_BIN, "db", "migrate"], env=env, capture_output=True)
    t0 = time.perf_counter()
    result = subprocess.run(
        [
            AIRFLOW_BIN,
            "dags",
            "test",
            "deep_diamond",
            "2024-01-01",
        ],
        env=env,
        capture_output=True,
        text=True,
    )
    elapsed = time.perf_counter() - t0
    shutil.rmtree(ah, ignore_errors=True)
    print(
        json.dumps(
            {
                "elapsed_seconds": round(elapsed, 6),
                "steps_executed": 18,
                "success": result.returncode == 0,
                "executor": "LocalExecutor",
            },
            indent=2,
        )
    )


if __name__ == "__main__":
    run()
