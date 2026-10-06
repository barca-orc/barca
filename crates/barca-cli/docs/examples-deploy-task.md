# Example: an asset feeding a task

A cached asset produces a model; a task deploys it. The task always runs, the asset only when
stale.

```python
from barca import asset, task


@asset()
def model() -> dict:
    return {"version": 3, "metrics": {"auc": 0.91}}


@task(inputs={"m": model})
def deploy(m: dict) -> None:
    print(f"deploying model v{m['version']} (auc {m['metrics']['auc']})")
```

```bash
barca run deploy pipeline.py
barca run deploy pipeline.py
barca run deploy pipeline.py --refresh model
barca run deploy pipeline.py --refresh model --no-cascade
```

What to notice:

- First run: 2 steps (`model`, then `deploy`). Second run: 1 step, because `model` is served
  from cache and only the task re-runs.
- `--refresh model` forces `model` to re-materialize, so that run is 2 steps again. It also
  re-materializes every asset downstream of `model` (here there is none besides the task);
  `--no-cascade` refreshes only `model`. `--refresh-all` refreshes every
  upstream asset.
- The task's `print` goes to stderr; stdout stays a single JSON line (when piped, or with `--json`).
- `barca get deploy pipeline.py` exits 2 (a usage error) and tells you to use `barca run`.

See also: `barca docs tasks`, `barca docs cache`.
