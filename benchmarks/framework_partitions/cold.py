"""Cold orchestration history: a selected chain or all per-key work.

Each sample uses a fresh directory and empty framework history. Version snapshots
are fixture inputs, not cached asset results. No framework receives a warm-up.
Uses the same environment variables and checks as bench.py.
"""

import json
import os

from bench import ROOT, fixture, measure


full = os.environ.get("COLD_MODE") == "full"
default_keys = "1,100,1000" if full else "1,1000,50000,200000"
for count in map(int, os.environ.get("KEYS", default_keys).split(",")):
    for framework in os.environ.get("FRAMEWORKS", "barca,dagster,prefect").split(","):
        for sample in range(2, int(os.environ.get("REPEATS", "1" if full else "3")) + 2):
            project = ROOT / f"{framework}-{count}-{sample}"
            project.mkdir(parents=True, exist_ok=False)
            (project / "dagster-home").mkdir()
            (project / "dagster-home/dagster.yaml").write_text("telemetry:\n  enabled: false\n")
            (project / "pipeline.py").write_text(fixture.pipeline("none" if full else "1000"))
            (project / "barca.toml").write_text("")
            versions = {f"r{i}": 1 for i in range(count)}
            (project / "previous.json").write_text(json.dumps(versions))
            if full:
                versions = {key: sample for key in versions}
            else:
                versions["r0"] = sample
            (project / "versions.json").write_text(json.dumps(versions))
            measure(framework, project, count, "cold_full" if full else "cold", sample)
