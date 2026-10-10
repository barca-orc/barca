---
title: Organizational groups
description: Organize pipeline nodes without changing execution or caching.
---

# Organizational groups

`group()` organizes assets, tasks and sensors in the UI and CLI. It never adds a
step, dependency, execution boundary or cache key. A group's output identifies
an existing node; it does not hide other dependencies crossing the group.

```python
from barca import asset, task, group

@asset
def rows():
    return [1, 2, 3]

@asset(inputs={"data": rows})
def model(data):
    return {"count": len(data)}

@task(inputs={"result": model})
def validate_model(result):
    assert result["count"] == 3

training_data = group("Training data", members=[rows], output=rows)
training = group(
    "Training",
    members=[training_data, model, validate_model],
    output=model,
    description="Preparation, fitting and checks",
)
experiment = group("Experiment", members=[training], output=training)
```

Declare groups as top-level assignments after their referenced functions/groups,
using a literal name, list or tuple of members, output reference, and optional
literal description. Groups can nest to any depth. Each node or group has at
most one parent. Duplicate membership, unknown references, cycles and outputs
outside the group's descendants are errors. Nodes may remain ungrouped.
Members may reference imported nodes from discovered pipeline files; nested groups
are currently referenced by their variable in the same file. Computed membership,
loops, conditional declarations, strings as references and starred arguments are
not supported. Groups cannot be used as `inputs=` or execution targets.

```bash
barca list pipeline.py --groups          # hierarchy, with members and outputs
barca list pipeline.py --groups --json   # {groups: [{id, name, description, members, output}]}
barca list pipeline.py                   # unchanged flat executable-node listing
```

Group IDs are `group:<file>:<variable>`, separate from `<file>:<function>` node IDs.
The server exposes the same metadata at `GET /groups`. The UI collapses roots and
opens their contents on double click or Enter, retaining nesting across List/Graph.
Health aggregates all descendant nodes: any failed member makes its ancestors red,
even when the output is cached successfully. CLI group listings show organization;
use `barca status` to inspect execution health.

In serve mode, group metadata uses the same retained graph as node inspection.
Groups that reference excluded definitions are omitted and reported through
`/health.load_errors`; healthy executable nodes and other files remain available.
One-shot CLI commands continue to reject invalid group declarations.
