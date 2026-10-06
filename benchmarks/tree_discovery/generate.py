"""Generate a project tree for the discovery benchmark.

`python generate.py <dir> [files] [pipelines]` writes `files` .py files spread over nested
directories, of which `pipelines` import barca and define two assets each (the second reads the
first); the rest are plain helper modules. Defaults: 1000 files, 100 pipelines.
"""

import sys
from pathlib import Path

PIPELINE = """from barca import asset
from helpers_{i} import scale


@asset()
def src_{i}() -> int:
    return scale({i})


@asset(inputs={{"s": src_{i}}})
def out_{i}(s: int) -> int:
    return s + 1
"""

HELPER = """def scale(x):
    return 2 * x


def unused_{i}(y):
    return [y] * 10
"""


def main() -> None:
    root = Path(sys.argv[1])
    files = int(sys.argv[2]) if len(sys.argv) > 2 else 1000
    pipelines = int(sys.argv[3]) if len(sys.argv) > 3 else 100
    root.mkdir(parents=True, exist_ok=True)
    (root / "barca.toml").write_text("")
    for i in range(files):
        d = root / f"area_{i % 10}" / f"team_{i % 7}"
        d.mkdir(parents=True, exist_ok=True)
        if i < pipelines:
            (d / f"pipeline_{i}.py").write_text(PIPELINE.format(i=i))
            (d / f"helpers_{i}.py").write_text(HELPER.format(i=i))
        elif i < files - pipelines:
            (d / f"module_{i}.py").write_text(HELPER.format(i=i))


if __name__ == "__main__":
    main()
