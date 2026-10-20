"""A minimal dotenv loader: KEY=VALUE lines, `#` comments, existing variables win.

`assets.py` calls it at import time, before `settings` is imported, the way the original
pipeline loads its dotenv file before its settings class reads the environment.
"""

import os
from pathlib import Path


def load(path: Path) -> None:
    if not path.is_file():
        return
    for line in path.read_text().splitlines():
        line = line.strip()
        if not line or line.startswith("#") or "=" not in line:
            continue
        key, _, value = line.partition("=")
        os.environ.setdefault(key.strip(), value.strip().strip('"'))
