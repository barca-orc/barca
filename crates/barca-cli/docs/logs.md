# Recorded step stdout

`barca logs RUN_ID` reads the captured stdout of an existing local run without loading
pipeline code or contacting a store/server. Find the exact id in get/run JSON or history.
Without a run id, `barca logs` reads the most recent run: the newest row of `barca history`,
running or finished. A line on stderr names it, so stdout stays the recorded lines (or the
JSON array):

```text
barca: logs of the most recent run: 07c4a64d0540 (run notify, success, started 2026-10-21 06:40:36)
```

```bash
barca logs --pretty
barca logs --step report --json
barca logs 7f30b3e9 --pretty
barca logs 7f30b3e9 --step report --pretty
barca logs 7f30b3e9 --step 'pipeline.py:report[k=a]' --json
barca logs 7f30b3e9 --json
barca logs 7f30b3e9 --env dev --json
```

Replace the example id with an actual run id. `--pretty` prints just the recorded lines,
with a newline after each. Blank lines and flushed trailing partial lines are preserved.
`--json` returns `[{"node_id":"pipeline.py:report","line":"loading 3 files"}]`, in
recorded arrival order; simultaneous workers can interleave lines. The shared output
rule applies: JSON when piped, text in a terminal, unless a flag or `BARCA_OUTPUT` overrides it.

`--step` accepts a function name or exact node id. A base id selects all its partition keys;
a partitioned id selects one key. Two definitions with the same function name require an
exact id. Unknown runs, unknown/unrecorded steps and ambiguous names exit 2, and so does
`barca logs` without a run id when no run is recorded yet. A recorded
execution that printed nothing returns `[]` (JSON) or empty stdout (text).
`--env` reads that environment's local metadata. There is no implicit remote fetch.

Get/run always reserve stdout for the command result. Non-JSON modes render step stdout
on stderr with `[node_id]` prefixes; framework `--agent` lines retain `[barca]` prefixes.
JSON output suppresses the captured lines, which remain recorded. Module-import stdout
is attributed to the step importing the module. Explicit writes to user stderr are not
captured in this stdout log. Native descriptor writes and inherited subprocess output
continue through the child stream on stderr; they are outside the Python stdout capture.

A task's returned value remains its run's final output, independent of its prints.
Lines flushed before ordinary failure are retained. A process killed before emitting
or flushing a line cannot promise that line. Logs are published at run completion;
this command does not tail an in-flight worker, so the most recent run returns no lines
while it is still running (the stderr line says `running`); pass the previous run's id. Writes of one run's log stream are atomic.

See also: `barca docs tasks`, `barca docs status`, `barca docs agents`.
