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
   format, layout or page. What a kind may do, as far as it can be read off the source:
   - only a fetch module talks to the server, and no other module of `lib` imports a value
     from a fetch module (hooks and components do);
   - a types module exports no value;
   - a display, format or layout module imports no page module;
   - a display, format, layout or types module is pure: it imports neither React nor a hook
     and touches no storage, document, listener or timer.
3. Vocabulary. A word of barca's vocabulary is decided on only in a display module.
   - The words of `vocabulary.types` (the generated unions of health, cause and cache state)
     mean one thing wherever they are spelled: outside a display module, a comparison against
     one (`===`, `!==`, `case`, `.with(`, `.includes(`) or a branch that yields one
     (`return 'blocked'`, `? 'failing'`) fails.
   - The words of `vocabulary.status_types` (a run's, a step's, a queue entry's and a
     materialization's status, a node's last attempt) are also English (`failed`, `running`,
     `error`), a tone and a glyph. They are checked where a status is in hand: on a line that
     names one (a variable or field whose name contains `status`, or `outcome`,
     `last_attempt`, `lastAttempt`), and on the `.with(` lines of a `match(` over one.
   Either fails unless the table exempts that file and word with a reason.
4. Derivations. A file the check can see computing something is registered: under `derives`
   of its module, or, outside `lib`, under the table's own `derives` by file. An entry says
   `what`, why it stays (`reason`), which of the signals below it accounts for (`sees`) and
   either `kept: true` (presentation) or the `issue` that moves it to barca-core. The signals,
   each a heuristic over one line of source:
   - `count`: `.filter(…).length`, `.reduce(`, `name += 1` (a name of two letters or more);
   - `order`: `.sort(`, `.toSorted(`;
   - `word`: a function whose declared return type is a vocabulary type (`): CacheState {`),
     or a table from one vocabulary type to another (`Record<Health, Cause>`). This one holds
     in a display module too: it may map a word to a label, a tone or a glyph, and when it
     produces a word, that is registered.
   A signal with no entry that sees it fails, and so does an entry that sees a signal the file
   no longer has, so the list follows the code.
5. Fixtures. Nothing the app ships imports `lib/testdata` or `kit`.

It runs as a prek hook (prek.toml), so on every pull request, in every merge group and in
`scripts/check-fast.sh`; each run first proves, on seeded violations, that it still catches
them. To extend the table, see `contributing/ui-layering` on the site.

What it cannot check, and review still reads for:
- that a CLI command prints the same JSON as its route (the table is a claim);
- whether a count or an order is of served rows or of rows on screen: the signals see the
  shape of the code, so a registered `kept` entry is a reviewer's claim, not a proof;
- arithmetic over served numbers (`a.total - a.cached`, `median * keys`), a maximum or a
  minimum, a count kept by `++`, in a one-letter name or by `.size`/`.length` of a collection
  built for it;
- an order made without `sort` (an insertion, a `reverse`), and a walk of the graph;
- a word compared through a variable, built from parts, or held in a constant
  (`const FAILED = 'failed'`); a status word on a line that names no status (`s === 'failed'`
  after `const s = run.status`), inside a multi-line `.with(` body, or in a `P.union(...)`;
- a word produced under a local alias of a vocabulary type, with no declared return type, or
  as a field of a returned object;
- a boolean a display module derives from a word (`isFailed(status)`) and what its caller
  then decides with it;
- anything in a `.tsx` file's JSX beyond the lines above.
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
# `import … from '…'`, a bare `import '…'` and `export … from '…'`: whether it is type-only,
# and what it names.
IMPORT = re.compile(
    r"""^[ \t]*import\s+(type\s+)?(?:[\w*\s{},$]+?\s+from\s+)?['"]([^'"]+)['"]"""
    r"""|^[ \t]*export\s+(type\s+)?(?:\*(?:\s+as\s+\w+)?|\{[^}]*\})\s+from\s+['"]([^'"]+)['"]""",
    re.M,
)
# The kinds that only compute from their arguments: nothing of the page, the browser or time
# passing reaches them except through a parameter.
PURE_KINDS = {"display", "format", "layout", "types"}
IMPURE = re.compile(
    r"\b(?:localStorage|sessionStorage)\b|(?<![\w.])document\.|\baddEventListener\("
    r"|\bsetTimeout\(|\bsetInterval\(|\brequestAnimationFrame\("
)


def imports(text: str) -> list[tuple[str, bool]]:
    """`(specifier, type_only)` of every import and re-export of `text`."""
    out = []
    for match in IMPORT.finditer(strip_comments(text)):
        spec = match.group(2) or match.group(4)
        out.append((spec, bool(match.group(1) or match.group(3))))
    return out


def lib_module(spec: str, from_lib: bool) -> str | None:
    """The module of `ui/src/lib` an import names (`./tone`, `@/lib/tone`), else None."""
    for prefix in ("@/lib/",) + (("./",) if from_lib else ()):
        if spec.startswith(prefix):
            name = spec[len(prefix) :]
            return None if "/" in name else name
    return None


def check_kind(name: str, kind: str, text: str, modules: dict) -> list[str]:
    """What a module of `kind` may not do, beyond talking to the server."""
    problems = []
    for spec, type_only in imports(text):
        target = lib_module(spec, True)
        other = modules.get(target, {}).get("kind") if target else None
        if other == "fetch" and kind != "fetch" and not type_only:
            problems.append(
                f"lib/{name}.ts imports the fetch module lib/{target}.ts and is a {kind} module: "
                "a module of `lib` is handed what was fetched (a hook or a component fetches)"
            )
        if other == "page" and kind in ("display", "format", "layout"):
            problems.append(
                f"lib/{name}.ts imports the page module lib/{target}.ts and is a {kind} module: "
                "a page reads display, format and layout, never the other way"
            )
        if kind in PURE_KINDS and (spec == "react" or spec.startswith(("@/hooks", "../hooks"))):
            problems.append(
                f"lib/{name}.ts imports {spec} and is a {kind} module: it computes from its "
                "arguments alone"
            )
    if kind in PURE_KINDS and (found := IMPURE.search(strip_comments(text))):
        problems.append(
            f"lib/{name}.ts uses `{found.group(0).rstrip('(.')}` and is a {kind} module: it computes "
            "from its arguments alone (no storage, document, listener or timer)"
        )
    return problems


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
        problems += check_kind(name, kind, present[name], modules)
    return problems


# ── Derivations ───────────────────────────────────────────────────────────────────────────

# What a line of source looks like when it computes something. Heuristics: see the module
# documentation for what each one misses.
SIGNALS = {
    "count": re.compile(
        r"\.filter\([^\n]*\)\.length\b|\.reduce\(|(?<![\w.])[\w.\[\]'\"?!]{2,}\s*\+=\s*1\b"
    ),
    "order": re.compile(r"\.sort\(|\.toSorted\("),
}
SIGNAL_WORDS = {
    "count": "a count (`.filter(…).length`, `.reduce(`, `+= 1`)",
    "order": "an order (`.sort(`, `.toSorted(`)",
    "word": "a word of barca's vocabulary (a function that returns one, or a table from one "
    "vocabulary type to another)",
}


def signals(text: str, type_names: list[str]) -> dict[str, int]:
    """Each signal `text` shows, with the first line that shows it."""
    patterns = dict(SIGNALS)
    if type_names:
        t = "|".join(sorted(map(re.escape, type_names)))
        patterns["word"] = re.compile(
            rf"\)\s*:\s*(?:{t})\b(?:\s*\|\s*(?:null|undefined))*\s*(?:\{{|=>)"
            rf"|\bRecord<\s*(?:{t})\s*,\s*(?:{t})\s*>"
        )
    seen: dict[str, int] = {}
    for number, line in enumerate(strip_comments(text).split("\n"), 1):
        for name, pattern in patterns.items():
            if name not in seen and pattern.search(line):
                seen[name] = number
    return seen


def check_derives(table: dict, files: dict[str, str], type_names: list[str]) -> list[str]:
    problems = []
    modules = table["modules"]
    elsewhere = table.get("derives", {})
    for rel in elsewhere:
        if rel not in files:
            problems.append(f"derives: {rel} is listed and does not exist")
        elif re.fullmatch(r"lib/[^/]+\.ts", rel):
            problems.append(f"derives: {rel} is a module of `lib`: its entries go under `modules`")
    for rel, text in files.items():
        lib = re.fullmatch(r"lib/([^/]+)\.ts", rel)
        entry = modules.get(lib.group(1), {}) if lib else {}
        # Transport counts attempts and bytes, never rows.
        if entry.get("kind") == "fetch":
            continue
        entries = entry.get("derives", []) if lib else elsewhere.get(rel, [])
        claimed: set[str] = set()
        for owed in entries:
            said = str(owed.get("what", "")).strip() and str(owed.get("reason", "")).strip()
            sees = owed.get("sees", [])
            # Owed work names the issue that removes it; what stays (presentation) says `kept`.
            if not said or isinstance(owed.get("issue"), int) == bool(owed.get("kept")):
                problems.append(
                    f"derives: {rel} entries need `what`, `reason` and either the `issue` that "
                    "removes them or `kept: true`"
                )
            if not isinstance(sees, list) or any(s not in SIGNAL_WORDS for s in sees):
                problems.append(
                    f"derives: {rel} `sees` lists signals: {', '.join(sorted(SIGNAL_WORDS))}"
                )
                continue
            claimed |= set(sees)
        seen = signals(text, type_names)
        for name, number in sorted(seen.items()):
            if name not in claimed:
                problems.append(
                    f"{rel}:{number}: computes {SIGNAL_WORDS[name]} and no `derives` entry of the file "
                    f"sees it. If it is of served rows, compute it in barca-core and serve it; if it "
                    f'is presentation, register it: `what`, `reason`, `sees: ["{name}"]`, `kept: true`'
                )
        for name in sorted(claimed - set(seen)):
            problems.append(
                f"derives: {rel} has an entry that sees a `{name}` the file no longer computes: "
                "remove the signal, or the entry"
            )
    return problems


# ── Vocabulary ────────────────────────────────────────────────────────────────────────────


class Vocabulary:
    """The words the check knows: `words` mean one thing wherever they are spelled, `status`
    words are checked where a status is in hand, `types` are the unions both come from."""

    def __init__(self, words: set[str], status: set[str] = frozenset(), types: list[str] = ()):
        self.words = set(words)
        self.status = set(status) - self.words
        self.types = list(types)


def union_words(generated: Path, name: str, key: str) -> set[str]:
    text = (generated / f"{name}.ts").read_text()
    union = re.search(rf"export type {name} = ([^;]+);", text)
    if not union:
        raise SystemExit(f"ui-layering: {name}.ts is not a union of words; fix `vocabulary.{key}`")
    return set(re.findall(r'"([a-z_]+)"', union.group(1)))


def vocabulary(table: dict, generated: Path) -> Vocabulary:
    """The values of the generated union types named in the table, less the ambiguous words."""
    listed = table["vocabulary"]
    ambiguous = set(listed["ambiguous"])
    words: set[str] = set()
    status: set[str] = set()
    for name in listed["types"]:
        words |= union_words(generated, name, "types")
    for name in listed.get("status_types", []):
        status |= union_words(generated, name, "status_types")
    return Vocabulary(
        words - ambiguous,
        status - ambiguous,
        [*listed["types"], *listed.get("status_types", [])],
    )


def decision_patterns(words: set[str]) -> list[re.Pattern]:
    w = "|".join(sorted(map(re.escape, words)))
    lit = rf"['\"]({w})['\"]"
    return [
        re.compile(rf"(?:===|!==|==|!=)\s*{lit}"),
        re.compile(rf"{lit}\s*(?:===|!==|==|!=)"),
        re.compile(rf"\bcase\s+{lit}"),
        re.compile(rf"\.with\(\s*{lit}"),
        re.compile(rf"\.with\(\s*\{{[^\n]*?\bstatus:\s*{lit}"),
        re.compile(rf"\.includes\(\s*{lit}"),
        re.compile(rf"\breturn\s+{lit}"),
        re.compile(rf"\?\s*{lit}"),
    ]


def decisions(text: str, words: set[str]) -> list[tuple[int, str]]:
    """`(line, word)` for every place `text` decides on, or yields, a word of the vocabulary."""
    if not words:
        return []
    patterns = decision_patterns(words)
    out = set()
    for number, line in enumerate(strip_comments(text).split("\n"), 1):
        for pattern in patterns:
            for match in pattern.finditer(line):
                out.add((number, match.group(1)))
    return sorted(out)


# A name that holds a status: `run.status`, `last_status`, `feedbackStatus`, `outcome`. A name
# that starts with a capital is a component or a type (`StatusGlyph`), not a value.
STATUS_NAME = re.compile(
    r"\b(?:status\w*|[a-z_]\w*Status\w*|[a-z_]\w*_status\w*|outcome|last_attempt|lastAttempt)\b"
)
STRING = re.compile(r"'(?:[^'\\\n]|\\.)*'|\"(?:[^\"\\\n]|\\.)*\"")


def status_decisions(text: str, words: set[str]) -> list[tuple[int, str]]:
    """`decisions` for the status words, which are English too: only on a line that names a
    status outside its strings, and on the `.with(` lines that follow a `match(` over one."""
    if not words:
        return []
    patterns = decision_patterns(words)
    out = set()
    chain = False
    for number, line in enumerate(strip_comments(text).split("\n"), 1):
        named = bool(STATUS_NAME.search(STRING.sub("''", line)))
        start = line.lstrip()
        if "match(" in line or "match<" in line:
            chain = named
        elif not start.startswith("."):
            chain = False
        if not (named or (chain and start.startswith(".with("))):
            continue
        for pattern in patterns:
            for match in pattern.finditer(line):
                out.add((number, match.group(1)))
    return sorted(out)


def check_vocabulary(table: dict, files: dict[str, str], vocab: Vocabulary) -> list[str]:
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
        found = set(decisions(text, vocab.words)) | set(status_decisions(text, vocab.status))
        for number, word in sorted(found):
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


def check(table: dict, files: dict[str, str], vocab: Vocabulary) -> list[str]:
    return (
        check_routes(table, files)
        + check_modules(table, files)
        + check_vocabulary(table, files, vocab)
        + check_derives(table, files, vocab.types)
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
            "table": {
                "kind": "layout",
                "does": "rows",
                "derives": [
                    {
                        "what": "the rows by name",
                        "reason": "layout: the order of a list on screen",
                        "sees": ["order"],
                        "kept": True,
                    }
                ],
            },
            "filters": {"kind": "page", "does": "a page's filters"},
        },
        "derives": {
            "pages/List.tsx": [
                {
                    "what": "how many rows are listed",
                    "reason": "presentation: a count of what is on screen",
                    "sees": ["count"],
                    "kept": True,
                }
            ]
        },
        "vocabulary": {"types": [], "ambiguous": [], "display_components": [], "exemptions": []},
    }
    vocab = Vocabulary(
        {"failing", "blocked", "stale", "behind"},
        {"failed", "running", "success", "pending"},
        ["Health", "CacheState", "RunViewStatus"],
    )
    clean = {
        "lib/api.ts": "export const api = { state: () => request<NodeState[]>('/state') }\n",
        # A display module maps a word to a tone, and a status to a word of its own.
        "lib/healthDisplay.ts": "import type { Health } from './generated/Health'\n"
        "export const tone = (h: Health) => (h === 'failing' ? 'failed' : 'neutral')\n"
        "export const live = (status: string): boolean => status === 'running'\n",
        "lib/table.ts": "// a comment may say 'blocked'\n"
        "import { tone } from './healthDisplay'\n"
        "import type { Api } from './api'\n"
        "export const rows = (n: Node[]) => n.map((x) => x.id).sort()\n",
        "lib/filters.ts": "import { rows } from './table'\nexport const shown = rows\n",
        # `failed` is a tone and `running` a glyph where no status is named.
        "components/Row.tsx": "const shown = broken ? 'failed' : 'neutral'\nconst glyph = busy ? 'running' : 'idle'\n",
        "pages/List.tsx": "const n = rows.filter((r) => r.shown).length\n",
        "hooks/useList.ts": "import { api } from '@/lib/api'\nexport const useList = () => api.state()\n",
    }
    failures = []
    if problems := check(table, clean, vocab):
        failures.append(f"a clean tree is reported: {problems}")

    def seeded(name: str, files: dict[str, str], expect: str, over: dict | None = None):
        problems = check({**table, **(over or {})}, {**clean, **files}, vocab)
        if not any(expect in p for p in problems):
            failures.append(
                f"not caught: {name} (expected a problem containing {expect!r}, got {problems})"
            )

    def table_with(module: str, entry: dict) -> dict:
        return {"modules": {**table["modules"], module: entry}}

    # ── a status is a word of the vocabulary where one is in hand ──
    seeded(
        "a run status compared outside a display module",
        {"components/Row.tsx": "const bad = run.status === 'failed'\n"},
        "components/Row.tsx:1: decides on 'failed'",
    )
    seeded(
        "a status rewritten by a branch",
        {"pages/Page.tsx": "const last = { status: keys.failed ? 'failed' : last.status }\n"},
        "pages/Page.tsx:1: decides on 'failed'",
    )
    seeded(
        "a match over a status, a line a word",
        {
            "pages/Page.tsx": "const word = match(run.status)\n  .with('pending', () => 'waiting')\n  .with('success', () => 'done')\n  .otherwise(() => '')\n"
        },
        "pages/Page.tsx:3: decides on 'success'",
    )
    seeded(
        "a status matched as a field",
        {"pages/Page.tsx": "match(outcome).with({ status: 'success' }, () => 1)\n"},
        "pages/Page.tsx:1: decides on 'success'",
    )
    # ── what a kind may do ──
    seeded(
        "a value of a fetch module in a layout module",
        {"lib/table.ts": "import { api } from './api'\nexport const rows = () => api\n"},
        "lib/table.ts imports the fetch module lib/api.ts",
    )
    seeded(
        "a page module read by a layout module",
        {"lib/table.ts": "import { shown } from '@/lib/filters'\nexport const rows = shown\n"},
        "lib/table.ts imports the page module lib/filters.ts",
    )
    seeded(
        "a hook in a display module",
        {
            "lib/healthDisplay.ts": "import { useMemo } from 'react'\nexport const tone = () => useMemo(() => 'neutral', [])\n"
        },
        "lib/healthDisplay.ts imports react",
    )
    seeded(
        "storage in a layout module",
        {"lib/table.ts": "export const width = () => Number(localStorage.getItem('w')) || 200\n"},
        "lib/table.ts uses `localStorage`",
    )
    # ── a derivation is registered ──
    seeded(
        "a display module that produces a word",
        {
            "lib/healthDisplay.ts": "export function shown(n: Node): CacheState {\n  return n.state\n}\n"
        },
        "lib/healthDisplay.ts:1: computes a word",
    )
    seeded(
        "a table from one word to another",
        {"lib/healthDisplay.ts": "const BY: Record<Health, CacheState> = TABLE\n"},
        "lib/healthDisplay.ts:1: computes a word",
    )
    seeded(
        "a count nobody registered",
        {"components/Row.tsx": "const failed = rows.filter((r) => r.bad).length\n"},
        "components/Row.tsx:1: computes a count",
    )
    seeded(
        "a tally nobody registered",
        {
            "lib/filters.ts": "export function tally(rows: Row[]) {\n  for (const r of rows) counts[r.kind] += 1\n}\n"
        },
        "lib/filters.ts:2: computes a count",
    )
    seeded(
        "an order nobody registered",
        {"hooks/useList.ts": "export const first = (rows: Row[]) => rows.toSorted(byAge)[0]\n"},
        "hooks/useList.ts:1: computes an order",
    )
    seeded(
        "an entry for a signal that is gone",
        {"lib/table.ts": "export const rows = (n: Node[]) => n.map((x) => x.id)\n"},
        "sees a `order` the file no longer computes",
    )
    seeded(
        "an entry that neither stays nor names its issue",
        {},
        "either the `issue` that removes them or `kept: true`",
        table_with(
            "table",
            {
                **table["modules"]["table"],
                "derives": [{"what": "x", "reason": "y", "sees": ["order"]}],
            },
        ),
    )
    seeded(
        "an entry for a file that is gone",
        {},
        "derives: pages/Gone.tsx is listed and does not exist",
        {"derives": {**table["derives"], "pages/Gone.tsx": []}},
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
