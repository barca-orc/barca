#!/usr/bin/env python3
"""The web UI only renders (#959, DIRECTION.md "The UI", specs/behavior/health.md).

Every derived answer (health, cause, rollup, run explanation, lateness, data signals) is computed
once in `barca-core`; TypeScript derives nothing, and an agent on the CLI can get everything the
UI shows. This check holds the UI's sources to that, against the table in
`crates/barca-cli/ui-layering.json`:

1. Routes. Every server route the UI calls is in the table, mapped to the CLI command that
   prints the same answer or exempted with a reason. The UI reaches the server only through
   `request` (lib/api.ts), `serverFetch` (lib/credentials.ts) and `followEvents`
   (lib/eventStream.ts), so a call with a literal path is a route; nothing else may fetch.
   (`cargo test -p barca ui_layering` checks the other half: each command is a real CLI
   command line and each route is one the server serves.)
2. Modules. Every module of `ui/src/lib` is in the table with its kind: fetch, types, display,
   format, layout or page. Only a fetch module talks to the server; only a types module is
   free of values.
3. Vocabulary. A word of barca's vocabulary (a `Health`, `HealthReason` or `CacheState` value,
   read from the generated types) is decided on only in a display module: anywhere else, a
   comparison against one (`===`, `!==`, `case`, `.with(`, `.includes(`) or a branch that
   yields one (`return 'blocked'`, `? 'failing'`) fails unless the table exempts that file and
   word with a reason and the issue that removes it.
4. Fixtures. Nothing the app ships imports `lib/testdata` or `kit`.

It runs as a prek hook (prek.toml), so on every pull request, in every merge group and in
`scripts/check-fast.sh`; each run first proves, on seeded violations, that it still catches
them. To extend the table, see `contributing/ui-layering` on the site.

What it cannot check: that a CLI command prints the same JSON as its route (the table is a
claim a reviewer reads); arithmetic over served numbers; a word compared through a variable or
built from parts; a module doing more than its kind says.
"""

from __future__ import annotations

import json
import re
import sys
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent
UI = ROOT / "ui" / "src"
TABLE = ROOT / "crates" / "barca-cli" / "ui-layering.json"

KINDS = {"fetch", "types", "display", "format", "layout", "page"}
# The only ways the UI reaches the server; a call of one with a literal path is a route.
CALLERS = ("request<", "serverFetch(", "followEvents(")
# What may not appear at all outside lib/credentials.ts: a way around the three above.
RAW_IO = re.compile(
    r"(?<![\w.])fetch\(|new EventSource\b|new XMLHttpRequest\b|new WebSocket\b|sendBeacon\("
)


def app_files(ui: Path = UI) -> list[Path]:
    """The sources the app ships: no tests, no kit, no fixtures, no generated types."""
    out = []
    for path in sorted(ui.rglob("*.ts*")):
        rel = path.relative_to(ui).as_posix()
        if path.suffix not in (".ts", ".tsx") or ".test." in path.name:
            continue
        if rel.startswith(("kit/", "lib/testdata/", "lib/generated/")) or rel.endswith(".d.ts"):
            continue
        out.append(path)
    return out


def strip_comments(text: str) -> str:
    """`text` with comments blanked, keeping line numbers and string contents."""
    out = []
    i, n = 0, len(text)
    quote = None
    while i < n:
        c = text[i]
        if quote:
            out.append(c)
            if c == "\\" and i + 1 < n:
                out.append(text[i + 1])
                i += 2
                continue
            if c == quote:
                quote = None
            i += 1
        elif c in "'\"`":
            quote = c
            out.append(c)
            i += 1
        elif text.startswith("//", i):
            while i < n and text[i] != "\n":
                i += 1
        elif text.startswith("/*", i):
            end = text.find("*/", i + 2)
            end = n if end < 0 else end + 2
            out.append("".join("\n" if ch == "\n" else " " for ch in text[i:end]))
            i = end
        else:
            out.append(c)
            i += 1
    return "".join(out)


# ── Routes ────────────────────────────────────────────────────────────────────────────────


def call_text(text: str, start: int) -> str:
    """The argument list of the call whose name ends at `start` (after a generic, if any)."""
    i = start
    if text[i - 1] == "<":
        depth = 1
        while i < len(text) and depth:
            # `=>` inside a generic is an arrow, not a closing bracket.
            if text[i] == "<":
                depth += 1
            elif text[i] == ">" and text[i - 1] != "=":
                depth -= 1
            i += 1
        if i >= len(text) or text[i] != "(":
            return ""
        i += 1
    begin, depth, quote = i, 1, None
    while i < len(text) and depth:
        c = text[i]
        if quote:
            if c == "\\":
                i += 1
            elif c == quote:
                quote = None
        elif c in "'\"":
            quote = c
        elif c in "([{":
            depth += 1
        elif c in ")]}":
            depth -= 1
        i += 1
    return text[begin : i - 1]


def literals(call: str) -> list[str]:
    """Every string or template literal in `call` that starts with `/`, placeholders as `{}`."""
    out = []
    i = 0
    while i < len(call):
        c = call[i]
        if c in "'\"":
            end = call.find(c, i + 1)
            end = len(call) if end < 0 else end
            if call[i + 1 : i + 2] == "/":
                out.append(call[i + 1 : end])
            i = end + 1
        elif c == "`":
            j, parts = i + 1, []
            while j < len(call) and call[j] != "`":
                if call.startswith("${", j):
                    depth, j = 1, j + 2
                    while j < len(call) and depth:
                        if call[j] == "`":  # a nested template inside the placeholder
                            j = call.find("`", j + 1)
                            j = len(call) if j < 0 else j
                        elif call[j] == "{":
                            depth += 1
                        elif call[j] == "}":
                            depth -= 1
                        j += 1
                    parts.append("{}")
                else:
                    parts.append(call[j])
                    j += 1
            text = "".join(parts)
            if text.startswith("/"):
                out.append(text)
            i = j + 1
        else:
            i += 1
    return out


def normalize(path: str) -> str:
    """`/assets/{}/partitions?{}` is `/assets/{}/partitions`; a trailing `{}` that is not a
    segment of its own (an optional query string) is dropped."""
    path = path.split("?")[0]
    path = re.sub(r"(?<!/)\{\}", "", path)
    return path.rstrip("/") or "/"


def routes_in(text: str) -> tuple[set[tuple[str, str]], int]:
    """The `(method, path)` of every call in `text`, and how many calls name no literal path."""
    text = strip_comments(text)
    found: set[tuple[str, str]] = set()
    opaque = 0
    for caller in CALLERS:
        at = 0
        while (at := text.find(caller, at)) >= 0:
            at += len(caller)
            before = text[at - len(caller) - 1 : at - len(caller)]
            # A definition (`function request<T>(…)`) or a longer name is not a call.
            if before.isalnum() or before in "_.":
                continue
            # `function request<T>(…)` defines the caller; it is not a call.
            if text[: at - len(caller)].rstrip().endswith("function"):
                continue
            call = call_text(text, at)
            paths = [normalize(p) for p in literals(call)]
            if not paths:
                opaque += 1
                continue
            method = re.search(r"method:\s*['\"](\w+)['\"]", call)
            for path in paths:
                found.add((method.group(1) if method else "GET", path))
    return found, opaque


def check_routes(table: dict, files: dict[str, str]) -> list[str]:
    problems = []
    listed = {(r["method"], r["path"]): r for r in table["routes"]}
    passed_in = set(table["routes_passed_in"])
    used: set[tuple[str, str]] = set()
    for rel, text in files.items():
        if rel != "lib/credentials.ts" and RAW_IO.search(strip_comments(text)):
            problems.append(
                f"{rel}: reaches the network without `request`, `serverFetch` or `followEvents`"
            )
        found, opaque = routes_in(text)
        if opaque and rel not in passed_in:
            problems.append(
                f"{rel}: {opaque} server call(s) with no literal path. Write the path at the call, "
                "or list the file under `routes_passed_in` (a function that is handed its path)"
            )
        for route in sorted(found):
            used.add(route)
            if route not in listed:
                problems.append(
                    f"{rel}: calls {route[0]} {route[1]}, which is not in `routes`. Add it with the CLI "
                    "command that prints the same answer (`cli`), or `exempt` it with a reason"
                )
    for route, entry in listed.items():
        if route not in used:
            problems.append(
                f"routes: {route[0]} {route[1]} is listed and the UI no longer calls it"
            )
        has_cli, exempt = bool(entry.get("cli")), entry.get("exempt")
        if has_cli == bool(exempt):
            problems.append(
                f"routes: {route[0]} {route[1]} needs exactly one of `cli` and `exempt`"
            )
        if exempt and not str(exempt.get("reason", "")).strip():
            problems.append(f"routes: {route[0]} {route[1]} is exempt without a reason")
    return problems


# ── Modules ───────────────────────────────────────────────────────────────────────────────

VALUE_EXPORT = re.compile(r"^export\s+(?:async\s+)?(?:function|const|let|class|enum)\b", re.M)


def check_modules(table: dict, files: dict[str, str]) -> list[str]:
    problems = []
    modules = table["modules"]
    present = {
        rel[len("lib/") : -len(".ts")]: text
        for rel, text in files.items()
        if re.fullmatch(r"lib/[^/]+\.ts", rel)
    }
    for rel in files:
        if rel.startswith("lib/") and rel.count("/") > 1:
            problems.append(f"{rel}: `ui/src/lib` has no folders but `generated` and `testdata`")
    for name in sorted(present):
        if name not in modules:
            problems.append(
                f"lib/{name}.ts is not in `modules`. Give it a kind ({', '.join(sorted(KINDS))}); a module that "
                "derives a barca answer belongs in barca-core"
            )
    for name, entry in modules.items():
        kind = entry.get("kind")
        if name not in present:
            problems.append(f"modules: lib/{name}.ts is listed and does not exist")
            continue
        if kind not in KINDS:
            problems.append(
                f"modules: lib/{name}.ts has kind {kind!r}; the kinds are {', '.join(sorted(KINDS))}"
            )
            continue
        if not str(entry.get("does", "")).strip():
            problems.append(f"modules: lib/{name}.ts needs `does`, a line on what it is")
        text = strip_comments(present[name])
        calls = (
            any(routes_in(present[name])[0])
            or routes_in(present[name])[1] > 0
            or RAW_IO.search(text)
        )
        if calls and kind != "fetch":
            problems.append(
                f"lib/{name}.ts talks to the server and is a {kind} module: only a fetch module does"
            )
        if kind == "types" and VALUE_EXPORT.search(text):
            problems.append(f"lib/{name}.ts is a types module and exports a value")
        for owed in entry.get("derives", []):
            said = str(owed.get("what", "")).strip() and str(owed.get("reason", "")).strip()
            # Owed work names the issue that removes it; what stays (layout) says `kept`.
            if not said or isinstance(owed.get("issue"), int) == bool(owed.get("kept")):
                problems.append(
                    f"modules: lib/{name}.ts `derives` entries need `what`, `reason` and either the `issue` that removes them or `kept: true`"
                )
    return problems


# ── Vocabulary ────────────────────────────────────────────────────────────────────────────


def vocabulary(table: dict, generated: Path) -> set[str]:
    """The values of the generated union types named in the table, less the ambiguous words."""
    words: set[str] = set()
    for name in table["vocabulary"]["types"]:
        text = (generated / f"{name}.ts").read_text()
        union = re.search(rf"export type {name} = ([^;]+);", text)
        if not union:
            raise SystemExit(
                f"ui-layering: {name}.ts is not a union of words; fix `vocabulary.types`"
            )
        words |= set(re.findall(r'"([a-z_]+)"', union.group(1)))
    return words - set(table["vocabulary"]["ambiguous"])


def decisions(text: str, words: set[str]) -> list[tuple[int, str]]:
    """`(line, word)` for every place `text` decides on, or yields, a word of the vocabulary."""
    if not words:
        return []
    w = "|".join(sorted(map(re.escape, words)))
    lit = rf"['\"]({w})['\"]"
    patterns = [
        re.compile(rf"(?:===|!==|==|!=)\s*{lit}"),
        re.compile(rf"{lit}\s*(?:===|!==|==|!=)"),
        re.compile(rf"\bcase\s+{lit}"),
        re.compile(rf"\.with\(\s*{lit}"),
        re.compile(rf"\.includes\(\s*{lit}"),
        re.compile(rf"\breturn\s+{lit}"),
        re.compile(rf"\?\s*{lit}"),
    ]
    out = set()
    for number, line in enumerate(strip_comments(text).split("\n"), 1):
        for pattern in patterns:
            for match in pattern.finditer(line):
                out.add((number, match.group(1)))
    return sorted(out)


def check_vocabulary(table: dict, files: dict[str, str], words: set[str]) -> list[str]:
    problems = []
    display = {
        f"lib/{name}.ts" for name, m in table["modules"].items() if m.get("kind") == "display"
    }
    display |= set(table["vocabulary"]["display_components"])
    exempt: dict[str, dict] = {}
    for entry in table["vocabulary"]["exemptions"]:
        exempt[entry["file"]] = entry
        if not str(entry.get("reason", "")).strip():
            problems.append(f"vocabulary: the exemption of {entry['file']} needs a reason")
    seen: dict[str, set[str]] = {}
    for rel, text in files.items():
        if rel in display:
            continue
        for number, word in decisions(text, words):
            seen.setdefault(rel, set()).add(word)
            if word not in exempt.get(rel, {}).get("words", []):
                problems.append(
                    f"{rel}:{number}: decides on '{word}', a word of barca's vocabulary. TypeScript derives "
                    "nothing: read the answer from the server (compute it in barca-core), or map the word "
                    "to its display in a display module"
                )
    for rel, entry in exempt.items():
        for word in entry.get("words", []):
            if word not in seen.get(rel, set()):
                problems.append(
                    f"vocabulary: {rel} is exempted for '{word}' and no longer decides on it: remove the exemption"
                )
    return problems


# ── Fixtures ──────────────────────────────────────────────────────────────────────────────

FIXTURE_IMPORT = re.compile(r"""from\s+['"][^'"]*(?:/testdata/|/kit/|@/kit\b)[^'"]*['"]""")


def check_fixtures(files: dict[str, str]) -> list[str]:
    allowed = {"pages/KitPage.tsx"}  # the kit page itself: development only, not in the built UI
    return [
        f"{rel}: imports test fixtures (`lib/testdata` or `kit`), which the app must not ship"
        for rel, text in files.items()
        if rel not in allowed and FIXTURE_IMPORT.search(strip_comments(text))
    ]


# ── The run ───────────────────────────────────────────────────────────────────────────────


def check(table: dict, files: dict[str, str], words: set[str]) -> list[str]:
    return (
        check_routes(table, files)
        + check_modules(table, files)
        + check_vocabulary(table, files, words)
        + check_fixtures(files)
    )


def self_test() -> list[str]:
    """Seeded violations the check must catch: if one passes, the guard has gone blind."""
    table = {
        "routes": [{"method": "GET", "path": "/state", "cli": "barca status --json"}],
        "routes_passed_in": [],
        "modules": {
            "api": {"kind": "fetch", "does": "fetches"},
            "healthDisplay": {"kind": "display", "does": "words"},
            "table": {"kind": "layout", "does": "rows"},
        },
        "vocabulary": {"types": [], "ambiguous": [], "display_components": [], "exemptions": []},
    }
    words = {"failing", "blocked", "stale", "behind"}
    clean = {
        "lib/api.ts": "export const api = { state: () => request<NodeState[]>('/state') }\n",
        "lib/healthDisplay.ts": "export const tone = (h: Health) => (h === 'failing' ? 'failed' : 'neutral')\n",
        "lib/table.ts": "// a comment may say 'blocked'\nexport const rows = (n: Node[]) => n.map((x) => x.id)\n",
    }
    failures = []
    if problems := check(table, clean, words):
        failures.append(f"a clean tree is reported: {problems}")

    def seeded(name: str, files: dict[str, str], expect: str, over: dict | None = None):
        problems = check({**table, **(over or {})}, {**clean, **files}, words)
        if not any(expect in p for p in problems):
            failures.append(
                f"not caught: {name} (expected a problem containing {expect!r}, got {problems})"
            )

    seeded(
        "a health word derived again in TypeScript",
        {
            "lib/table.ts": "export function health(n: Node) {\n  if (n.cache.state === 'stale' && n.upstreamFailed) return 'blocked'\n  return 'behind'\n}\n"
        },
        "lib/table.ts:2: decides on 'stale'",
    )
    seeded(
        "a health word yielded by a branch in a component",
        {"components/Row.tsx": "const word = failed ? 'failing' : label\n"},
        "components/Row.tsx:1: decides on 'failing'",
    )
    seeded(
        "a match on a cache state outside a display module",
        {"pages/Page.tsx": "match(state).with('blocked', () => 1).otherwise(() => 0)\n"},
        "pages/Page.tsx:1: decides on 'blocked'",
    )
    seeded(
        "a route with no CLI equivalent",
        {
            "lib/api.ts": clean["lib/api.ts"]
            + "const x = () => request<X>(`/secret/${id}?${q}`, { method: 'POST' })\n"
        },
        "calls POST /secret/{}",
    )
    seeded(
        "a fetch around the client",
        {"hooks/useThing.ts": "const r = await fetch('/state')\n"},
        "reaches the network",
    )
    seeded(
        "a server call from a layout module",
        {"lib/table.ts": "export const rows = () => serverFetch('/state')\n"},
        "only a fetch module does",
    )
    seeded(
        "a module without a kind",
        {"lib/scoring.ts": "export const score = 1\n"},
        "lib/scoring.ts is not in `modules`",
    )
    seeded(
        "a route nothing calls",
        {"lib/api.ts": "export const api = {}\n"},
        "the UI no longer calls it",
    )
    seeded(
        "fixtures in the app",
        {"pages/Page.tsx": "import { served } from '@/lib/testdata/served'\n"},
        "imports test fixtures",
    )
    seeded(
        "a stale exemption",
        {},
        "no longer decides on it",
        {
            "vocabulary": {
                **table["vocabulary"],
                "exemptions": [
                    {"file": "lib/table.ts", "words": ["stale"], "reason": "x", "issue": 1}
                ],
            }
        },
    )
    return failures


def main() -> int:
    failures = self_test()
    if failures:
        print(
            "ui-layering: the check no longer catches what it is for:",
            *failures,
            sep="\n  ",
            file=sys.stderr,
        )
        return 1
    table = json.loads(TABLE.read_text())
    files = {p.relative_to(UI).as_posix(): p.read_text() for p in app_files()}
    problems = check(table, files, vocabulary(table, UI / "lib" / "generated"))
    if problems:
        print(
            "ui-layering (#959): the web UI only renders; every derived answer is computed in barca-core.",
            *problems,
            sep="\n  ",
            file=sys.stderr,
        )
        print(
            f"\nThe table is {TABLE.relative_to(ROOT)}; how to extend it: site/src/content/docs/contributing/ui-layering.md",
            file=sys.stderr,
        )
        return 1
    return 0


if __name__ == "__main__":
    sys.exit(main())
