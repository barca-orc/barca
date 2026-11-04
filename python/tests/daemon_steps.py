"""Run steps in-process through the daemon worker's step runner (`_worker._run_daemon_step`).

The coordinator starts every worker with `--daemon` and sends one step at a time over its
socket; this drives the same function without the socket, recording what the worker would
have reported. Inputs name either an artifact in `provided` or an earlier step's node id.
"""

from contextlib import ExitStack
from unittest import mock

from barca import _runtime, _worker


def run_steps(steps, art_dir=None, provided=None):
    """Run `steps` in order and return the reports: `{"type": "result", "node_id",
    "artifact"}` or `{"type": "error", **emit_step_error kwargs}`.

    `provided` maps a node id to `{"path": ...}`; a step's `inputs` map parameters to node
    ids, resolved to that path or to the artifact an earlier step reported. `partition_keys`
    expands into one step per key, as the coordinator does.
    """
    reports = []
    produced = {key: ref["path"] for key, ref in (provided or {}).items()}
    art_dir = str(art_dir) if art_dir else _worker._default_artifact_dir()

    def completed(node_id, artifact):
        produced[node_id] = artifact["path"]
        reports.append({"type": "result", "node_id": node_id, "artifact": artifact})

    with ExitStack() as patches:
        patches.enter_context(mock.patch.object(_runtime, "emit_step_completed", completed))
        patches.enter_context(
            mock.patch.object(
                _runtime, "emit_step_error", lambda **kw: reports.append({"type": "error", **kw})
            )
        )
        patches.enter_context(mock.patch.object(_runtime, "emit_log", lambda *a, **kw: None))
        for name in ("_ctrl_c_does_nothing", "_ctrl_c_interrupts_the_step"):
            patches.enter_context(mock.patch.object(_worker, name, lambda: None))
        modules = {}
        lru = _worker._ArtifactLRU()
        for step in steps:
            for one in _expand(step):
                one["inputs"] = {
                    param: produced.get(upstream, upstream)
                    for param, upstream in one.get("inputs", {}).items()
                }
                _worker._run_daemon_step(one, modules, art_dir, lru)
    return reports


def _expand(step):
    keys = step.get("partition_keys")
    if not keys:
        return [dict(step)]
    out = []
    for key in keys:
        suffix = ",".join(f"{k}={v}" for k, v in sorted(key.items()))
        one = {k: v for k, v in step.items() if k != "partition_keys"}
        one["node_id"] = f"{step['node_id']}[{suffix}]"
        one["direct_kwargs"] = {**step.get("direct_kwargs", {}), **key}
        out.append(one)
    return out
