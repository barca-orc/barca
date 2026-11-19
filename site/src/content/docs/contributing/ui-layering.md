---
pagefind: false
title: UI layering
description: The web UI only renders. What that means, the checks that hold it, and how to extend their table.
---

The web UI and the CLI render; they derive nothing. Every answer that takes two facts to
produce a third (a node's health, what blocks it, a group's counts, a run's explanation, whether
a schedule is late, a data signal) is computed once, in `barca-core`, and printed as is by
`barca status --json`, `barca history --json` and the server's routes. So the map, the bars
and the CLI cannot disagree, and an agent on the CLI can get everything the UI shows. The rule
is in `DIRECTION.md` ("The UI") and `specs/behavior/health.md` ("Four layers").

A static map from an enum value to a word, a tone or a glyph is presentation. Anything else
that reads a state belongs in Rust.

## What is checked

The table is `crates/barca-cli/ui-layering.json`. Two checks hold it:

| Check | Runs | Holds |
|---|---|---|
| `scripts/check-ui-layering.py` | a prek hook (`prek.toml`): every pull request and merge group in every CI tier, UI-only diffs included, and `scripts/check-fast.sh` | the UI's sources (`ui/src`) to the table |
| `cargo test -p barca ui_layering` | the Rust tests (the merge tier) | the table to the real CLI and the server's router |
| `cargo test -p barca-core ui_fixture` | the Rust tests | the UI's fixtures (`ui/src/lib/testdata/served/*.json`, `health-alone.json`, `weights.json`) to what `barca-core` computes |

Together they fail when:

- **A route has no CLI equivalent.** Every server route the UI calls (through `request` in
  `lib/api.ts`, `serverFetch` or `followEvents`, the only ways it may reach the server) is in
  `routes`, with the CLI command that prints the same answer (`cli`) or an exemption (`exempt`)
  that says why: authentication, an event stream, or a gap with the issue that closes it. The
  command must be one the real CLI parses, and the route one the server serves with that method.
- **A module of `ui/src/lib` has no kind.** `modules` lists every module with what it is: `fetch`
  (talks to the server), `types` (generated wire types and the few shapes the server builds by
  hand; no values), `display` (an enum value to its word, tone or glyph), `format` (text from
  served fields), `layout` (positions, rows, trees, geometry) or `page` (a page's URL, model and
  program). Only a fetch module talks to the server.
- **A word of barca's vocabulary is decided in TypeScript.** The words are the values of
  `Health`, `HealthReason`, `CacheState`, `Cause`, `RootKind` and `ActionKind`, read from the
  generated types (less the words too common to mean one: `vocabulary.ambiguous`). Outside a display
  module, a comparison against one (`===`, `!==`, `case`, `.with(`, `.includes(`) or a branch
  that yields one (`return 'blocked'`, `? 'failing'`) fails.
- **The app imports fixtures.** `lib/testdata` and `kit` are for tests and the kit page.
- **The generated types drift.** CI regenerates `ui/src/lib/generated` from Rust and fails on a
  difference (`pnpm --dir ui gen:types`), so a field the UI reads exists in Rust.

Each run of the script first seeds violations into a small tree and fails if one gets through.

What they cannot check: that a command prints the same JSON as its route (the table is a claim
a reviewer reads); arithmetic over served numbers; a word compared through a variable; a
module that does more than its kind says. Review still reads for those.

## When a check fails

Move the derivation to `barca-core` and serve the answer; that is the fix in nearly every case.
A new field on a status row or a run is generated into `ui/src/lib/generated` and reaches the
CLI's JSON in the same change (`CLAUDE.md`, "CLI and manual are part of every feature").

Extend the table only for what is true of the new code:

- **A new route**: add `{"method", "path", "cli"}` to `routes`, the path with `{}` for each
  parameter and without its query string. If the CLI has no command for it, add one when it is
  small; otherwise file an issue and write `"exempt": {"reason": "...", "issue": N}`.
- **A new module**: add it to `modules` with its `kind` and a line on what it `does`. A module
  that would need a seventh kind is deriving something.
- **A display mapping**: put it in a `display` module (`healthDisplay.ts`,
  `cacheStateDisplay.ts`, `glyph.ts`), not in the component that draws it.
- **A false positive** (a glyph name or a run step's status that spells the same as a state):
  add `{"file", "words", "reason"}` to `vocabulary.exemptions`. An exemption for a derivation
  that is still owed names the `issue` that removes it, and `derives` on its module says what
  is owed. An exemption nothing uses any more fails the check, so the list only shrinks.

## Fixtures

The UI's unit tests and its kit page need rows "as the server would serve them", and a value
someone typed can drift from what the server computes. So the values come from Rust
(`crates/barca-core/src/ui_fixture_tests.rs`), which fails when a checked-in file differs from
what the code produces:

- **A project as served** (`ui/src/lib/testdata/served/`, read through
  `ui/src/lib/testdata/project.ts`). The test copies the fixture project
  (`crates/barca-core/testdata/ui_fixture/`: a node that is fresh, stale, failing, blocked,
  never run, paused, pinned, partitioned with failed keys, on a late schedule, covered by a
  queued run), records a history for it with a fixed clock and writes the rows of `GET /state`,
  `GET /state/rollup`, the run list, four run details, the pauses, the pins and the
  explanations of runs in flight. A test that asserts on something the server computes (a
  health, a cause, a root, an estimate, a weight, a count, a headline, a schedule's standing, a
  line of a run's explanation) reads it from there.
- **A row spelled by hand** (`ui/src/lib/testdata/served.ts`) gets its `health`, reason and
  cause by lookup in `health-alone.json`, which the same test writes by running
  `health::classify` on each such row. A fixture that states a health states its reason and
  cause with it (`blockedBy` and `LATE_SCHEDULE` read them off the project's rows): no table
  from a health to a reason or a cause is written in TypeScript, in tests either.

Fixtures spelled by hand remain for presentation alone: a layout, the words for a given number,
an empty state, a size the fixture project does not have. The server-side joins that live in
`barca-server` (`NodeState` beyond its status row, `RunSummary`, the record half of `RunView`)
are repeated in the Rust test from the same reads, so a field added there is added to the test
by hand. After changing the health table, a wording, a weight or the fixture project:

```bash
BARCA_UPDATE_SNAPSHOTS=1 cargo test -p barca-core ui_fixture
pnpm --dir ui test
```
