# Lexical rebinding and unused-input warnings (#300 / P06)

## Boundary

Fix the rebind collector used by the existing conservative unused-input warning.
A name captured by a match pattern or bound by a nested function/class cannot reliably
refer to a known unrelated imported module. Treat SQL-like entry points on that receiver
as potentially dynamic, preserving the existing warning suppression rule.

No Python planner imports, execution changes, public API, parser rewrite or scope model
replacement. The collector remains deliberately conservative across nested scopes.

## Implementation and evidence

1. Add parser-level regression cases for capture/as/star/mapping patterns and nested
   function, async function and class names shadowing an unrelated module. Establish that
   those cases currently warn falsely; keep an unshadowed-import control that still warns.
2. Extend the collector with definition-name bindings and pattern-capture bindings, using
   the same MatchAs/MatchStar/MatchMapping AST rules already used by cone/decorator analysis.
   Continue walking definition bodies and pattern children; a value pattern is not a capture.
3. Run an actual pipeline where a local query-engine class shadows `json`, and the query
   uses an input through a dynamic string. Assert the correct result and absence of false
   warnings across match and nested-definition cases. Include a genuinely unused control.
4. Run focused binding/warning tests, CLI integration and formatting/lint checks. Document
   this as a warning correctness change; existing CLI keys and error/output contracts remain.

The shared binding-analysis consolidation in #295/#316 can reuse these proven lexical
rules later. This bounded correction does not make that broader refactor a prerequisite.
