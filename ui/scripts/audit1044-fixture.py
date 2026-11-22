"""Generate a reproducible equivalent of #971/#1044, not the lost original audit project.

616 nodes: 610 assets, three scheduled sensors, three tasks; six failing roots;
76 assets without a refresh path; 2,562 partition keys, 51 with zero rows.
Seed with `barca get pipeline.py` (expected failures), then serve with scheduling off.
"""

import sys
from pathlib import Path

root = Path(sys.argv[1])
root.mkdir(parents=True, exist_ok=True)
if "--plain" in sys.argv[2:]:
    (root / "pipeline.py").write_text(
        "from barca import asset\n\n"
        + "\n".join(f"@asset\ndef audit_asset_{i:04d}():\n    return {i}\n" for i in range(616))
    )
    raise SystemExit(0)
source = [
    "from barca import asset, sensor, task, group, partitions, Schedule\nimport pyarrow as pa\n"
]
for i in range(3):
    source.append(
        f'@sensor(freshness=Schedule("*/15 * * * *"))\ndef clock_{i}():\n'
        '    return False, "audit-revision"\n'
    )
for i in range(610):
    name = f"asset_{i:04d}"
    if i < 6:
        source.append(f'@asset\ndef {name}():\n    raise ValueError("audit failing root {i}")\n')
    elif i == 6:
        source.append(
            '@asset(partitions={"key": partitions([str(i) for i in range(2562)])})\n'
            f'def {name}(key):\n    return pa.table({{"value": [] if int(key) < 51 else [int(key)]}})\n'
        )
    elif i < 83:
        source.append(f"@asset\ndef {name}():\n    return {i}\n")
    else:
        upstream = f"asset_{i % 6:04d}" if i % 17 == 0 else f"clock_{i % 3}"
        source.append(
            f'@asset(inputs={{"value": {upstream}}})\ndef {name}(value):\n    return {i}\n'
        )
for i in range(3):
    source.append(
        f'@task(inputs={{"value": asset_{100 + i:04d}}})\ndef check_{i}(value):\n    return None\n'
    )
for group_index, (start, stop) in enumerate(((0, 232), (232, 432), (432, 610))):
    members = [f"asset_{i:04d}" for i in range(start, stop)]
    if group_index == 0:
        members += [f"clock_{i}" for i in range(3)] + [f"check_{i}" for i in range(3)]
    source.append(
        f'audit_{group_index} = group("Audit {group_index}", members=[{", ".join(members)}], output=asset_{stop - 1:04d})\n'
    )
(root / "pipeline.py").write_text("\n".join(source))
