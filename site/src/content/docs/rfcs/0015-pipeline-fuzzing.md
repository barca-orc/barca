---
title: 'RFC-0015: Pipeline Fuzzing'
description: 'barca fuzz checks a pipeline before real data reaches it. Assets declare the tables they produce in SQL DDL, inline or in schema files. Barca proves what it can about SQL steps from those schemas alone, then runs generated data through every step and reports the smallest input that breaks one. Schemas are never read by a run.'
---

- **Status:** Under Review
- **Date:** 2026-10-29
- **Touches:** barca-core | barca-cli | python/barca
- **Supersedes / Related:** supersedes [RFC-0014](/rfcs/0014-pipeline-fuzzing/) (rejected before review). Resolves the parked "schema contract on sinks" idea (DIRECTION.md, "Heard, not planned"). Builds on the serialization rules (RFC-0005), failure phases (#665) and the worker's duckdb input views (`barca docs types`). Shaped by six rounds of field trials of a Python prototype on a production project of about 360 nodes (section 9).

---

## 1. Summary

A barca asset is a function of its inputs. Given a description of the data entering a pipeline,
everything below it is determined, so the pipeline can be checked before real data arrives.
This RFC adds that check:

```python
@asset(output_schema="""
    order_id    BIGINT PRIMARY KEY,
    customer_id BIGINT NOT NULL REFERENCES customers (customer_id),
    region      VARCHAR NOT NULL CHECK (region IN ('emea', 'amer', 'apac')),
    amount      DOUBLE CHECK (amount BETWEEN 0 AND 10000)
""")
def orders() -> duckdb.DuckDBPyRelation: ...
```

```bash
barca fuzz stage.py
```

`barca fuzz` does two things with the same schemas:

1. **It proves.** For a step whose result is a DuckDB query, it infers from the input schemas
   which output columns can be NULL and which column sets are unique, and reports the step's
   `output_schema` **proven** for every possible input, or **not proven** with the reason ("the
   join to `e` on (`group_key`, `item`) is not on a key of `e`").
2. **It tests.** It generates data from the schemas, runs every step on it through the worker's
   own execution path, checks each output against its schema and the pipeline's invariants, and
   shrinks each failure to the smallest input that still fails, with the columns that cause it
   marked.

Schemas are SQL DDL, written inline next to the asset or in `.sql` files. They are declarations:
not part of the run hash, never read by `barca get`, `run` or `serve`. A project that declares
none is unaffected.

Rust parses the schemas and the queries, proves, generates, drives the examples and shrinks.
Python runs the steps, as it always has.

## 2. Motivation

A pipeline is tested today on the data that exists. That proves it handles today's data, not
the day a source sends zero rows, a column is null for the first time, a key repeats or a
lookup has no match. Those failures surface in production, on a schedule.

Barca can check this ahead of time, where general-purpose orchestrators cannot:

- **The DAG is known without running anything.** Barca reads it from source; a test harness
  written by hand repeats the wiring and drifts from it.
- **Steps are functions of declared inputs.** Given data for its inputs, any step can run in
  isolation.
- **Data crosses steps as serialized artifacts.** A class of real failures lives at that
  boundary and never shows in a unit test that passes Python objects around.
- **Most transformation logic is SQL.** Relational semantics are formal enough to prove keys and
  nullability of a query's result from those of its inputs.

In the trials, on a stage that already had example tests, hand-written Hypothesis tests and a
runtime validation task, both the static check and fuzzing found a join that double-counted
spend and a boolean flag that could be NULL. Over four larger stages, fuzzing found eight more
confirmed bugs (section 9).

## 3. Guide-Level Explanation

### 3.1 Declaring schemas

An asset declares the table it produces with `output_schema=`, in one of two forms.

**Inline**, a column list next to the asset. The table is named after the asset:

```python
@asset(output_schema="""
    order_id    BIGINT PRIMARY KEY,
    customer_id BIGINT NOT NULL REFERENCES customers (customer_id),
    amount      DOUBLE CHECK (amount BETWEEN 0 AND 10000),
    ordered_at  TIMESTAMP NOT NULL,
    shipped_at  TIMESTAMP,
    CHECK (shipped_at >= ordered_at)
""")
def orders() -> duckdb.DuckDBPyRelation: ...
```

**A table name**, defined in a schema file:

```python
@asset(output_schema="customers")
def customers() -> duckdb.DuckDBPyRelation: ...
```

```sql
-- schemas/customers.sql
CREATE TABLE customers (
    customer_id BIGINT PRIMARY KEY,
    segment     VARCHAR NOT NULL CHECK (segment IN ('retail', 'wholesale'))
);
```

Inline suits a schema one asset uses; a file suits a schema shared across stages or long enough
to crowd the decorator. Both live in one namespace, so a `REFERENCES` in either form can point at
a table in the other.

What a schema says, and what barca does with it:

| DDL | Meaning |
|---|---|
| column type, `NOT NULL` | type and nullability |
| `PRIMARY KEY`, `UNIQUE` (one column or several) | the values are distinct |
| `REFERENCES t (col)` | values come from `t.col`: generated data shares keys, so joins match |
| `CHECK (...)` | any SQL condition on a row: ranges, closed sets, patterns, relations between columns |

A schema may list only some of the columns; the others are allowed through unchecked.

**The most useful schemas restate checks the project already makes.** A validation task that
asserts unique keys, closed sets of values or non-null flags on real data becomes the
`output_schema` of the table it checks. The question then becomes whether any input the upstream
schemas allow can break it.

### 3.2 Starting from real data

Writing a 28-column schema by hand was most of the time spent in the trials. Barca drafts one
from the asset's latest cached result:

```
$ barca fuzz stage.py --draft-schema orders
-- drafted from orders' result of 2026-10-28 (12,408 rows); review before use
    order_id    BIGINT PRIMARY KEY,           -- distinct in this result
    customer_id BIGINT NOT NULL,
    region      VARCHAR NOT NULL CHECK (region IN ('amer', 'apac', 'emea')),
    amount      DOUBLE,                       -- 31 NULLs
    ordered_at  TIMESTAMP NOT NULL,
    shipped_at  TIMESTAMP
```

The draft describes one result, not every possible one, so it is printed for review rather than
written anywhere.

### 3.3 Running

```bash
barca fuzz stage.py                   # proves, then tests
barca fuzz rollup stage.py --seed 7   # only what rollup needs, reproducibly
barca fuzz stage.py --static-only     # proves only: no Python, no data, milliseconds
```

Only nodes defined in the named files run. A node they import from another file never runs; if
it has an `output_schema`, data is generated for it, so a stage can be checked on its own from the
schemas of the stages before it.

### 3.4 Reading the report

```
$ barca fuzz stage.py
static (holds for every input the schemas allow):
  entries   proven      keys (group_key, item, start, finish); 6 of 6 columns never NULL
  lines     not proven  rows can repeat on (record_id): the join to e on (group_key, item)
                        is not on a key of e (its keys: group_key, item, start, finish)
  rollup    not proven  online can be NULL: bool_or(channel = 'web') is NULL when channel is
  details   unknown     post-processes its query in pandas; fuzzing covers it

fuzz: 100 examples, 3.2s: 2 findings

finding 1: lines, output_schema (confirms the static result)
  rows repeat on (record_id): 4 lines for 2 records
  smallest input records: 2 rows
    * group_key  'a'         'a'
    * item       1           1
    * finish     2025-01-01  2025-01-02
      record_id  0           1
  at stage.py:41
  replay: barca fuzz stage.py --replay 1

finding 2: rollup, output_schema (confirms the static result)
  online is NULL in 1 row
  smallest input records: 1 row
    * channel  null
  replay: barca fuzz stage.py --replay 2

seed 4114
```

- **Static** says what holds for every input: **proven**, **not proven** with the reason,
  **unknown** when the step's result is not a query it can read.
- **Findings** give the step, what failed, the smallest input, the columns that matter (`*`),
  and a command that reruns exactly that input.
- A finding is a bug or an assumption to write down. If `channel` is never NULL in practice,
  `NOT NULL` in the schema says so, and the finding goes away.

### 3.5 The fix loop

```bash
barca fuzz stage.py --replay 1     # the failing input again, after a fix: instant
barca fuzz stage.py                # everything again
```

```
static:
  lines     proven      keys (record_id)
  rollup    proven      keys (group_key); 3 of 3 columns never NULL
fuzz: 100 examples, 3.1s: ok
```

**Proven** after a fix means the join is right for every input, not only the 100 that were
tried.

### 3.6 When nothing failed, but nothing was tested either

A run is `ok` only if the logic ran. Otherwise it is `inconclusive`, with what never ran:

```
fuzz: 100 examples: inconclusive
  never reached:
    pnl  the LEFT JOIN to lookup on (promo_key) never matched (94 runs)
         nothing makes lookup.promo_key meet plan.promo_id: add a REFERENCES
```

### 3.7 Invariants across tables

A property of two tables, which a schema cannot state, is an invariant:

```python
from barca.fuzz import conserves, invariant


@invariant(inputs={"lines": lines, "entries": entries})
def every_line_has_an_entry(lines: pl.DataFrame, entries: pl.DataFrame) -> None:
    orphans = lines.join(entries, on="entry_id", how="anti")
    assert orphans.height == 0, f"{orphans.height} lines with no entry"


units_conserved = conserves((shipments, "units"), (pnl, "units"), by=["region"])
```

`conserves` checks that a quantity's total is the same in two tables, per group: the property a
double-counting join breaks, and that a validator comparing two totals built from the same
inflated rows cannot see.

### 3.8 Steps that must not run

```python
from barca.fuzz import fixed


@asset(inputs={"report": report}, fuzz=False)    # publishes: never run while fuzzing
def publish(report: pl.DataFrame) -> None:
    write_to_warehouse(report)


@asset(fuzz=fixed({"fx": 1.1}))                   # not a table: this value stands in
def rates() -> dict:
    return load_rates()
```

A step with `fuzz=False` and an `output_schema` still feeds what reads it, from its schema.

### 3.9 Safety

Fuzzing runs the pipeline's code. While it does, credentials are removed from the environment,
network connections are refused, and duckdb's remote filesystems are disabled. A step that tries
to reach storage is reported as stopped, not as a finding:

```
fuzz: removed 3 credential variables (STORE_ACCOUNT_KEY, ...); --allow-env NAME keeps one
  export  stopped: tried to connect to storage.example.com:443 (mark it fuzz=False)
```

### 3.10 In CI and for agents

```bash
barca fuzz stage.py --seed 1 --fail-inconclusive        # a pre-push gate, about 10 s
barca fuzz stage.py --json | jq '.findings[].node_id'   # JSON on stdout, progress on stderr
```

## 4. Reference-Level Explanation

### 4.1 Public surface

**Decorator arguments.** Listed in `decorator_args.rs`; not part of the run hash
(`definition.rs`); never read by a run.

| Argument | On | Value |
|---|---|---|
| `output_schema=` | `@asset`, `@sensor` | a string: a table name, or an inline column list (4.2) |
| `fuzz=` | `@asset`, `@sensor` | `True` (default), `False`, or `barca.fuzz.fixed(value)` |

The value is a string literal or a module-level constant assigned one, read statically like
any decorator argument. Anything computed is exit 2.

**`barca.fuzz`.** A module with no imports at import time.

| Name | Purpose |
|---|---|
| `fixed(value)` | `fuzz=fixed(value)`: the step's output in every example |
| `invariant(inputs={...})` | decorator: a check over several nodes' outputs, run in every example that produced them all; it fails by raising |
| `conserves((node, col), (node, col), by=[...], where=...)` | an invariant that two columns have equal totals per group; `where` filters rows (a polars expression or a function) |

**`barca fuzz [TARGET[,TARGET...]] [FILES...]`.** Experimental. Targets and files resolve as
for `barca get`.

| Flag | Meaning |
|---|---|
| `--examples N` | examples generated (default 100) |
| `--seed N` | a reproducible run (default random, printed) |
| `--max-rows N` | most rows in a generated table (default 20) |
| `--max-keys N` | most keys of a partition dimension per example (default 3; 4.12) |
| `--static-only` | the static check alone, on queries written as literals |
| `--replay N` | rerun finding N's saved input |
| `--draft-schema NODE` | print a schema drafted from the node's latest cached result |
| `--skip NODE[,NODE]` | as `fuzz=False`, for one run |
| `--allow-network` | lift the network sandbox |
| `--allow-env NAME[,NAME]` | keep a variable the credential guard would remove |
| `--fail-inconclusive` | exit 1 when anything was never reached |
| `--json` | results as JSON on stdout |

**Exit codes.**

| Code | Meaning |
|---|---|
| 0 | no finding |
| 1 | a finding; or, with `--fail-inconclusive`, logic never reached |
| 2 | usage: a malformed or unresolved schema, a schema that cannot be generated, credentials the pipeline sets on import or in a step, nothing to fuzz |
| 3 | infra |
| 130 | cancelled |

A static "not proven" does not change the exit code. It is a reason to look, and fuzzing decides.

**Configuration.** `[schemas] paths = ["schemas"]` in `barca.toml`: the directories searched
for schema files, recursively (default `schemas/` at the project root).

**Output.**

- **stdout** carries only the report. What steps print, from Python or native code, goes to
  stderr, so `--json` is always valid.
- **`.barca/fuzz/finding-N/`** holds each finding's smallest input, as parquet, with
  `finding.json`, for `--replay`.
- **Nothing else** in `.barca/` is read or written: no ledger rows, no artifacts, no store
  access. `--draft-schema` reads the ledger to find the node's latest result.
- **The JSON output** is snapshotted in the CLI contract like any command's: static verdicts,
  findings, check status, never reached, not run, stopped by the sandbox, coverage.

### 4.2 Schemas

**Files.** `*.sql` files under the schema paths. They may contain only `CREATE TABLE`
statements, one or more per file; any other statement is exit 2, so a query file cannot silently
define nothing. Table names may be qualified (`retail.customers`).

**Inline.** The body of a `CREATE TABLE`, without `CREATE TABLE name (...)`. The table takes the
asset's node name. A string that is a bare (optionally qualified) identifier is a reference to a
table in the files; any other string is inline.

**Namespace.** File tables and inline schemas share one namespace. A name defined twice is exit
2, naming both places; an unknown name is exit 2 with the closest names.

**What is read.**

- **Column types:** DuckDB's scalar types.
- **Column constraints:** `NOT NULL`, `PRIMARY KEY`, `UNIQUE`, `REFERENCES`, `CHECK`.
- **Table constraints:** compound `PRIMARY KEY`, `UNIQUE`, `FOREIGN KEY ... REFERENCES`,
  `CHECK`.
- `DEFAULT`, collations, generated columns and nested types are rejected for now (exit 2).

**Semantics are SQL's.** A `CHECK` holds when its condition is true or NULL, so
`CHECK (amount >= 0)` allows NULL; only `NOT NULL` forbids it. `REFERENCES` names where a
column's values come from. It is used to generate data and by the static check, not enforced on
outputs.

**Who reads them.** Rust: parsing (`sqlparser`, DuckDB dialect), resolution and validation
happen when the files are planned, before anything runs. The worker receives a normalised
description (columns, types, nullability, keys, references, and each `CHECK` as SQL text).

### 4.3 The static check

**Scope.** An asset in the named files whose result is one DuckDB query. The query comes from
the source when the step returns a literal (`return duckdb.sql("...")`, optionally with `.df()`
or `.pl()`). Otherwise it is the query the step was seen running while fuzzing:

- the worker records the text passed to `duckdb.sql`, `query` and `execute`, module functions
  and connection methods;
- when the step returns a relation one of them made, it reports that query, and the
  `CREATE VIEW` statements run before it, with the step's result;
- a query whose text depends on the data is checked for each distinct text, up to 5.

This reaches SQL built in helper modules and run through wrappers. `--static-only` reads
literals only.

**What it infers.** For each output column, whether it can be NULL and why; and the sets of
output columns whose values are distinct.

- **Keys** come from:
  - an input's `PRIMARY KEY` or `UNIQUE`;
  - `GROUP BY`, by name, by position or `ALL`;
  - `DISTINCT`;
  - an aggregate without `GROUP BY` (one row);
  - a `UNION ALL [BY NAME]` whose branches set a column to different literals, which keeps the
    branches' keys plus that column.

  A join keeps the left side's keys only if it is on a key of the right side, so each row
  matches at most one. Otherwise they are lost, and the reason names the join and the right
  side's keys.
- **NULL** comes from:
  - a nullable input column;
  - the unmatched side of an outer join;
  - an aggregate over a nullable value, or over no rows;
  - a `CASE` without `ELSE`;
  - a function that can return NULL on non-NULL arguments.
- **NULL is ruled out** by:
  - `COALESCE` with a non-NULL argument;
  - a `WHERE` that rejects NULL for a column: a comparison, `IN`, `LIKE` or `IS NOT NULL`,
    directly or through strict functions;
  - inside a `CASE` branch whose `WHEN` rejects it, and in the branches after a `WHEN x IS NULL`;
  - an input `CHECK` of the form `a <> v OR b IS NOT NULL` under a `WHERE a = v`.

**Functions.** A table classifies each known function as strict (NULL exactly when an argument
is), never NULL, or NULL-producing. A function not in the table makes the columns computed from
it unknown. So an unfamiliar function can cost a "proven", never produce a false one.

**Assume-guarantee.** Each step is checked against its inputs' contracts:

- an input is described by the `output_schema` of the node it comes from;
- otherwise by what was inferred for that node;
- otherwise it is unknown.

A broken contract is reported where it breaks, not at every step below.

**Verdicts**, against the step's `output_schema`:

| Verdict | Meaning |
|---|---|
| `proven` | each `NOT NULL` column is inferred non-NULL, and each `PRIMARY KEY` and `UNIQUE` contains an inferred key. Holds for every input matching the input schemas, of any size |
| `not proven` | what is not guaranteed, and why. Usually a bug; possibly a guarantee resting on something the schemas do not say, which fuzzing confirms or not. The report links each to the finding that confirms it |
| `unknown` | the query could not be read: a result built with relation methods or post-processed in pandas, an unknown function, or SQL outside the subset |
| `inferred` | a SQL step without `output_schema`: what it guarantees, for the steps below |

**Subset.** `SELECT` with `WITH`, subqueries in `FROM`, inner and outer joins on `ON`
equalities or `USING`, `WHERE`, `GROUP BY`, `DISTINCT`, `UNION [ALL] [BY NAME]`, aggregates,
window functions, `CASE`, `IF`, `COALESCE`. Anything else is unknown.

**Soundness.** "Proven" is sound for the modelled semantics: SQL's three-valued logic and join
semantics, the function table, and the installed duckdb major version (under 1.x `x / 0` is NULL,
under 2.x `inf`). It is incomplete by design: correct code can be "not proven" when the reason
it is correct is not in the schemas. Values (ranges, closed sets, types) are not inferred;
fuzzing checks them.

### 4.4 Generation

Rust generates each example's tables from the schema descriptions, with `proptest` strategies:

- **What is generated:**
  - roots in the named files that have an `output_schema`;
  - nodes with `fuzz=False` and an `output_schema`;
  - imported nodes with an `output_schema` that the named files read.

  `fixed()` values stand in as given.
- **Columns.** A type gives the generator. `NOT NULL` removes NULL. `CHECK`s of a simple shape
  narrow it directly:
  - bounds: `BETWEEN`, `>=`, `<`, ...;
  - closed sets: `IN (...)`;
  - patterns: `regexp_full_match`, `LIKE`;
  - lengths.

  Rows that break any other `CHECK` are dropped by the worker, which evaluates the conditions in
  DuckDB before the step runs.
- **Values repeat.** A column whose values need not be distinct draws a pool of 1 to n values
  per table, and its rows pick from the pool. Free draws of strings and wide integers almost
  never repeat, which leaves grouping, deduplication and join-back logic untested. This was the
  most effective single change in the trials.
- **Keys are distinct.** A `PRIMARY KEY` or `UNIQUE` draws distinct values. A compound one draws
  distinct tuples from per-column pools large enough to hold them.
- **References share values.** For each example, a pool is drawn for each referenced column,
  from the most constrained of the columns that share it, keeping values every one of them
  accepts. The referenced key takes a subset of the pool and the referencing columns pick from
  it. So joins match, some references are orphans, and several rows can share a key.
- **Rows:** 0 to `--max-rows` per table, biased toward empty, one row and small tables.

A schema whose columns admit no value (contradictory `CHECK`s) is exit 2 at the start, naming
the column.

### 4.5 Execution

Rust drives each example. The worker runs it, through the same step execution a run uses: a
single function shared by the run loop and fuzz mode.

1. **Inputs.** The worker writes the generated tables as parquet with the declared types, drops
   the rows that break a `CHECK`, and writes each `fixed()` value with barca's serializers.
2. **Steps run in topological order.** Each one: inputs are read with the reader its annotation
   selects, duckdb input views are bound, the step is called with its timeout, and the result is
   written after the same format resolution as a run.
3. **Each table output is checked against its `output_schema`, in DuckDB:**
   - columns present and castable;
   - `NOT NULL`;
   - duplicates on `PRIMARY KEY` and `UNIQUE`;
   - each `CHECK`.

   A dict that would be stored as pickle is a finding too.
4. **A failure stops only what depends on it;** the other branches keep running, so one example
   can find several bugs. Invariants run after the nodes they read. Tasks are not run.
5. **Joins are measured.** For each step whose query was read, Rust derives a count query per
   `LEFT JOIN` (the left side's rows, and those that matched). The worker runs it on the same
   input artifacts.

A parameter whose annotation barca does not recognise gets a warning, since a run reads it the
same way.

### 4.6 Shrinking

Rust shrinks each finding:

1. Rerun only the steps on the way to the failing one.
2. Remove rows: halves, then quarters, down to single rows. Keep each removal after which the
   input still matches its schema and the run fails the same way (same step, phase, exception
   type and line).
3. Make each column as plain as its schema allows, keeping each change that still fails: one
   value (`'a'`, 0, the lower bound, the first value of an `IN` list, the shortest string a
   pattern allows); then NULLs filled; then small distinct values; then rounded numbers.
4. Mark a column `*` when making it plain, or making its values all different, loses the
   failure.
5. Stop after 30 s per finding, and say so.

The result is saved under `.barca/fuzz/finding-N/` for `--replay`.

### 4.7 Verdicts and coverage

Borrowing the distinction from simulation testing: a schema or invariant must hold **always**,
and each step's logic must run **sometimes**. A run reports, per example set, one of:

- `findings`: something that must always hold failed.
- `inconclusive`: nothing failed, but something never ran.
- `ok`: neither.

"Never ran" means, over the examples:

- a step ran at least 10 times without failing and never returned a row (naming an input that
  never had rows, if one never did);
- an output column was NULL in every row of at least 10 outputs with rows;
- a `LEFT JOIN` never matched in at least 10 runs whose left side had rows;
- a step was stopped by the sandbox every time;
- a reference pool had no value every sharing column accepts, in at least a fifth of the
  examples.

The report names each, with the likely cause where barca can tell. The coverage table gives, per
step:

- the share of inputs with rows;
- the share of runs that returned rows;
- failures and sandbox stops;
- time per run.

### 4.8 The sandbox

In fuzz mode, before any user code runs:

- **Credentials.** Rust starts the worker with an environment from which credential-like
  variables are removed:
  - names barca redacts (`*_TOKEN`, `*_SECRET`, `*_KEY`, `*_PASSWORD`);
  - connection strings;
  - cloud SDK names.

  `--allow-env` keeps named ones. A finding notes when its step read a removed variable.
- **Writes to the environment** are watched in the worker through Python's audit hook:
  - a credential set while the pipeline is imported is exit 2, naming the line that set it and
    the file being imported;
  - one set by a step while it runs is refused at that line, and the run stops with exit 2.
- **Network.** Every Python-level IP connection and name lookup is refused. In duckdb, the
  remote filesystems (`httpfs`, `azure`) are disabled and extensions are not downloaded, on the
  default connection and on every connection opened later. A step that tries is reported as
  stopped, not as a finding.
- **Not covered:** other native code opening sockets, and local files written to absolute
  paths. The manual says, first, to mark every step that reads or writes storage `fuzz=False`.

### 4.9 Drafting schemas

`--draft-schema NODE` asks the worker to describe the node's latest cached result in DuckDB:

- the column types;
- columns with no NULLs, drafted as `NOT NULL`;
- columns whose values are all distinct, drafted as key candidates;
- string columns with at most 20 distinct values, drafted as `CHECK (... IN (...))`.

It prints the result as an inline column list. It is a draft of one result, marked as such, and
never written to a file.

### 4.10 Rust ↔ Python boundary

`barca fuzz` plans like `get`: it resolves files and targets, parses the decorators and
schemas, and rejects errors before starting a worker. Fuzz mode adds worker messages:

| Message | Worker does |
|---|---|
| start (fuzz mode) | install the sandbox, import the named files |
| write inputs | write generated tables as parquet with declared types, dropping rows that break a `CHECK`; write fixed values |
| run step | as `execute`, plus: validate the output against its schema, record the SQL the step ran |
| run queries | run count queries for join coverage on given input artifacts |
| run invariant | call an invariant on given artifacts |
| describe result | the statistics for `--draft-schema` |

The worker stays stateless and has no database access. Planning still imports nothing. The
worker imports the named files, as it does to run them, and takes `fuzz=` values from the
decorators.

### 4.11 Node kinds and what is not run

| Kind | In a fuzz run |
|---|---|
| asset | runs; generated from its `output_schema` when it is a root, imported, or `fuzz=False` |
| sensor | generated from its `output_schema` |
| task | not run |
| invariant | runs after the nodes it reads; never run by `get` |

Partitioned nodes run as in 4.12. `asset_ref` inputs and what depends on them are not run in
this version. They are listed under `not run` with the reason, as is every node in the named
files that nothing ran.

### 4.12 Partitioned nodes

A partitioned function is written once for any number of keys (`barca docs partitions`; with
`batch=`, the same function serves one key, a batch or all of them). The fuzzer relies on that:
**the key is one more generated input**, and partitions add a single generated quantity, the
number of keys. Nothing about a partition needs its own schema form.

**Model.** A partitioned node is one table whose key starts with the dimension. Its
`output_schema` describes one key's rows; across keys it is the same table with the dimension
as a column, which is how batch mode already presents a partitioned input (a lazy table with the
dimension as a column) and splits a result (`PARTITION_BY`).

```python
@sensor(output_schema="retailer TEXT PRIMARY KEY, as_of TIMESTAMP NOT NULL")
def retailer_versions() -> dict: ...


@asset(partitions={"retailer": partitions_from(retailer_versions)},
       output_schema="retailer TEXT NOT NULL, sku TEXT NOT NULL, qty INT CHECK (qty >= 0), "
                     "PRIMARY KEY (retailer, sku)")
def cleaned(retailer: str, retailer_versions: str) -> pl.DataFrame: ...


@asset(inputs={"all_cleaned": collect(cleaned)})
def summary(all_cleaned: list[pl.DataFrame]) -> pl.DataFrame: ...
```

**Where keys come from.**

| Dimension | Keys in an example |
|---|---|
| `partitions_from(<dict source>)` | the source schema's primary key is the dimension; a key's value is its row's other column, or a dict of them when there are several |
| `partitions_from(<list source>)` | a one-column table |
| `partitions_from(<partitioned asset>)` | the upstream's keys |
| `partitions([...])` or `partitions(<expression>)` | a non-empty subset of the declared keys, as `get` resolves them |

**How many.** Each example draws the number of keys from 0 to `--max-keys` (default 3). Zero is
drawn only for derived dimensions: a literal list cannot produce it in a real run, and a finding
there would be false. With several dimensions, keys are drawn per dimension and the product runs
(2 × 2 at most by default).

**Execution.** Per example, the partitioned cone runs once per key, then each `collect` fan-in
once over the outputs in declared key order, as in a real run. Keys are passed exactly as barca
passes them (a dict source's integer key arrives as a string).

**Checks this adds**, on top of 4.7:

- **The dimension column.** When a per-key schema has a column named after the dimension, rows
  generated for key `k` have it set to `k`, and a step's output for `k` with another value is a
  finding (rows filed under the wrong partition).
- **Batch grouping.** For a `batch=` node, each example runs the drawn keys twice: in one call,
  and in a random grouping of them. Each key's output must be the same, so "one function for any
  number of keys" is checked, not assumed. A function that ranks, deduplicates or normalises
  across the whole call instead of within each key fails it.
- **The static check across keys.** A fan-in over a lazy input or a `UNION ALL` of per-key
  outputs has the key `(dimension, per-key key)`. A step keyed on the per-key key alone is
  `not proven`.

What the generated key counts reach that unpartitioned fuzzing does not: an empty fan-in
(`pl.concat([])` raises; a sensor that returns nothing produces it), a single key (a spread
across keys is NaN), a key with no rows, and key collisions across partitions.

**Cost.** With up to 3 keys, the partitioned cone runs up to 3 times per example; a batched node
twice more for the grouping check. Unpartitioned nodes are unaffected.

## 5. Determinism, Caching & Testing

- **Run hash.** Schemas and `fuzz=` are not in it. Adding or editing one recomputes nothing; a
  test checks that a `get` before and after adding a schema has the same run hash and is cached.
- **Determinism.** `--seed` reproduces a run, shrinking included. `--replay` reproduces one
  finding without generation.
- **Tests:**
  - **Rust units:** schema parsing and resolution; one test per inference rule in 4.3,
    including the cases that must stay "not proven" (a join not on a key, an unguarded `CASE`, a
    `CHECK` whose `WHERE` does not match); the generators (pools, keys, references, `CHECK`
    narrowing); the shrinker.
  - **End to end,** with planted bugs: a join fan-out, a NULL flag, a division by zero on empty
    input, a numpy value in a dict, a conservation break; the sandbox (credentials at import and
    in a step, network, duckdb remote reads).
- **Contract.** `barca fuzz --help` and the JSON output are snapshotted; the command, its flags,
  `output_schema=`, `fuzz=` and `[schemas]` are listed as experimental.

## 6. Performance

Nothing changes for runs. Planning reads two more decorator arguments as text, and parses schema
files only for `barca fuzz`.

| Part | Cost |
|---|---|
| the static check | milliseconds per step, with no Python |
| fuzzing (prototype) | 8–20 examples per second on DuckDB stages: about 10 s for the default 100, which fits a pre-push hook |
| the slowest step | bounds the run; `fuzz=False` keeps out a step that takes seconds |

## 7. Drawbacks

- **Schemas are work.** Each finding is a bug or an assumption to write down, and the first run
  on a real stage reports both. `--draft-schema` reduces the writing, not the reviewing.
- **Fuzzing runs user code.** The sandbox covers credentials, Python networking and duckdb, not
  every native library or local file. `fuzz=False` is the answer, and the manual says it first.
- **The static check is never finished.** Gaps in the SQL subset or the function table show as
  "unknown" or a false "not proven", never as a false "proven", but they need an owner.
- **Data is generated independently,** except where `REFERENCES` links it. Logic gated on
  conditions across many rows or tables is reached rarely; a fuzz-only asset that derives one
  generated table from another is the documented remedy.
- **Generation is barca's own.** Turning types and `CHECK`s into generators is new code, and the
  heuristics of a mature property-testing library (Hypothesis's boundary bias and targeted
  search) have to be rebuilt where they matter.

## 8. Rationale & Alternatives

- **The Python prototype.** The trials ran an engine written in Python: dataframely schemas,
  Hypothesis, and sqlglot for the static check. It established the design, but it put planning in
  Python next to barca's Rust planner, with a second parser and a second execution path. This
  RFC keeps its behaviour and moves the engine to Rust.
- **Schemas in Python (dataframely, pandera).** Rust cannot read them, rules written as polars
  expressions are a second language beside the pipeline's SQL, and the static check could
  recognise only two shapes of them. SQL DDL gives keys, references and `CHECK`s in the language
  the queries are written in, readable at plan time.
- **Other neutral formats.**
  - JSON Schema describes rows, not keys or relations.
  - Frictionless Table Schema has keys and references but no conditions across columns.
  - Arrow schemas have types only.
- **Checks on real runs.** Validating outputs in `get` and `serve` costs every run, and the
  trials put the value in inputs that have not happened yet. A read-only `barca check` of cached
  results remains possible.
- **Coverage-guided fuzzers** (libFuzzer, AFL, `cargo-fuzz`) are guided by instrumentation of
  Rust code. The code under test here is Python steps, so they get no signal.
- **Deterministic simulation** (Antithesis; turmoil and madsim in Rust) finds bugs in
  interleavings of concurrent systems. These bugs are in data. Its vocabulary of assertions that
  hold *always* and events that happen *sometimes* is borrowed in 4.7.
- **SMT-based checking** of SQL (Cosette, VeriEQL) is stronger than key and nullability
  inference, and much more work for aggregates, floats and strings. Left for later.
- **More generation controls.** The prototype added derived tables, row bounds, generation
  ranges, branch labels, a step cache, and checks of single nodes and edges. None found a bug
  that the core plus a fuzz-only asset could not, so none is in this RFC.

## 9. Prior Art and Evidence

**Prior art:**

- **Property-based testing** (QuickCheck, Hypothesis, proptest): generation with shrinking.
- **Key and functional-dependency inference** over relational algebra: textbook, and used by
  query optimisers for join elimination.
- **Data tests on real data** (dbt tests, Great Expectations, Dagster asset checks): none
  generate inputs or prove properties of transformations.

**Field trials.** Six rounds of the Python prototype on a production barca project:

| Finding | Found by |
|---|---|
| a join back on part of the key, double-counting spend (HIGH) | static check and fuzzing; before them, only by a hand-written harness |
| a boolean output NULL for an unmapped upstream value | static check and fuzzing |
| 2 double-counting joins, 3 NULL/NaN propagations, 2 unit errors, 1 dropped-rows filter, in four larger stages | fuzzing, through output schemas and invariants |

What each design choice answers:

| Choice | Why |
|---|---|
| value pools | without them, the fan-out was missed until columns were narrowed by hand |
| barca's own shrinker | Hypothesis's took about 5.5 min per report; barca's takes 18 s |
| `*` columns, one column per line | they named the cause in every confirmed bug |
| `inconclusive` | a P&L check reported `ok` over 200 examples whose lookup join never matched |
| the sandbox | a default setup would have run steps with real storage credentials |
| captured queries | every real stage built its SQL in helpers |
| positional `GROUP BY`, `UNION BY NAME`, guarded `CASE`, `CHECK` facts | without them the fixed join could not be told from the buggy one |

## 10. Unresolved Questions

1. **Exit code for "not proven".** 0 in this draft; a `--fail-unproven` could enforce proofs.
2. **References by name.** The prototype shared keys between columns named like another
   table's primary key, with nothing declared. This RFC requires `REFERENCES`. That is
   predictable, but costs a line per relation.
3. **Steering.** The prototype steered Hypothesis toward examples where more steps returned rows.
   Whether proptest-based generation needs its own form of it is to be measured.
4. **Are partitions really transparent to fuzzing?** 4.12 assumes a partitioned function
   behaves the same for any keys, so the number of keys is the only thing to generate. A
   function that branches on the key's value (`if retailer == "r01"`), or reads data the key
   names outside its inputs, breaks the assumption; whether such steps are common enough to
   need key-value coverage (each declared key at least once, reported as `sometimes`) is to be
   measured on the trial project.
5. **sqlparser coverage** of DuckDB syntax used in real stages, measured on the trial project
   before acceptance.

## 11. Future Possibilities

- `barca check`: validate cached results against their schemas, read-only, on request.
- Schemas in the UI and in `barca sql`: column types, keys and references beside each asset.
- SMT-based checking of invariants on SQL steps.
- Running examples in parallel workers.
