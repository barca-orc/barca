"""Check finite design models and prove the checks detect four broken transitions.

python specs/models/check.py --jar /path/to/tla2tools.jar [--json results.json]
The pinned tool digest and model scope are documented in README.md.
"""

import argparse
import hashlib
import json
import re
import subprocess
import tempfile
from pathlib import Path

HERE = Path(__file__).resolve().parent
TOOL_SHA256 = "936a262061c914694dfd669a543be24573c45d5aa0ff20a8b96b23d01e050e88"


def check(jar, root, name, source, configuration, expected=None):
    directory = root / name
    directory.mkdir()
    model = directory / "AssetOwnership.tla"
    model.write_text(source)
    config = directory / "model.cfg"
    config.write_text(configuration)
    result = subprocess.run(
        [
            "java",
            "-XX:+UseParallelGC",
            "-Xmx512m",
            "-cp",
            str(jar),
            "tlc2.TLC",
            "-workers",
            "1",
            "-metadir",
            str(directory / "states"),
            "-config",
            str(config),
            str(model),
        ],
        cwd=directory,
        capture_output=True,
        text=True,
        check=False,
        timeout=120,
    )
    output = result.stdout + result.stderr
    (directory / "output.log").write_text(output)
    if expected:
        assert result.returncode != 0 and f"Invariant {expected} is violated" in output, output
    else:
        assert result.returncode == 0 and "No error has been found" in output, output
    states = re.search(r"(\d+) states generated, (\d+) distinct states found", output)
    print(f"{name}: {'caught ' + expected if expected else 'passed'}", flush=True)
    return {
        "check": name,
        "expected_violation": expected,
        "generated_states": int(states[1]) if states else None,
        "distinct_states": int(states[2]) if states else None,
    }


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--jar", required=True, type=Path)
    parser.add_argument("--json", type=Path)
    args = parser.parse_args()
    jar = args.jar.resolve()
    assert hashlib.sha256(jar.read_bytes()).hexdigest() == TOOL_SHA256, "Use the pinned TLC jar"
    source = (HERE / "AssetOwnership.tla").read_text()
    concurrent = (HERE / "concurrent.cfg").read_text()
    conservative = (HERE / "conservative.cfg").read_text()
    partitioned = (HERE / "partitioned.cfg").read_text()
    mutants = [
        (
            "late_completion",
            "IF w.job[2] > selected[w.job[1]]",
            "IF TRUE",
            concurrent,
            "NoRevisionRegression",
        ),
        (
            "duplicate_owner",
            r"/\ job \notin RunningJobs",
            r"/\ TRUE",
            concurrent,
            "NoDuplicateWork",
        ),
        (
            "cancel_other_subscriber",
            r"w.job # requests[r] \/ OtherNeeds(w.job, r)",
            r"w.job # requests[r]",
            concurrent,
            "CancelPreservesOthers",
        ),
        (
            "unsafe_concurrent_versions",
            r"/\ ConcurrentVersions \/ ~\E w \in running : w.job[1] = job[1]",
            r"/\ TRUE",
            conservative,
            "ConservativeVersions",
        ),
    ]
    with tempfile.TemporaryDirectory(prefix=".model-check-", dir=HERE) as temporary:
        root = Path(temporary)
        rows = [
            check(jar, root, "concurrent", source, concurrent),
            check(jar, root, "conservative", source, conservative),
            check(jar, root, "partitioned", source, partitioned),
        ]
        for name, before, after, config, expected in mutants:
            assert source.count(before) == 1, f"Mutation did not match exactly once: {name}"
            rows.append(check(jar, root, name, source.replace(before, after), config, expected))
    if args.json:
        args.json.write_text(
            json.dumps(
                {
                    "tool": "TLC 1.7.4",
                    "tool_sha256": TOOL_SHA256,
                    "model_sha256": hashlib.sha256(source.encode()).hexdigest(),
                    "configs_sha256": {
                        name: hashlib.sha256((HERE / name).read_bytes()).hexdigest()
                        for name in ("concurrent.cfg", "conservative.cfg", "partitioned.cfg")
                    },
                    "checks": rows,
                },
                indent=2,
            )
            + "\n"
        )


if __name__ == "__main__":
    main()
