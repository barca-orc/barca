---
title: 'RFC-0014: Pipeline Fuzzing and Static Checks'
description: 'barca fuzz checks a pipeline against schemas with no real data: it proves what it can about SQL steps from the schemas alone, then runs generated frames through every step and reports the smallest input that breaks one. Schemas are declared on the decorators and never read by a run.'
---

- **Status:** Rejected (2026-10-29), before review: superseded by [RFC-0015](/rfcs/0015-pipeline-fuzzing/), which keeps the core of this design but moves schemas to SQL DDL and the engine to Rust.
- **Date:** 2026-10-29
- **Touches:** barca-core | barca-cli | python/barca
- **Superseded by:** [RFC-0015: Pipeline Fuzzing](/rfcs/0015-pipeline-fuzzing/).
- **Supersedes / Related:** the parked "schema contract on sinks" idea (DIRECTION.md, "Heard, not planned"). Builds on the serialization rules (RFC-0005), failure phases (#665) and the duckdb input views (`barca docs types`). The design was shaped by six rounds of field trials of a test candidate (`python -m barca._fuzz`) on a production project of about 360 nodes; section 9 summarises what they found.

---

## 1. Summary

A barca asset is a function of its inputs. Given a description of the data entering a
pipeline, everything below it is determined, so the pipeline can be checked before real data
reaches it. This RFC adds that check, in two layers that use the same schemas:

1. **A static check.** For a step whose result is one DuckDB query, the query is parsed, and
   which output columns can be NULL and which sets of columns are unique are inferred from the
   schemas of its inputs. The step's `output_schema` is then **proven** for every input the
   schemas allow, of any size, or reported **not proven** with the reason ("the join to `c` on
   (`group_key`, `item`) is not on a key of `c`").
2. **Fuzzing.** Frames are generated from the schemas, run through every step with barca's
   real readers and writers, and checked against each step's `output_schema` and the pipeline's
   invariants. A failure is shrunk to the fewest rows and plainest values that still fail, with
   the columns that cause it marked.

```python
@asset(output_schema=Orders)
def orders() -> duckdb.DuckDBPyRelation: ...


@asset(inputs={"orders": orders}, output_schema=Rollup)
def rollup(orders: duckdb.DuckDBPyRelation) -> duckdb.DuckDBPyRelation:
    return duckdb.sql("select region, sum(amount) as total from orders group by 1")
```

```
$ barca fuzz stage.py
static (SQL steps against their output_schema, no data; holds for every input the schemas allow):
  rollup  proven  keys (region); 2 of 2 columns never NULL

pipeline from orders: 1 finding  [100 examples, 2.1s]

finding 1: rollup, phase output_schema
  ValidationError: column 'total' failed validation for 1 rules: 'nullability' failed for 1 rows
  smallest input orders: 1 row
    * amount  null
      region  'a'
```

Schemas are declared on the decorators, are not part of the run hash, and are never read by
`barca get`, `run` or `serve`. A pipeline without schemas runs exactly as before.

## 2. Motivation

A pipeline is tested today on the data that exists. That proves it handles today's data, not
the day a source sends zero rows, a column is null for the first time, a key repeats, or a
lookup has no match. Those failures surface in production, on a schedule.

Barca is in an unusual position to check this before it happens:

- **The DAG is known statically.** A harness written by hand repeats the wiring and drifts
  from it.
- **Steps are functions of their declared inputs.** Given frames for the inputs, a step can be
  run in isolation.
- **The boundary between steps is serialization.** A class of real failures lives there and
  never appears in a unit test that passes Python objects around: a `Decimal` that turns a dict
  into pickle, a column that changes type after a null.
- **Most transformation logic is SQL.** Relational semantics are formal enough to prove
  properties of a query from the properties of its inputs: keys, nullability.

The trials (section 9) bear this out. On a stage that already had example tests, hand-written
Hypothesis tests and a runtime validation task, the static check and fuzzing each found a join
that double-counted spend and a boolean flag that could be NULL. Fuzzing alone, over four larger
stages, found eight more confirmed bugs.

## 3. Guide-Level Explanation

### 3.1 Declaring schemas

Schemas are [dataframely](https://dataframely.readthedocs.io/) schema classes:

```python
import dataframely as dy
import polars as pl


class Orders(dy.Schema):
    order_id = dy.Int64(primary_key=True)
    customer_id = dy.Int64()
    region = dy.Enum(["emea", "amer", "apac"])
    amount = dy.Float64(min=0, max=10_000, nullable=True)
    ordered_at = dy.Datetime()
    shipped_at = dy.Datetime(nullable=True)

    @dy.rule()
    def shipped_after_ordered(cls) -> pl.Expr:
        return pl.col("shipped_at") >= pl.col("ordered_at")
```

- `output_schema=` (on `@asset` and `@sensor`): what the step returns. Checked on every
  output while fuzzing; generated from when nothing upstream is.
- `input_schema={"param": Schema}` (on `@asset`): what the step assumes of one input. Used to
  generate that input when nothing upstream produces it, and as the input's description for
  the static check when the producing node has no `output_schema`.

**The main pattern is to restate what you already check.** A validation task that asserts
unique keys, closed sets of values or non-null flags on real data becomes the `output_schema`
of the table it checks. The question then becomes whether any input the upstream schemas allow
can break it.

### 3.2 Running

```bash
pip install "barca[fuzz]"
barca fuzz stage.py                   # static check, then fuzzing
barca fuzz rollup stage.py --seed 7   # only what rollup needs, reproducibly
barca fuzz stage.py --json            # results as JSON on stdout
```

Only nodes defined in the named files run. A node they import from another file is not run;
if it has an `output_schema`, frames are generated for it, so a stage can be checked on its own
from the schemas of the stages before it.

### 3.3 Reading the report

A finding names the step, the phase (`user_code`, `write_output`, `output_schema`,
`invariant`), the error and the line, and the smallest input that still fails:

```
finding 1: one_line_per_record, phase invariant
  AssertionError: 4 lines for 2 records
  smallest input records: 2 rows
    * group_key  'aaa'       'aaa'
    * item       1           1
    * finish     2025-01-01  2025-01-02
      record_id  0           1
      start      2025-01-01  2025-01-01
    (* the failure goes away when this column is made plain, or its values different)
  saved: .barca/fuzz/finding-1-one_line_per_record
```

A finding is either a bug or an assumption to write down. If `amount` is never null in
practice, say so in the schema and the finding goes away.

A check with no finding is `ok` only if the logic was exercised. It is `inconclusive` when a
step never returned rows, a column was NULL in every row of every run, a `LEFT JOIN` never
matched, or a step was stopped by the sandbox every time. Those are listed under
`never exercised`.

### 3.4 Invariants across tables

A property of two tables, which a schema cannot state, is an invariant:

```python
from barca.fuzz import conserves, invariant


@invariant(inputs={"lines": lines, "entries": entries})
def every_line_has_an_entry(lines: pl.DataFrame, entries: pl.DataFrame) -> None:
    orphans = lines.join(entries, on="entry_id", how="anti")
    assert orphans.height == 0, f"{orphans.height} lines with no entry"


units_conserved = conserves((shipments, "units"), (pnl, "units"), by=["region"])
```

`conserves` checks that a quantity's total is the same in two tables, per group. That is the
property a double-counting join breaks, and that a validator comparing two totals built from
the same inflated rows cannot see.

### 3.5 Steps the fuzzer must not run

```python
from barca.fuzz import fixed


@asset(inputs={"report": report}, fuzz=False)        # never run; publishes to the warehouse
def publish(report: pl.DataFrame) -> None: ...


@asset(fuzz=fixed({"fx": 1.1}))                       # never run; its output is this value
def rates() -> dict: ...
```

A step with `fuzz=False` and an `output_schema` still feeds what reads it, from the schema.

## 4. Reference-Level Explanation

### 4.1 Public surface

**Decorator arguments.** Accepted by the parser and listed in `decorator_args.rs`. None of
them count toward the run hash (`definition.rs`), and none is read by a run.

| Argument | On | Value |
|---|---|---|
| `output_schema=` | `@asset`, `@sensor` | a dataframely `Schema` class |
| `input_schema=` | `@asset` | `{param: Schema}`; each key must be a parameter in `inputs=` |
| `fuzz=` | `@asset`, `@sensor` | `True` (default), `False`, or `barca.fuzz.fixed(value)` |

**`barca.fuzz`**, a module that imports nothing at import time:

| Name | Purpose |
|---|---|
| `fixed(value)` | `fuzz=fixed(value)`: the step's output is `value` in every example |
| `invariant(inputs={...})` | decorator: a check over several nodes' outputs, run on every example that produced all of them; it fails by raising |
| `conserves((node, col), (node, col), by=[...], where=...)` | an invariant that two columns have the same total per group, with an optional row filter (a polars expression or a function) |

**`barca fuzz [TARGET[,TARGET...]] [FILES...]`.** Experimental. Targets and files resolve as
for `barca get` (`barca docs discovery`).

| Flag | Meaning |
|---|---|
| `--examples N` | examples generated (default 100) |
| `--seed N` | reproducible run (default random, printed) |
| `--max-rows N` | most rows in a generated frame (default 20) |
| `--static-only` | the static check alone; reads only queries written as literals |
| `--skip NODE[,NODE]` | as `fuzz=False`, for one run |
| `--allow-network` | lift the network sandbox (4.7) |
| `--allow-env NAME[,NAME]` | keep a variable the credential guard would remove |
| `--fail-inconclusive` | exit 1 when anything was never exercised |
| `--json` | results as JSON on stdout |

Exit codes follow the contract: 0 no finding; 1 a finding (or, with `--fail-inconclusive`,
logic never exercised); 2 usage, including a schema that cannot be generated, credentials the
pipeline sets on import or while a step runs, and nothing to fuzz; 3 infra; 130 cancelled.
A static "not proven" does not change the exit code: it is a reason to look, and fuzzing
decides.

**Output.** Only the report goes to stdout. What steps print, from Python or native code, goes
to stderr, so `--json` output is always valid. Each finding's smallest input is saved as
parquet under `.barca/fuzz/finding-N-<node>/`, with `finding.json`. Nothing else in `.barca/`
is read or written: no ledger rows, no artifacts, no store access.

The JSON schema (static verdicts, checks with status, findings, coverage, never exercised, not
run, stopped by the sandbox) is snapshotted in the CLI contract like any other command's.

### 4.2 The static check

**Scope.** An asset in the named files whose result is one DuckDB query. The query is taken
from the source when the step returns a literal (`return duckdb.sql("...")`, optionally with
`.df()` or `.pl()`). Otherwise it is the query the step was seen running while fuzzing: the
fuzzer records the text passed to `duckdb.sql`, `query` and `execute` (module functions and
connection methods) during each step. When the step returns the relation one of those calls
made, that query is analysed, together with the `CREATE VIEW` statements the step ran before it.
That covers SQL built in helper modules and run through wrappers. A query whose text depends on
the data is analysed for each distinct text, up to 5.

**What it infers**, for each output column, whether it can be NULL and why, and the sets of
output columns whose values are distinct (keys):

- **Keys** come from an input's primary key or `unique=True` column, `GROUP BY` (by name, by
  position, or `ALL`), `DISTINCT`, an aggregate without `GROUP BY` (one row), and a `UNION ALL`
  whose branches set a column to different literals (`'plan' AS edition`), which keeps the
  branches' keys plus that column. A join keeps the left side's keys only if it is on a key of
  the right side (each row matches at most one); otherwise those keys are lost, and the reason
  names the join and the right side's keys.
- **NULL** comes from a nullable input column, the unmatched side of an outer join, an
  aggregate over a nullable value or over no rows, a `CASE` without `ELSE`, and functions that
  can return NULL on non-NULL arguments (`TRY_CAST`, `NULLIF`, `LAG`, a scalar subquery).
  NULL is ruled out by `COALESCE` with a non-NULL argument, by a `WHERE` that rejects NULL for a
  column (a comparison, `IN`, `LIKE` or `IS NOT NULL`, directly or through functions that are
  NULL only when their argument is), inside a `CASE` branch whose `WHEN` rejects NULL for it, in
  the branches after a `WHEN x IS NULL`, and by two shapes of schema rule:
  `pl.col("b").is_not_null()`, and `(pl.col("a") != "v") | pl.col("b").is_not_null()` together
  with a `WHERE a = 'v'`.
- **Functions.** Each function the check knows is classified: strict (NULL exactly when an
  argument is NULL), never NULL (`count`, `row_number`, `coalesce` with a non-NULL argument),
  or NULL-producing. A function it does not know makes the columns computed from it **unknown**,
  not "never NULL", so an unfamiliar function cannot produce a false "proven". The candidate
  treated every unknown function as strict; the implementation of this RFC must not.

**Assume-guarantee.** Each step is checked against the contracts of its inputs. An input is
described by the `output_schema` of the node it comes from; otherwise by what was inferred for
that node; otherwise by this step's `input_schema` for it. So a broken contract is reported at
the step that breaks it, not at every step below.

**Verdicts**, against the step's `output_schema`:

| Verdict | Meaning |
|---|---|
| `proven` | every non-NULL column is inferred non-NULL, and the primary key (and each `unique` column) contains an inferred key. Holds for every input matching the input schemas, of any size |
| `not proven` | names what is not guaranteed and why. Usually a bug; possibly a guarantee that rests on something the schemas do not state, which fuzzing then confirms or not. The report links a "not proven" to the finding that confirms it |
| `unknown` | the query could not be read: no captured or literal query, a result built with relation methods (`.filter()`, `.aggregate()`) or post-processed in pandas, an unknown function, SQL outside the subset below |
| `inferred` | a SQL step without `output_schema`; reports what it guarantees, for the steps below |

**Subset read.** `SELECT` with `WITH`, subqueries in `FROM`, inner and outer joins on `ON`
equalities or `USING`, `WHERE`, `GROUP BY`, `DISTINCT`, `UNION [ALL] [BY NAME]`, aggregates,
window functions, `CASE`, `IF`, `COALESCE`. Anything else is `unknown`.

**Soundness.** "Proven" is sound for the modelled semantics: DuckDB's three-valued logic and
join semantics, the classified functions, and the installed DuckDB major version (under 1.x
`x / 0` is NULL; under 2.x it is `inf`). It is incomplete by design: correct code can be "not
proven" when the reason it is correct is not in the schemas.

**Values are not checked.** Ranges, closed sets and types are not inferred; fuzzing checks
them.

### 4.3 Generation

Frames are generated from dataframely schemas, with every random choice a Hypothesis draw: a
`dataframely.random.Generator` subclass routes `sample_int`, `sample_float`, `sample_string`
and the rest through Hypothesis. dataframely keeps its column semantics, and Hypothesis
controls every value.

- **What is generated.** In the named files: roots with an `output_schema`; nodes with
  `fuzz=False` and an `output_schema`; nodes with an `input_schema` for an input nothing upstream
  produces. Imported nodes with an `output_schema` that the named files read. `fixed()` values
  stand in as given.
- **Sample once, then filter.** Each column is sampled once, and rows the schema rejects (a
  rule, a repeated key) are dropped, instead of dataframely's loop that resamples to an exact
  row count. Under Hypothesis that loop was the bottleneck: about 17 rounds per frame, because
  Hypothesis draws small, repeated values.
- **Values repeat.** A column that does not hold distinct values draws a pool of 1 to n values
  per frame, and its rows pick from the pool. Free draws of strings and wide integers almost
  never repeat, which leaves grouping, deduplication and join-back logic untested. This was the
  single most effective change in the trials.
- **Keys are distinct.** A one-column key draws distinct values. A compound key draws distinct
  tuples from per-column pools large enough to hold them.
- **Shared keys.** A column named like another generated schema's one-column primary key, with
  a compatible type, shares its domain. For each example, one pool is drawn per domain, from its
  most constrained member (a `regex`, bounds), and keeps only values every member accepts. The
  primary key side takes a subset of the pool, and the other columns pick from it. So joins
  match, some references are orphans, and several rows can share a key. A domain that often has
  no acceptable value is reported, naming the members whose checks reject it.
- **Rows.** 0 to `--max-rows` per frame, biased toward small and boundary cases.

Columns of a schema whose values a dataframely sampler cannot produce (a `regex` with
`min_length`) are reported at the start of the run, with exit 2.

### 4.4 Execution

Steps run in one process, in topological order, through the worker's own code paths:

- input artifacts are read with the reader the parameter annotation selects;
- duckdb input views are bound by parameter name;
- the step is called through `_execute`, with its timeout;
- the result is written with barca's serializers, after the same format resolution as a run.

A dict that would be stored as pickle is a finding (`write_output`). Each table output is
validated against its `output_schema` (with casting). A step whose annotation barca does not
recognise (a type alias, none) gets a warning, since a run reads it the same way.

A step that fails stops only what depends on it; the other branches keep running, so one check
finds every bug its examples reach. Invariants run after the nodes they read. Tasks are not run.

### 4.5 Shrinking

Hypothesis generates; barca shrinks. Hypothesis's shrinker reruns the whole example per attempt
and hit its five-minute cap on every finding of a 28-column frame. Barca's shrinker:

1. reruns only the steps on the way to the failing one;
2. removes rows by halves, then quarters, down to single rows, keeping each removal that still
   fails the same way (same step, phase, exception type and line);
3. makes each column as plain as its schema allows (one value: `'a'`, 0, the first allowed
   value, the lower bound, the shortest string a regex allows; then nulls filled; then small
   distinct values; then rounded numbers);
4. marks a column `*` when making it plain, or making its values all different, loses the
   failure;
5. stops after a budget (30 s per finding) and says so.

### 4.6 Coverage and verdicts

Per check: `findings` if any; else `inconclusive` if any node in it is listed under
`never exercised`; else `ok`. A node is never exercised when, in the examples:

- it ran at least 10 times without failing and never returned a row (naming an input that never
  had rows, if one never did);
- a column of its output was NULL in every row of at least 10 outputs with rows;
- a `LEFT JOIN` of its query never matched in at least 10 runs whose left side had rows. For
  each step whose query was read (4.2), a count of matched rows per `LEFT JOIN` is run on the
  same bound inputs, so a join hidden behind `coalesce(k.key, l.id || '@plan')` is caught;
- it was stopped by the sandbox in every run.

Hypothesis is steered toward examples in which more steps return rows and more joins match,
and `conserves` steers toward larger gaps. Neither needs configuration.

The coverage table reports per node: the share of table inputs with rows, the share of runs
returning rows, failures, sandbox stops, and time per run.

### 4.7 The sandbox

The fuzzer runs the pipeline's code. Before any step runs:

- **Credentials** are removed from the environment: names barca redacts (`*_TOKEN`, `*_SECRET`,
  `*_KEY`, `*_PASSWORD`), connection strings, and the cloud SDKs' own names. `--allow-env` keeps
  named ones. The run names what it removed, and a finding notes when its step read one.
- **Writes to the environment are watched** through Python's audit hook (`os.putenv`). A
  credential set while the pipeline is imported stops the run with exit 2, naming the line that
  set it and the file being imported. One set by a step while it runs is refused at that line,
  and the run stops.
- **Network.** Every Python-level IP connection and name lookup is refused. In duckdb, the
  remote filesystems (`httpfs`, `azure`) are disabled and extensions are not downloaded, on the
  default connection and on every connection opened after the fuzzer starts. A step that tries
  is listed under `stopped by the sandbox` and is not a finding.
- **Not covered:** other native code that opens sockets, and local files written to absolute
  paths. The manual tells users to mark every step that reads or writes storage `fuzz=False`.

### 4.8 Rust ↔ Python boundary

`barca fuzz` is a Rust subcommand. It:

1. resolves files and targets as `get` does;
2. parses them, rejecting unknown decorator arguments with exit 2, as today;
3. writes the planned graph as JSON: node ids, kinds, inputs, source files and function names,
   partitioned or unsupported nodes;
4. starts `python -m barca._fuzz` with the graph and the flags;
5. passes its exit code through.

The Python side imports the named files, as a worker does, and takes schemas and `fuzz=` values
from the decorators. The `barca` stubs record their arguments on the function, which remains
the same object. The node graph comes from Rust, so node ids, `name=`, imports and
`asset_ref` resolve exactly as in a run.

Planning still imports nothing: `output_schema=` and the rest are read as text by the parser,
and only the fuzzer evaluates them, in its own process. Python keeps no database access; the
fuzzer reads no ledger.

### 4.9 Node kinds and unsupported nodes

| Kind | In a fuzz run |
|---|---|
| asset | runs; generated from its `output_schema` when it is a root or `fuzz=False` |
| sensor | generated from its `output_schema` (its change signal is ignored) |
| task | not run |
| invariant | runs after the nodes it reads; not a node of the DAG, never run by `get` |

Partitioned nodes, `asset_ref` inputs and what depends on them are not run in this version, and
are listed under `not run` with the reason, as is every node in the named files that no check
ran.

## 5. Determinism, Caching & Testing

- **Run hash:** schemas and `fuzz=` are not part of it; adding or editing them recomputes
  nothing. A test checks a `get` before and after adding a schema: same run hash, cached.
- **Determinism:** `--seed` makes a run reproducible, including shrinking. Findings are
  deduplicated by step, phase, exception type and line.
- **Tests:**
  - static inference rules, one per row of 4.2, including the cases that must stay "not
    proven" (a non-key join, an unguarded `CASE`, a rule whose `WHERE` does not match);
  - generation properties (pools non-empty under constraints, compound keys honouring row
    counts, shared keys matching);
  - the sandbox (credentials at import and during a step, network, duckdb remote reads);
  - end-to-end pipelines with planted bugs: a join fan-out, a nullable flag, a division by zero
    on empty input, a numpy value in a dict.
- **Contract:** `barca fuzz --help` and its JSON schema are snapshotted, and the command and
  its flags are listed as experimental.

## 6. Performance

Nothing changes for runs: the parser reads three more decorator arguments as text, and the stubs
record their arguments on the function. The static check takes milliseconds per step.
Fuzzing in the trials ran 8–20 examples per second on DuckDB stages, so the default 100
examples take about 10 s, which fits a pre-push hook. The slowest single step bounds it:
`fuzz=False` keeps a step that takes seconds per run out.

## 7. Drawbacks

- **Schemas are work.** Each finding is a bug or an assumption to write down, and the first run
  on a real stage reports some of both. The trials' cost was 45–55 minutes of agent time per
  large stage, mostly writing schemas.
- **Fuzzing runs user code.** The sandbox covers credentials, Python networking and duckdb, not
  every native library or local file writes. `fuzz=False` is the remedy, and the manual says so
  first.
- **The static check is incomplete.** Correct code can be "not proven", and the SQL subset will
  need to grow. Each gap shows up as "unknown" or as a false "not proven", never as a false
  "proven".
- **Frames are generated independently** apart from shared keys. Logic gated on conditions
  across many rows or tables is reached rarely. A fuzz-only harness asset that derives one
  generated frame from another is the documented remedy.
- **A dependency:** `barca[fuzz]` adds dataframely, Hypothesis, polars and sqlglot.

## 8. Rationale & Alternatives

- **Checks on real runs.** Validating outputs against schemas in `get` and `serve` was the
  first design. It was dropped: it costs every run a validation pass, and the trials showed the
  value is in what the schemas say about inputs that have not happened yet. A read-only
  `barca check` of cached results against their schemas remains possible later.
- **A schema language of barca's own.** dataframely already has typed columns, rules, primary
  keys and samplers; a barca DSL would be one more language to learn and maintain. pandera was
  considered; its data synthesis is pandas-only and its validation of a duckdb relation
  materialises it.
- **Hypothesis's shrinker.** Kept for generation, replaced for shrinking (4.5).
- **Fuzzing alone, without the static check.** Fuzzing found the same two bugs in the trial.
  The static check is kept because it proves what fuzzing can only sample: a join that is on a
  key holds for every input, including the large ones fuzzing never generates. It also gives a
  reason in relational terms, and it costs milliseconds.
- **SMT-based bounded checking** of SQL (as in Cosette or VeriEQL) is stronger than inference
  over keys and nullability, but its encoding of SQL with aggregates, floats and string functions
  is much larger work. Left for later.
- **Generation controls.** The candidate added `derive()`, `rows()`, `generate()`,
  `check_only()`, `key(transform=)`, `reached()`, a step memo, and `edges` and `nodes` checks.
  None of them found a bug that the core plus a harness asset could not. They are left out.

## 9. Prior Art and Evidence

- **Hypothesis / QuickCheck:** property-based testing with shrinking; barca uses Hypothesis
  for generation.
- **dataframely:** schemas with samplers; barca generates through them.
- **Dagster asset checks, dbt tests, Great Expectations:** checks on real data. None generate
  inputs or prove properties of transformations.
- **Functional dependency and key inference** over relational algebra: textbook (Armstrong's
  axioms); used by query optimisers for join elimination.

Field trials on a production barca project (about 360 nodes), six rounds:

| Finding | Found by |
|---|---|
| A join back on part of the key, double-counting spend (HIGH) | static check and fuzzing; earlier only by a hand-written harness |
| A boolean output NULL for an unmapped upstream value | static check and fuzzing |
| 2 double-counting joins, 3 NULL/NaN propagations, 2 unit errors, 1 dropped-rows filter, in four larger stages | fuzzing, through output schemas and invariants |

What the trials changed in the design:

- value pools, since without them the fan-out was missed;
- barca's shrinker, which cut time to the first report from about 5.5 min to 18 s;
- `*` cause columns and one column per line in repros;
- `inconclusive`, since a check whose lookup join never matched had reported `ok` over 200
  examples;
- the sandbox, since a default setup would have run steps with real storage credentials;
- capturing executed SQL, since every real stage built its SQL in helpers;
- positional `GROUP BY`, `UNION BY NAME`, guarded `CASE` and schema rules in the static check,
  without which the fixed join could not be told from the buggy one.

## 10. Unresolved Questions

1. **The graph from Rust (4.8)** is the intended boundary; the candidate rebuilt it in Python
   from the decorators. Confirm before implementation.
2. **Exit code for a static "not proven".** The draft keeps it 0. A `--fail-unproven` flag
   could make it 1 for teams that want the proof enforced.
3. **More rule shapes** for the static check (`is_in`, `when/then/otherwise`), or a declared
   form instead of reading polars expressions.
4. **Partitioned nodes.** Generating keys from the declared domain and passing a batch of drawn
   keys is the natural extension; not in this version.

## 11. Future Possibilities

- `barca check`: validate cached results against their schemas, read-only, on request.
- SMT-based bounded verification of invariants on SQL steps.
- The generation controls listed in section 8, each when a trial shows a bug only it reaches.
- Running examples in parallel processes.
