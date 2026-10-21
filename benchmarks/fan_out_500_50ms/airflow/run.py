"""Run Airflow DAG with LocalExecutor for genuine parallel execution."""

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
    airflow_home = tempfile.mkdtemp(prefix="airflow_bench_")
    dags_dir = os.path.join(airflow_home, "dags")
    os.makedirs(dags_dir)
    shutil.copy(os.path.join(SCRIPT_DIR, "dag.py"), dags_dir)

    # Use LocalExecutor with PostgreSQL-compatible SQLite for parallelism.
    # LocalExecutor forks a process per task, giving real parallelism.
    env = {
        **os.environ,
        "AIRFLOW_HOME": airflow_home,
        "AIRFLOW__CORE__DAGS_FOLDER": dags_dir,
        "AIRFLOW__CORE__LOAD_EXAMPLES": "False",
        "AIRFLOW__CORE__LOAD_DEFAULT_CONNECTIONS": "False",
        "AIRFLOW__CORE__EXECUTOR": "LocalExecutor",
        "AIRFLOW__CORE__PARALLELISM": "32",
        "AIRFLOW__CORE__MAX_ACTIVE_TASKS_PER_DAG": "500",
        "AIRFLOW__DATABASE__SQL_ALCHEMY_CONN": f"sqlite:///{airflow_home}/airflow.db",
        "AIRFLOW__LOGGING__LOGGING_LEVEL": "ERROR",
    }

    # Initialize DB
    subprocess.run([AIRFLOW_BIN, "db", "migrate"], env=env, capture_output=True)

    # Airflow 3: `dags test` respects the configured executor (LocalExecutor).
    t0 = time.perf_counter()
    result = subprocess.run(
        [
            AIRFLOW_BIN,
            "dags",
            "test",
            "fan_out_500_50ms",
            "2024-01-01",
        ],
        env=env,
        capture_output=True,
        text=True,
    )
    elapsed = time.perf_counter() - t0

    shutil.rmtree(airflow_home, ignore_errors=True)

    print(
        json.dumps(
            {
                "elapsed_seconds": round(elapsed, 6),
                "steps_executed": 500,
                "success": result.returncode == 0,
                "executor": "LocalExecutor",
            },
            indent=2,
        )
    )


if __name__ == "__main__":
    run()
