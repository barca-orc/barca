#!/usr/bin/env python3
"""Check specification navigation, invariant ownership and referenced evidence.

This validates structure, not semantic conformance of prose or the implementation.
Model safety and real runtime behavior have separate checks linked from specs/README.md.
"""

import ast
import hashlib
import json
import re
import sys
from collections import defaultdict
from pathlib import Path
from urllib.parse import unquote, urlsplit

ROOT = Path(__file__).resolve().parents[1]
SPECS = ROOT / "specs"
LINK = re.compile(r"\[[^\]\n]*\]\(([^)\n]+)\)")
INVARIANT = re.compile(r"\bINV-[A-Z0-9]+(?:-[A-Z0-9]+)*\b")
DEFINITION = re.compile(r"^\s*- \*\*(INV-[A-Z0-9]+(?:-[A-Z0-9]+)*):\*\*", re.MULTILINE)


def prose(text):
    return re.sub(r"```[^\n]*\n[\s\S]*?```", "", text)


def anchors(path):
    text = prose(path.read_text())
    found = set()
    counts = defaultdict(int)
    for heading in re.findall(r"^#{1,6}\s+(.+)$", text, re.MULTILINE):
        heading = re.sub(r"\[([^]]+)\]\([^)]*\)", r"\1", heading)
        slug = re.sub(r"[^\w\- ]", "", heading.strip().lower()).replace(" ", "-")
        number = counts[slug]
        counts[slug] += 1
        found.add(slug if number == 0 else f"{slug}-{number}")
    return found


def main():
    files = sorted(
        p
        for p in SPECS.rglob("*")
        if p.is_file()
        and not any(
            part.startswith(".") or part == "__pycache__" for part in p.relative_to(SPECS).parts
        )
    )
    markdown = [p for p in files if p.suffix == ".md"] + [ROOT / "GLOSSARY.md"]
    errors = []
    edges = defaultdict(set)
    definitions = defaultdict(list)
    references = set()
    test_names = set()
    for path in (ROOT / "python/tests").rglob("test_*.py"):
        tree = ast.parse(path.read_text(), filename=str(path))
        doc = ast.get_docstring(tree) or ""
        references.update(INVARIANT.findall(doc))
        for owner in re.findall(r"\bspecs/[A-Za-z0-9_./-]+\.(?:md|allium)\b", doc):
            if not (ROOT / owner).is_file():
                errors.append(f"{path.relative_to(ROOT)}: missing contract owner {owner}")
        for node in ast.walk(tree):
            if isinstance(node, (ast.FunctionDef, ast.AsyncFunctionDef)) and node.name.startswith(
                "test_"
            ):
                test_names.add(node.name)
    link_count = 0
    for path in markdown:
        text = prose(path.read_text())
        for invariant in DEFINITION.findall(text):
            definitions[invariant].append(path.relative_to(ROOT).as_posix())
        references.update(INVARIANT.findall(text))
        for name in re.findall(r"`(test_[A-Za-z0-9_]+)`", text):
            if name not in test_names:
                errors.append(f"{path.relative_to(ROOT)}: unknown Python test {name}")
        for raw in LINK.findall(text):
            target = raw.strip().removeprefix("<").removesuffix(">")
            parsed = urlsplit(target)
            if parsed.scheme or parsed.netloc:
                continue
            destination = (path.parent / unquote(parsed.path)).resolve() if parsed.path else path
            link_count += 1
            if not destination.is_relative_to(ROOT) or not destination.exists():
                errors.append(f"{path.relative_to(ROOT)}: missing local link {target}")
                continue
            edges[path].add(destination)
            if (
                parsed.fragment
                and destination.suffix == ".md"
                and unquote(parsed.fragment) not in anchors(destination)
            ):
                errors.append(f"{path.relative_to(ROOT)}: missing anchor {target}")
    for invariant in sorted(references):
        owners = definitions[invariant]
        if len(owners) != 1:
            errors.append(f"{invariant}: expected one definition, found {owners}")
    pending = [SPECS / "README.md"]
    reached = set()
    while pending:
        path = pending.pop()
        if path in reached:
            continue
        reached.add(path)
        pending.extend(edges[path] - reached)
    for path in files:
        if (
            path.suffix in {".md", ".allium", ".yaml", ".py", ".tla", ".cfg", ".json"}
            and path not in reached
        ):
            errors.append(f"{path.relative_to(ROOT)}: unreachable from specs/README.md")
    manifest = json.loads((SPECS / "models/verification.json").read_text())
    expected = {"AssetOwnership.tla": manifest["model_sha256"], **manifest["configs_sha256"]}
    for name, digest in expected.items():
        if hashlib.sha256((SPECS / "models" / name).read_bytes()).hexdigest() != digest:
            errors.append(
                f"specs/models/{name}: verification manifest is stale; rerun the model checker"
            )
    if errors:
        print("\n".join(errors), file=sys.stderr)
        return 1
    print(
        f"Specifications: {len(files)} files reachable, {link_count} local links, "
        f"{len(definitions)} uniquely owned invariants; test references and model hashes valid"
    )
    return 0


if __name__ == "__main__":
    sys.exit(main())
