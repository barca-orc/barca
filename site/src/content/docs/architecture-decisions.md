---
title: Architecture Decisions
description: The main decisions in barca's execution engine, what was tried first, and what was rejected.
---

This page records the main decisions in barca's execution engine: what the code does now, what
was tried before, and what was rejected. [Architecture](/architecture/) describes the current
code without the history.

Performance figures from early development have been removed from this page because they were
not tied to a version, a machine or a date. Current measurements are on the
[framework comparison](/comparisons/framework-comparison/) page and in `benchmarks/RESULTS.md`
in the repository.

## 1. Unix domain sockets for worker coordination

### The decision

Workers talk to the Rust coordinator over a Unix domain socket, in JSON frames with a 4-byte
length prefix. Each worker keeps one connection open for its lifetime.

### What we tried

**v0.1.x: JSON lines on stderr.** Workers wrote `BARCA:2:{json}` lines to stderr and Rust read
them. The coordinator sent work in batch files and had no way to send anything to a running
worker, so `parallel()` could not be built on it.

**v0.2.0, first attempt: one socket per worker, polled in turn.** The coordinator polled each
connection with a 1 ms timeout. One pass over N workers took about N milliseconds, and with
more than about 100 workers the coordinator fell behind the rate at which workers finished and
runs hung.

**v0.2.0, final: one task per connection, one channel.** Each connection gets a tokio task
that forwards its messages to a single mpsc channel. The coordinator reads from that channel,
so the cost of receiving a message does not depend on the number of workers. This is the
current design. `crates/barca-core/tests/socket_stress.rs` exercises many workers connecting
to one listener at once.

### Why a Unix socket

- It is bidirectional: a worker sends results and receives the next work on the same
  connection. Pulling work and `parallel()` both need this.
- It is local. Nothing goes through the network stack and no port is opened.
- Length-prefixed JSON takes a few lines in both Rust and Python and can be read with
  ordinary tools when debugging.

### What we rejected

- **Shared memory or mmap.** It needs a custom format and careful synchronisation. The socket
  was not a measurable cost for steps that do real work.
- **gRPC or HTTP.** More framing and more dependencies, with nothing gained for local
  processes.
- **Named pipes.** A pipe carries data one way, so each worker would need two.

## 2. Stateless workers and one ready queue

### The decision

Workers hold no assignment of their own. Steps that are ready sit in one queue owned by the
coordinator, and a worker that has nothing to do is given the next ones.

### What we tried

**A queue per worker.** The first coordinator held one queue per worker and dealt steps out
round-robin when a phase was loaded. Problems:

- If a worker's first step was slow, the steps queued behind it waited while other workers
  were idle.
- When a worker called `parallel()` and was suspended, its queued steps had to be moved to
  other workers. That needed deadlock detection and extra queue management.
- Round-robin could not take step duration into account.

### Why one queue

- An idle worker always has work if any step is ready.
- A slow step delays only itself.
- Calls made through `parallel()` go on the same queue and any idle worker takes them.
- The queue is the one place where ordering and batching decisions are made (see decision 6).

## 3. SIGSTOP and SIGCONT for `parallel()`

### The decision

When a step calls `parallel()`, the coordinator:

1. sends SIGSTOP to that worker, which freezes it with its state in memory;
2. starts a temporary worker so the number of active workers stays the same;
3. puts the calls on the ready queue;
4. when all of them have finished, stops the temporary worker, sends SIGCONT to the original
   worker and sends it the results.

### What we tried

**Suspension tracked in the coordinator.** The first design recorded which workers were
waiting on a parallel group and ran a deadlock check that started temporary workers when every
worker was suspended. The check had unclear cases (some workers suspended, not all), temporary
workers needed queues of their own, and with per-worker queues a suspended worker's steps were
stranded.

**Running the calls in the calling worker.** With a pool of one, the worker ran its own
parallel calls. That works and gives no parallelism.

### Why signals

- A stopped process uses no CPU and resumes where it was. No Python-side cooperation is
  needed.
- With a replacement started, the number of running workers stays at the pool size.
- Nesting works the same way: a temporary worker that calls `parallel()` is stopped and
  replaced in turn.
- The coordinator's queue logic does not need to know about processes. The I/O layer
  (`io_loop.rs`) handles them.

### Limitations

- Unix only. Barca does not run on Windows.
- A stopped process keeps its memory. Deep nesting with large data in memory uses that much
  RAM for as long as the calls run.
- Calls made through `parallel()` are not steps of the plan: they are not counted in a run's
  step totals and have no telemetry spans of their own.

## 4. The plan is loaded into the coordinator by type, not by name

### The decision

The coordinator's `load_phase(phase, provided_inputs)` takes a planner `Phase` directly. Every
queue item carries the planner's `StepId`. There is no intermediate mapping by string.

### The problem this solved

The first bridge between planner and coordinator used two string maps, and resolved
dependencies by looking names up in them:

```rust
// A missing key dropped the dependency without an error.
if let Some(&upstream_item) = node_to_item.get(upstream_id) {
    deps.push(...);
}
```

This caused three bugs, none of which raised an error:

- **Missing outputs** (`final_output: null`). The coordinator's ids had a branch suffix that
  the planner's ids did not, so outputs were not matched.
- **Progress undercounted.** Callbacks fired for some steps only.
- **Failures not reported.** A failed item was recorded in the coordinator and never checked,
  so the process exited with code 0.

### The step count check

Every step must end as done, failed or skipped. Two checks enforce it:

1. **Planned steps.** `load_phase()` returns the number of items it added. `commands.rs`
   asserts that it equals the number of steps (or partition keys) in the phase. A mismatch is
   a bug in barca and panics.
2. **`parallel()` calls.** A parallel group counts completed items, and the stopped worker is
   resumed only when the count equals the number of calls.

## 5. Rust for planning, Python for execution

### The decision

The Rust binary does parsing, graph construction, planning, cache checks, worker management
and the database. Python workers run your functions, read and write artifacts, and send
`parallel()` requests.

### Why not all Python

A command that has nothing to run should return without waiting for a Python interpreter and
your imports. With planning in Rust, a fully cached `barca get` starts no worker at all, and
a run that does execute pays for interpreter start-up once per worker, not once per step.

### Why not all Rust

The code being run is Python. Embedding an interpreter in the binary would tie barca to one
Python version and would not use your virtualenv. Barca starts ordinary Python processes
instead:

- your virtualenv is used as it is (the `python` beside the `barca` executable, else
  `python3` on `PATH`);
- Python 3.12 and later are supported;
- a worker that crashes does not take the coordinator down.

## 6. Batch size from measured cost, not declared limits

### The decision

Workers are started once per run and kept for all its phases. A worker takes a batch of `K`
steps from the ready queue at a time. `K` is computed from measured step time, not from
concurrency limits written by the user:

```text
K = clamp(
      floor   = ceil(comm_cost / (per_task_cost × 1%)),   # keep coordination under 1% of work
      ceiling = max(1, remaining / (workers × 3)),        # leave enough batches for every worker
    )
```

Workers time every step (wall time, CPU time, peak memory) and send the numbers back with the
result. The same message releases the step from the batch, carries the reference to the
result and updates the estimate.

The estimate for a step comes from, in order: its own history, the history of other
partitions of the same node, and a default of 30 seconds for a node that has never run. The
default is high on purpose. Guessing too high gives `K = 1` and a little extra coordination.
Guessing too low puts several slow steps on one worker while others sit idle. For the same
reason the estimate rises quickly and falls slowly (by at most 30% per observation).
Estimates are saved in the `cost_estimates` table at the end of a run, so the default applies
to a node only until it has run once.

A batch is a lease. If a worker dies, the step it was running uses up one of its attempts, and
the steps it had not started go back to the front of the queue. A step can therefore start
more than once.

### What we rejected

- **Concurrency limits declared by the user** (tags, slots, pools). They ask a person to guess
  a number that barca can measure, since it runs local processes and keeps run history.
- **A separate calibration run.** The first batches of a run with no history serve that
  purpose: the 30-second default makes them one step each.
- **Sending data through the queue.** The queue carries references to artifact files and the
  worker reads the files itself. The cost of handing out a batch is then fixed, which is what
  lets batching reduce it.

Batching changes only which worker runs a step and when. The plan (phases, the set of
partitions) does not depend on measured times.

## 7. Not built

### Worker affinity

A step goes to whichever worker is idle. Barca does not prefer a worker that already has the
relevant modules imported or data in memory.

### Windows

SIGSTOP and SIGCONT and Unix domain sockets are both Unix features. Windows support would need
a replacement for each.

### Distributed execution

A run uses the processes of one machine. Running steps on several machines would need a
network transport in place of the Unix socket and a way to hand work between machines. A
remote store shares results between machines today; it does not split one run across them.

### What changed since this page was first written

- Retries now wait `retry_backoff × attempt` seconds when `retry_backoff` is set. An earlier
  version of this page said retries were immediate only.
- Work is handed out in leased batches (decision 6). Earlier versions sent one step at a time.
