"""The private modules of the `barca` package import each other without cycles.

Every `import` statement counts, including ones inside functions: a lazy import breaks a cycle
at run time but not the coupling, and the next refactor turns it back into an import error.
The public modules (`barca`, `barca.api`, `barca.client`) are left out: they re-export the
private ones, and the private ones may reach the public names lazily.
"""

import ast
from pathlib import Path

PACKAGE = Path(__file__).resolve().parents[1] / "barca"


def _imports(path: Path) -> set[str]:
    """The `barca._*` modules `path` imports, anywhere in the file."""
    found = set()
    for node in ast.walk(ast.parse(path.read_text(), str(path))):
        if isinstance(node, ast.Import):
            for alias in node.names:
                if alias.name.startswith("barca._"):
                    found.add(alias.name.split(".")[1])
        elif isinstance(node, ast.ImportFrom) and node.module:
            if node.module == "barca":
                found.update(a.name for a in node.names if a.name.startswith("_"))
            elif node.module.startswith("barca._"):
                found.add(node.module.split(".")[1])
    return {m for m in found if m != path.stem}


def _graph() -> dict[str, set[str]]:
    return {p.stem: _imports(p) for p in PACKAGE.glob("_*.py") if p.stem != "__main__"}


def _cycles(graph: dict[str, set[str]]) -> list[list[str]]:
    """Every elementary cycle of `graph`, each as the modules on it in order."""
    cycles = []
    for start in sorted(graph):
        stack = [(start, [start])]
        while stack:
            node, path = stack.pop()
            for nxt in sorted(graph.get(node, ())):
                if nxt == start and len(path) > 0:
                    cycles.append(path)
                elif nxt not in path and nxt in graph and nxt > start:
                    stack.append((nxt, path + [nxt]))
    return cycles


def test_private_modules_import_without_cycles():
    graph = _graph()
    assert graph, PACKAGE
    cycles = _cycles(graph)
    assert not cycles, "\n".join(" -> ".join(c + [c[0]]) for c in cycles)
