---
title: "Barca vs Dagster"
description: Install size, steps to a first result, run time, run output and error output for the same two-asset pipeline, measured on 2026-10-21 with barca 0.28.0 and Dagster 1.13.26.
---

Last measured: 2026-10-21, with barca 0.28.0 and Dagster 1.13.26, both from PyPI with `uv`, on
Linux (AMD Ryzen AI MAX+ 395). barca ran under Python 3.13; the Dagster project scaffold chose
Python 3.14. Setup and commands are in
[`benchmarks/RESULTS.md`](https://github.com/barca-orc/barca/blob/main/benchmarks/RESULTS.md).
It replaces the first comparison, made on 2026-06-10 with barca 0.2.0 and Dagster 1.13.8 on
macOS.

## Install footprint

| Metric | barca 0.28.0 | Dagster 1.13.26 | Dagster scaffold |
|--------|-------|---------|---------|
| Packages installed | 1 | 48 | 98 |
| Venv size (`du -sm`) | 31 MB | 81 MB | 184 MB |
| Lockfile entries | 2 | 53 | 112 |
| Wheel download | 13.9 MB | 2.0 MB, plus its dependencies | |

"Dagster 1.13.26" is `uv pip install dagster==1.13.26` into an empty venv. "Dagster scaffold" is
the project that `uvx create-dagster@1.13.26 project myproj --uv-sync` creates, which also
installs `dagster-dg-cli` and `dagster-webserver`. The barca wheel (manylinux x86_64) contains
the Rust binary, the web UI that `barca serve` shows and the Python decorator stubs, and
declares no dependencies.

`python -c "import dagster"` took 365 ms (median of 20), `python -c "import barca"` 42 ms, and
`python -c pass` 13 ms.

## Steps to first output

### Barca

```bash
uv add barca                    # install
cat > pipeline.py << 'PY'       # write code
from barca import asset

@asset()
def raw_data() -> list[dict]:
    return [{"x": 1}, {"x": 2}, {"x": 3}]

@asset(inputs={"data": raw_data})
def summary(data: list[dict]) -> dict:
    return {"count": len(data), "total": sum(d["x"] for d in data)}
PY
barca get pipeline.py            # run
```

### Dagster

```bash
uvx create-dagster@latest project myproj --uv-sync   # scaffold and install
cd myproj && source .venv/bin/activate               # enter project
# write src/myproj/defs/assets.py                    # code
dg launch --assets raw_data,summary                  # run
```

```python
# src/myproj/defs/assets.py
import dagster as dg

@dg.asset
def raw_data() -> list[dict]:
    return [{"x": 1}, {"x": 2}, {"x": 3}]

@dg.asset
def summary(raw_data: list[dict]) -> dict:
    return {"count": len(raw_data), "total": sum(d["x"] for d in raw_data)}
```

With `--uv-sync`, `create-dagster` did not prompt. The scaffold's `definitions.py` loads every
module in `defs/`, so the new file needed no registration. On 2026-06-10 the quickstart also had
the reader install pandas and create a data directory and a CSV file; the pipeline above does
not need them.

## Running the same two-asset pipeline

| Metric | barca 0.28.0 | Dagster 1.13.26 |
|--------|-------|---------|
| Command | `barca get pipeline.py` | `dg launch --assets raw_data,summary` |
| Time, nothing cached | 73 ms (67–76) | 3.64 s (3.45–3.94) |
| Time, second run | 13 ms (13–15), 0 steps run | not measured |
| Output lines | 1 JSON line on stdout, 2 progress lines on stderr | 29 DEBUG lines on stderr |
| Result access | artifact path in the JSON; `-o value` prints the value | pickled to the instance's storage directory |
| Error output | 1 JSON line on stdout, 4 lines on stderr | 104 lines on stderr |

Times are medians of 10 runs, the two tools taking turns run by run; the range is the fastest
and slowest run. barca's first time deletes `.barca/` before each run. Dagster wrote its run to a
`DAGSTER_HOME` without a `dagster.yaml`, and the virtualenv was not activated; that adds a
one-line instance notice, a seven-line virtualenv warning and blank lines to the 29 DEBUG
lines, 38 lines in all.

### Barca output

```
[barca] importing pipeline.py
[barca] 2/2 steps | done in 0.0s
{"elapsed_seconds":0.0609,"final_output":{"_barca_artifact":{"format":"json","path":".barca/artifacts/pipeline.py--summary/2d10c4….json","size_bytes":24}},"phases":1,"run_id":"…","status":"success","steps":[…],"steps_executed":2,"warnings":[]}
```

The `steps` array holds each step's id, run hash, cache status and time. The current shape is in
the [CLI contract](/reference/cli-contract/).

### Dagster output (abridged from 29 lines)

```
… - dagster - DEBUG - __ASSET_JOB - … - RUN_START - Started execution of run for "__ASSET_JOB".
… - dagster - DEBUG - __ASSET_JOB - … - ENGINE_EVENT - Executing steps using multiprocess executor
… - dagster - DEBUG - __ASSET_JOB - … - raw_data - STEP_WORKER_STARTING - Launching subprocess
… (12 more lines for raw_data) …
… - dagster - DEBUG - __ASSET_JOB - … - summary - STEP_WORKER_STARTING - Launching subprocess
… (12 more lines for summary) …
… - dagster - DEBUG - __ASSET_JOB - … - ENGINE_EVENT - Multiprocess executor: parent process exiting after 2.81s
… - dagster - DEBUG - __ASSET_JOB - … - RUN_SUCCESS - Finished execution of run for "__ASSET_JOB".
```

## Error output

The same asset raising `ValueError("something went wrong")`.

### Barca 0.28.0

Exit code 1. On stdout, one JSON object with `"status": "failed"`, `"error": "ValueError:
something went wrong"` and `"failed_node": "broken.py:oops"`. On stderr:

```
[barca] importing broken.py
[barca] 0/1 steps | failed in 0.0s
{"artifact_dir":".barca/artifacts/broken.py--oops","code":1,"error":"step 'broken.py:oops' failed: ValueError: something went wrong","kind":"step_failed","node":"broken.py:oops","remediation":"Fix the error in 'broken.py:oops' (see the traceback) and re-run the same command. …","traceback":"  File \"broken.py\", line 6, in oops\n    raise ValueError(\"something went wrong\")"}
[barca] run failed: step 'broken.py:oops' failed: ValueError: something went wrong; 0 downstream steps skipped (exit 1)
```

The traceback holds only the frames from the user's file.

### Dagster 1.13.26

Exit code 1 and 104 lines on stderr, ending in `Error: Materialization failed.` They hold three
`Stack Trace:` blocks. The first:

```
dagster._core.errors.DagsterExecutionStepExecutionError: Error occurred while executing op "oops"::

ValueError: something went wrong

Stack Trace:
  File ".../dagster/_core/execution/plan/utils.py", line 57, in op_execution_error_boundary
    yield
  File ".../dagster/_utils/__init__.py", line 394, in iterate_with_context
    next_output = next(iterator)
  File ".../dagster/_core/execution/plan/compute_generator.py", line 136, in _coerce_op_compute_fn_to_iterator
    ...
  File ".../dagster/_core/execution/plan/compute_generator.py", line 116, in invoke_compute_fn
    ...
  File ".../src/myproj/defs/broken.py", line 6, in oops
    raise ValueError("something went wrong")
```

The frame from the user's file is the last one, below the Dagster frames.

## Other things observed on 2026-06-10

These are notes from using Dagster 1.13.8 on 2026-06-10. They have not been checked against a
later Dagster release, except where a note dated 2026-10-21 says so.

- `dg list defs` printed a table of every asset with its deps, group and kind, without running
  anything. barca's equivalent is `barca list`, added in 0.2.1.
- The Dagster scaffold included a `.gitignore` covering the files Dagster writes. barca 0.2.0
  created `.barca/` without one. Note, 2026-10-07: barca 0.18.0 writes `.barca/.gitignore`
  containing `*`, so the directory ignores itself.
- A `.py` file placed in the scaffold's `defs/` folder was picked up without registration.
  barca 0.2.0 needed the files named on the command line. Note, 2026-10-07: barca 0.18.0 reads
  every `.py` file under the project root that imports barca when no file is named; see
  [Discovery](/reference/discovery/).
- The Dagster project needed a `pyproject.toml`, a `src` layout, `definitions.py` and a `defs/`
  folder before `dg launch` ran. `barca get file.py` runs on a single file.
- Each asset printed 14 DEBUG lines per run. No option to reduce this was found during the test.
- `create-dagster` asked "Run uv sync? (y/n)". No `--yes` flag was found during the test. Note,
  2026-10-21: `create-dagster@1.13.26 project myproj --uv-sync` ran without asking.
- `dg` commands printed a five-line warning when the project's virtualenv was not activated,
  including when run as `.venv/bin/dg`. Note, 2026-10-21: in 1.13.26 the warning is seven lines
  and says that adding `project_and_activated_venv_mismatch` to `cli.suppress_warnings` turns it
  off.
- Both a `dagster` command and a `dg` command were installed.
