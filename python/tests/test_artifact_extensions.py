"""The artifact extension table of specs/interfaces/artifact-formats.md is the worker's map.

Rust's `sql::format_of` is checked against the same table (`format_of_agrees_with_the_spec_table`).
"""

from pathlib import Path

from barca._artifacts import _FORMAT_EXTENSIONS, FORMAT_OF_EXTENSION

SPEC = Path(__file__).resolve().parents[2] / "specs" / "interfaces" / "artifact-formats.md"


def spec_rows() -> list[tuple[str, str, bool]]:
    section = SPEC.read_text().split("## Artifact file extensions", 1)[1].split("\n## ", 1)[0]
    rows = []
    for line in section.splitlines():
        if line.startswith("| `."):
            ext, fmt, written = (c.strip() for c in line.strip("|").split("|"))
            rows.append((ext.strip("`"), fmt, written == "yes"))
    assert rows, "the spec's extension table is missing"
    return rows


def test_extension_map_is_the_spec_table():
    assert FORMAT_OF_EXTENSION == {ext: fmt for ext, fmt, _ in spec_rows()}


def test_written_extensions_are_the_spec_table():
    assert _FORMAT_EXTENSIONS == {fmt: ext for ext, fmt, written in spec_rows() if written}
