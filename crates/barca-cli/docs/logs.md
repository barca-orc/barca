# Recorded step output

`barca logs RUN_ID` reads what a run's steps wrote, line by line: their stdout, and their
Python stderr (`logging`, `warnings`, `print(..., file=sys.stderr)` and the traceback of each
failed attempt). It loads no pipeline code and contacts no store. Find the exact id in get/run
JSON or history. Without a run id, `barca logs` reads the most recent run: the newest row of
`barca history`, running or finished. A line on stderr names it, so stdout stays the recorded
lines (or the JSON array):

```text
barca: logs of the most recent run: 07c4a64d0540 (run notify, success, started 2026-10-21 06:40:36)
```

```bash
barca logs --pretty
barca logs --step report --json
barca logs -f
barca logs 7f30b3e9 --pretty
barca logs 7f30b3e9 --step report --pretty
barca logs 7f30b3e9 --stream stderr --pretty
barca logs 7f30b3e9 --timestamps --pretty
barca logs 7f30b3e9 --step 'pipeline.py:report[k=a]' --json
barca logs 7f30b3e9 --json
barca logs 7f30b3e9 --env dev --json
```

Replace the example id with an actual run id. `--pretty` prints just the recorded lines,
with a newline after each; `--timestamps` puts each line's time before it. Blank lines and
flushed trailing partial lines are preserved. `--json` returns one object per line, in
recorded arrival order; simultaneous workers can interleave lines:

```json
[{"node_id":"pipeline.py:report","stream":"stdout","ts":"2026-10-22T10:04:31.207Z","line":"loading 3 files"},
 {"node_id":"pipeline.py:report","stream":"stderr","ts":"2026-10-22T10:04:31.208Z","line":"WARNING pipeline: 2 rows dropped"}]
```

- `stream` is `stdout` or `stderr`. `--stream stdout` or `--stream stderr` keeps one; the
  default is both, in the order they were written.
- `ts` is when the worker wrote the line: RFC 3339, UTC, with milliseconds. Lines recorded by a
  barca older than 0.29.2 have `ts: null` (and are all `stdout`). Text output leaves the time
  out unless you pass `--timestamps`, so it stays what the step printed.

The shared output rule applies: JSON when piped, text in a terminal, unless a flag or
`BARCA_OUTPUT` overrides it.

`--step` accepts a function name or exact node id. A base id selects all its partition keys;
a partitioned id selects one key. Two definitions with the same function name require an
exact id. Unknown runs, unknown/unrecorded steps and ambiguous names exit 2, and so does
`barca logs` without a run id when no run is recorded yet. A recorded execution that printed
nothing returns `[]` (JSON) or empty stdout (text). `--env` reads that environment's history.

## What is recorded

- **stdout**: everything a step prints, attributed to the step, including what its module
  prints when the step imports it.
- **stderr**: what goes through Python's `sys.stderr` while a step runs: `logging`'s default
  handler (`logging.basicConfig()`, the last-resort handler), `warnings`, `print(...,
  file=sys.stderr)`. A logging handler made while the module was imported writes to the step
  that is running when it logs. A progress bar that redraws its line with `\r` is recorded as
  the line it ended up showing.
- **The traceback of a failed attempt**: `Traceback (most recent call last):`, the frames of
  your code (barca's own are left out) and `ErrorType: message`, as `stderr` lines of the step,
  once per attempt.

Not recorded: writes straight to file descriptors 1 and 2 (native code, C extensions,
`os.write`), the output of a subprocess that inherits them, and barca's own `[barca]` lines
(except the notes barca adds to a run's log itself, with no step: why it was cancelled, and the
log cap below).
A repeated library warning that barca collapses (`[barca] N more: ...`) is recorded once. A
worker that dies without a Python exception (killed, a crash in native code) leaves no
traceback; the lines it wrote before are kept.

What you see while a run goes is unchanged: get/run reserve stdout for the command result, and
in non-JSON modes print step stdout on stderr with `[node_id]` prefixes. A step's stderr goes
to stderr as it is written, in every mode, as before.

A task's returned value remains its run's final output, independent of its prints. Lines
written before an ordinary failure are kept. A process killed before it flushed a line cannot
promise that line. Lines are kept as a run goes, so `barca logs` on a running run prints its
lines so far.

## Following a run: `barca logs -f`

`-f` (`--follow`) prints a run's lines as they arrive and exits when the run ends, with the
run's outcome as its exit code: 0 when it succeeded, 130 when it was cancelled, 1 otherwise
(failed, or interrupted because its process died). A last line on stderr says how it ended:

```text
barca: run 8d9d82c4dd20 ended: success
```

On a run that has already ended, `-f` prints its lines and exits the same way. Without a run
id, `-f` follows the newest running run; when none is running it reads the most recent run.
With `--json`, `-f` prints one JSON object per line (JSON Lines) instead of an array, so each
line can be read as it comes. `--step` and `--stream` filter as without `-f`, except that on a
run still going a `--step` name that matches nothing prints nothing rather than exiting 2: the
step may not have started yet.

## Debugging a live pipeline

Start the run in one terminal and follow it in another:

```bash
barca run report pipeline.py
barca logs -f
```

Narrow it to the step you are chasing, or to its logging and tracebacks:

```bash
barca logs -f --step report
barca logs --step report --stream stderr --timestamps
```

A failed step's traceback is the end of its stderr, so `barca logs --step report --stream
stderr` after a failure shows what it logged and where it raised.

Against a server (`barca docs server-mode`), the same commands read the server's runs: its
scheduled runs, and runs started with `--server`. With a server configured (`BARCA_SERVER` or
`[server] url`), `barca logs` asks it, after the usual `[barca] server <url> (<source>)` line;
`--server <url>` names one for a single command, and `--local` reads this machine's runs
instead. There is no fallback from one to the other, but barca points at this machine's runs:

- A run id the server does not have is exit 2. When this machine has that run, the error's
  remediation says so (with `--step` and `-f` too):

  ```text
  error: run '89a8d6599fa5' was not found on this server
  this run is on this machine: barca logs 89a8d6599fa5 --local
  ```

- Without a run id, from a configured server (not `--server <url>`), when this machine's newest
  run started after the run the server picked, one more stderr line names it; stdout (the
  lines, or the JSON) is unchanged:

  ```text
  [barca] note: this machine has a newer run 89a8d6599fa5 (run hello, success); barca logs --local reads it
  ```

  Start times are recorded to the second, so two runs started in the same second are not
  told apart.

```bash
barca logs --server -f
barca logs 7f30b3e9 --server --step report --stream stderr
barca logs 7f30b3e9 --local
```

The server answers `GET /runs/{run_id}/logs` (with `?step=` and `?stream=`), and `-f` reads
the run's `GET /runs/{run_id}/events` stream (`barca docs scheduling`). A run a laptop ran
against the server's ledger keeps its lines on the laptop: read them there with `--local`.

## How lines are kept

As a run goes, its lines are appended to a file of its own,
`.barca/live-logs/<run_id>.jsonl` (beside the metadata database of its environment); that is
what `-f`, and `barca logs` on a running run, read. When the run is recorded, its lines go into
the metadata database in one transaction and the file is deleted. A run whose process died
leaves the file, and `barca logs` reads its lines from it.

Recorded lines are kept with the run's history: `barca gc` evicts results, not history or
logs. A run records complete user lines until their text reaches or exceeds 16 MiB. The
whole crossing line is retained and followed by one `stderr` line,

```text
[barca] log cap reached: this run recorded 16 MiB of output; later lines were shown live but not recorded
```

Later user lines are not recorded, but framework notes may still be appended. This is a soft
threshold: a single line can exceed 16 MiB, and the marker names the threshold rather than
the exact final byte count. Every line is still shown live, as before.

Recording costs time in proportion to the lines: about 3 µs a line while the step runs and 6 µs
a line when the run is recorded (a step that writes 100,000 lines to stderr took 0.34 s instead
of 0.08 s, and its run 0.6 s more to record). A step that logs a few hundred lines does not
notice.

See also: `barca docs tasks`, `barca docs status`, `barca docs agents`, `barca docs
server-mode`.
