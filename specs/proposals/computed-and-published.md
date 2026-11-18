# Computed and published

Status: unapproved, unimplemented. Two principles were decided in the discussion of
[#951](https://github.com/barca-orc/barca/issues/951) on 2026-11-17 and are the premise of
this document: a step being *computed* and its output being *published* are separate facts,
and publication does not occupy step capacity. Everything else here (the rules, failure
semantics and open questions) is a draft to decide. [#1006](https://github.com/barca-orc/barca/issues/1006)
tracks it; its first pieces are #1002, #1004 and #1005. Nothing in it is a current contract; the
owners it would change are [execution](../behavior/execution.md),
[resources](../behavior/resources.md), [upload receipts](../interfaces/artifact-upload-receipts.md),
[artifact formats](../interfaces/artifact-formats.md) and the
[worker protocol](../interfaces/worker-protocol.md).

## The problem

Today a step has one "done". It holds its capacity slot, and with a store has no
materialization row, until its function returned, its artifact was encoded, its sinks were
written and its upload was confirmed. Four findings from the #951 experiments are that one
choice (0.43.1, object-store emulators, one connection modelled at 20 MB/s):

| Finding | Measured |
| --- | --- |
| A sink write holds the slot | A 355 MB sink held the slot 4.7–9.9 s after a 2 s step; upload is 70–95% of a large sink write |
| Sink time is in no step's duration | The duration is stamped before sinks are written, the result is reported after |
| A sink materializes the result a second time | A DuckDB relation's query ran twice: 16.8–18.0 s with a 12 MB local sink, 8.7–9.1 s without. An aggregate relation's second encode had a different row order than its artifact |
| A restart redoes finished steps | A step whose upload was unconfirmed at the drain deadline gets no row, although its artifact is complete on local disk: about 10 s redone for a 3 s step with a 120 MB sink |

## Proposed states

A step's output moves through three facts, recorded separately:

1. **Computed**: the function returned and the artifact is complete at its local path. The
   step is recorded, its capacity is released, dependents may start, and a later run reuses it.
2. **Prepared**: every publication of the step is reduced to files on local disk that need
   only to be transferred. For the store upload and for a file sink in the artifact's format
   this is the artifact's pinned snapshot, and costs a hard link. For a dataset sink
   (`@sink("dir/")`, `partition_by=`, `rows_per_file=`) or a sink in another format it is an
   encode to local staging files.
3. **Published**: the bytes are at their destination and confirmed: the store object (a
   receipt) and each sink (a `sink_publications` record).

## Proposed rules

- **Capacity covers computing and preparing, not publishing.** Preparing that needs an encode
  uses CPU and memory proportional to the result, so it stays inside the step's slot.
  Publishing is a byte transfer from local files and is admitted by its own bound.
- **Publication is bounded by staged bytes, not by a new concurrency limit.** Transfers keep
  the transfer helper's existing concurrency (four files at once per run). What is new is a
  bound on bytes pinned in staging: when it is reached, a step waits to be computed until
  transfers drain, because unbounded pinned snapshots would trade a slow run for a full disk.
- **A sink publishes the artifact; it does not materialize the result again.** A file sink in
  the artifact's format holds exactly the artifact's bytes. A lazy result (DuckDB relation,
  LazyFrame) is evaluated once.
- **A run completes when everything it owes is published or has failed.** Publication overlaps
  later steps; the run's end waits for it.
- **A dependent sees the asset, not the sink.** A step's inputs come through the DAG as
  artifacts; a sink is a copy for systems outside barca. A sink is guaranteed to be at its
  destination when its run completes, not when its dependents start. (Today it is written
  before the step is reported, so a dependent reading the sink's path out of band finds it;
  that was never a declared dependency.)
- **Durations are reported per fact.** A step reports compute time (as today) and its
  prepare time; publication time is reported per destination against the run, so "wall minus
  the sum of steps" has a named owner.
- **A cache hit owes whatever is not published**, as today: a missing store object is a pending
  upload, a sink without a current `sink_publications` record is republished. Republishing an
  eligible sink is a copy of the artifact, not a decode and re-encode.
- **Without a store and without sinks nothing changes**: computed is the only fact.

## Failures and recovery (proposed)

| Event | Effect |
| --- | --- |
| Store upload fails after the step computed | As #390 today: the step stays succeeded, the upload becomes a `pending_uploads` row, the run does not fail |
| Sink publication fails after the step computed | Decided 2026-11-17: neither the step nor the run fails. A sink is a leaf that nothing in the run waits on. The step stays succeeded, a warning names the step and sink, the sink has no publication record and the next run publishes it. Nothing is recomputed |
| Preparing a sink fails (encode error, unsupported format) | The step fails, as today: this happens inside the slot, before the step is reported |
| Drain deadline, cancel or requeue with transfers in flight | Computed steps keep their rows. Unconfirmed uploads become `pending_uploads` rows; unpublished sinks have no record. The requeued run treats both as owed and recomputes nothing |
| Coordinator and helper killed outright | The same, from what was recorded at computed: rows exist, publications are owed. Staging cleanup is the existing dead-PID backstop |

A failed sink publication and a failed store upload are handled alike: owed, reported and
retried. This reverses version 2 of [artifact formats](../interfaces/artifact-formats.md),
where a sink that cannot be written fails its step; that rule would remain only for a sink
that cannot be prepared.

## What this changes in existing owners

- [Upload receipts](../interfaces/artifact-upload-receipts.md): "an unconfirmed upload cannot
  publish a receipt-driven checkpoint row" would no longer hold back the row. The row is
  written at computed with the producer's digest, as the fail-soft path already does, and a
  later receipt fills a missing `output_hash`.
- [Artifact formats](../interfaces/artifact-formats.md): "a sink that cannot be written fails
  its step" narrows to a sink that cannot be prepared; a publication failure is owed and
  retried. The sink's bytes are defined as the artifact's for an eligible file sink.
- [Worker protocol](../interfaces/worker-protocol.md): the worker returns prepared sink
  requests with its result instead of writing sinks; the helper's `put` already accepts any
  destination. A transfer helper is needed for a remote sink even with local artifacts.
- [Resources](../behavior/resources.md): capacity is defined over computing and preparing.

## Open questions

1. **A dependent that acts on a sink being published.** Reading another step's sink is
   settled above. What remains is a dependent that does not read the sink but tells an
   external system about it (a `@task` that starts a warehouse load): it can run while the
   transfer is in flight. Not proposed now: a per-sink option to publish before dependents
   start, which fits this model if the case turns out to matter.
2. **The staging bound**: a default, and whether it is configured in bytes or as a fraction of
   free disk.
3. **What a run with an owed sink reports.** With the decision above it exits 0, so a cron
   or CI caller learns of a missing export only from the warning, the run's JSON or
   `barca status`. To confirm: exit 0, and which output key carries the owed sinks.
4. **When a server's ledger reports a computed result to another machine.** A laptop asks the
   ledger which results exist and fetches them from the store
   (`barca docs server-mode`). A row written at computed could be reported before its object
   is in the store. Proposed: the server reuses its own computed results at once, and the
   ledger reports a result to other machines only once its upload is confirmed. Not traced in
   the code.
5. **Preparing outside the slot for dataset sinks** whose encode streams with bounded memory.
   Not proposed now; it would need the memory bound stated and measured.
6. **How publication appears** in `barca status`, run JSON and the UI (a step computed but not
   yet published), which is a CLI contract change.

## Evidence it would need

- A run of large sinks at `capacity = 1` where the next step starts while the previous sink
  uploads, with the same final bytes at every sink as today.
- A restart at each of: sink in flight, upload in flight, between steps. The requeued run
  executes only the interrupted step.
- A late sink failure: exit 1, the step named, no recomputation on the next run, the sink
  written by it.
- A lazy result evaluated once, and a sink byte-identical to its artifact.
- Staging stays under its bound when steps compute faster than the network uploads.
- Real object-store latency, which the experiments above did not have.
