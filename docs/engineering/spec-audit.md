# Pre-merge specification audit

Audited head: `bb5d5f08` on 2026-10-26, PR #659. Status: findings, not approval to merge.
This is non-normative evidence. It does not change approved requirements or repair runtime
behavior. [Specification index](../../specs/README.md); [architecture plan](architecture-cleanup.md).

## Classification and merge recommendation

- **U — unconsidered or underspecified:** an observable boundary lacks a decision or a usable
  acceptance criterion. A missing test alone does not establish that nobody considered it.
- **V — violated:** concrete implementation behavior disagrees with a stated obligation.
  Each finding distinguishes an implemented-contract mismatch from approved, unimplemented
  intent. The latter is architectural backlog, not a regression introduced by this PR.
- **Q — specification quality:** an invalid model, misleading claim, contradictory wording,
  broken navigation or insufficiently precise evidence ownership. These are spec defects,
  even when the runtime is behaving as intended.

**Hold the clean-audit/merge recommendation for Q1–Q5.** They need a small specification and
checker correction pass, not the entire engine redesign. Resolve the log-bound policy in V1
at the same time. U findings need explicit decisions or recorded deferral before their
corresponding architecture changes. V2–V10 remain separately tracked implementation work;
they must not be presented as passing conformance. No merge or external issue creation was
performed by this audit.

## U — unconsidered or underspecified boundaries

### U1. Multiple sinks have no partial-publication recovery contract

New cross-sink gap; high confidence; decide before sink/lifetime refactoring.
[Artifact formats](https://github.com/barca-orc/barca/blob/bb5d5f08/specs/interfaces/artifact-formats.md#L20) says a sink failure fails
the step and staged writes preserve prior bytes. It does not define all-or-nothing behavior
across sinks, whether later sinks still run, or what an explicit retry repeats.

The [worker](https://github.com/barca-orc/barca/blob/bb5d5f08/python/barca/_worker.py#L868) attempts every sink, keeps successful writes
and raises a combined error afterward. The [materialization path](https://github.com/barca-orc/barca/blob/bb5d5f08/python/barca/_worker.py#L989)
then reports failure rather than a successful result. This is not proof of a transactional
violation: the spec never promises a transaction across sinks. Specify whether successful
external publications survive and how retry/side-effect uncertainty is exposed. Add a
first-sink-success/later-failure case, including a later successful sink and retry.

### U2. Project budgets lack identity and conflicting-settings arbitration

Newly identified detail within an already acknowledged resource-design gap; medium confidence;
record a decision before implementing cross-process resource ownership.
[Resources](https://github.com/barca-orc/barca/blob/bb5d5f08/specs/behavior/resources.md#L21) scopes one budget across CLI, server,
background work and children, while allowing explicit overrides. It does not say which
budget wins when two live invocations supply different overrides, how a live budget changes,
or whether environments and canonical/symlinked project roots share an owner.

Current [execution settings](https://github.com/barca-orc/barca/blob/bb5d5f08/crates/barca-core/src/config.rs#L35) resolve environment
and file settings per caller. [Shared capacity](https://github.com/barca-orc/barca/blob/bb5d5f08/crates/barca-core/src/capacity.rs#L1)
is an in-server ledger, not that project-wide authority. This audit does not choose a lock,
daemon or quota mechanism. Acceptance needs conflicting overrides and equivalent project
paths, not only two requests with the same configuration. General enforcement/accounting
questions were already considered; this is the missing arbitration rule.

### U3. Read-only preview does not define resolver/import side effects

New cross-owner ambiguity; executed observation; decide before promising a side-effect-free
preview. [Agents](https://github.com/barca-orc/barca/blob/bb5d5f08/specs/behavior/agents.md#L67) requires a read-only impact preview that
does not execute work to resolve unknown sensors. The [declaration owner](https://github.com/barca-orc/barca/blob/bb5d5f08/specs/interfaces/user-api.allium#L176)
permits explicit resolver imports for nonliteral partitions and computed grouping.

[Explain](https://github.com/barca-orc/barca/blob/bb5d5f08/crates/barca-core/src/queries.rs#L472) builds the DAG, and
[dynamic resolution](https://github.com/barca-orc/barca/blob/bb5d5f08/crates/barca-core/src/load.rs#L442) imports the module and evaluates
its expression. The probe's `--dry-run` creates an import-time side-effect file, while the
node-body marker remains absent. Thus the experiment does **not** show a node or sensor
was executed by preview. It shows that “read-only” needs an explicit boundary: orchestration
state only, or arbitrary project side effects too. Define unknown/unevaluated partition
behavior and an acceptance case with an import side effect.

### U4. Evidence coverage is not yet an obligation-by-obligation inventory

Known verification limit, newly quantified; follow-up for architecture acceptance gates.
The [checker](https://github.com/barca-orc/barca/blob/bb5d5f08/scripts/check-specs.py#L112) validates named definitions and references,
not their implementation status or test assertions. There are 47 named invariants; 12 are
mentioned in Python test module docstrings. This count is **not a coverage percentage**:
Rust tests and Python tests without those labels cover additional behavior, and a docstring
mention is not proof of an assertion.

The missing artifact is a per-obligation record of implemented/pending/deviation status and
the exact executable scenario that supports it. Whole-module links are useful navigation,
but cannot establish that every invariant in a document is tested. Start with the first
publication/ownership refactor rather than implying the current index already certifies it.

Already considered open questions, not new U findings: observation scope and multi-source
ordering, distinct retained versions and remote deletion scope, direct-check deadlines,
invocation deduplication lifetime, log reconnection guarantees and environment/dependency
snapshot boundaries. They remain in the owners and [discussion handoff](requirements-handoff.md).

## V — implementation disagreements

### V1. Recorded logs exceed the implemented contract's hard 16 MiB bound

New confirmed implemented-contract mismatch; high confidence; resolve before merging a
truthful contract. [Task output](https://github.com/barca-orc/barca/blob/bb5d5f08/specs/interfaces/task-output.md#L54) says at most
16 MiB of line text, then a marker. [Recorder::push](https://github.com/barca-orc/barca/blob/bb5d5f08/crates/barca-core/src/run_logs.rs#L109)
records the whole crossing line before checking the cap; `push_note` can append afterward.

Executed: one 17 MiB print succeeds and the database retains a line of **17,825,792 bytes**,
exceeding **16,777,216**. The [existing cap test](https://github.com/barca-orc/barca/blob/bb5d5f08/crates/barca-core/src/run_logs.rs#L275)
uses two exactly half-cap lines, so it misses overshoot. Decide between an explicit soft
crossing-line limit and a real byte bound, including marker/framework notes. Do not silently
truncate user lines or redefine the promise as part of this audit.

### V2. A historical cache hit can select the wrong current partition result

Known implemented correctness defect #657; reproduced; high confidence; pending PR #658.
[Freshness INV-SELECT-01](https://github.com/barca-orc/barca/blob/bb5d5f08/specs/behavior/freshness.md#L88) requires the current identity,
not the latest historical row. [Incremental lookup](https://github.com/barca-orc/barca/blob/bb5d5f08/crates/barca-core/src/incremental.rs#L807)
selects `MAX(id)` per node. Inputs `1,2,1,1` produce `2,4,2,4`, where the last result must
remain 2. The probe records this independently of the previously cited audit.

The owner explicitly discloses this defect. #658 was still open when checked. Recheck it
before merge and refresh baseline notices if it lands; no claim that this docs branch fixes it.

### V3. Run-start code/configuration snapshots are not established

Known approved-but-unimplemented obligation; reproduced for project helper code; high
confidence. [Operations](https://github.com/barca-orc/barca/blob/bb5d5f08/specs/behavior/operations.md#L62) and INV-ATTEMPT-01 freeze
active attempts. The [source loader](https://github.com/barca-orc/barca/blob/bb5d5f08/python/barca/_source_import.py#L81) reads source
at import time; cached modules are not a whole-project snapshot.

Executed: start a run, wait until its sensor is executing, change a not-yet-imported helper
from returning 1 to 2, then release the sensor. The downstream asset returns **2** within
that same run. This establishes a code-snapshot gap; it does not independently audit every
configuration/environment dependency. Keep it separate from artifact-byte isolation.

### V4. Ordinary user subprocesses can outlive a successful run

Known approved process-tree requirement, newly reproduced concrete case; high confidence.
[Operations](https://github.com/barca-orc/barca/blob/bb5d5f08/specs/behavior/operations.md#L79) requires entire owned-process-tree
cleanup and one-off execution cleanup. [Worker shutdown](https://github.com/barca-orc/barca/blob/bb5d5f08/crates/barca-core/src/io_loop.rs#L1073)
signals worker PIDs; [graceful termination](https://github.com/barca-orc/barca/blob/bb5d5f08/crates/barca-core/src/io_loop.rs#L1892) also
addresses a direct child PID. This is not a general descendant ownership/reaping boundary.

Executed: an ordinary task starts a Python child with redirected standard streams, without
creating a new session, then returns successfully. Its child remains alive after the run.
The audit identifies and terminates only its own tagged child and confirms cleanup. Do not
infer safety for unrelated processes or deliberately detached services from this probe.

### V5. Global resource allocation, enforcement and bounded buffering are absent

Approved implementation gap; high confidence from source, no new load benchmark performed.
[Resources](https://github.com/barca-orc/barca/blob/bb5d5f08/specs/behavior/resources.md#L21) requires aggregate CPU/memory budgets and
truthful enforcement across processes. [Configuration](https://github.com/barca-orc/barca/blob/bb5d5f08/crates/barca-core/src/config.rs#L23)
has pool/capacity controls, and [SharedCapacity](https://github.com/barca-orc/barca/blob/bb5d5f08/crates/barca-core/src/capacity.rs#L10)
shares weights within one server; neither establishes aggregate CPU use or job memory.

[Run state](https://github.com/barca-orc/barca/blob/bb5d5f08/crates/barca-core/src/execution.rs#L357) retains per-key maps and
[StepRecorder](https://github.com/barca-orc/barca/blob/bb5d5f08/crates/barca-core/src/persist.rs#L210) uses an unbounded sender. In
addition to the already tracked cold execution/inspection gaps (#655/#656), live
[RunChannel](https://github.com/barca-orc/barca/blob/bb5d5f08/crates/barca-server/src/state.rs#L259) retains every event in a vector and
[ordinary log reads](https://github.com/barca-orc/barca/blob/bb5d5f08/crates/barca-cli/src/commands/logs.rs#L135) materialize a complete
vector. [Agent streaming bounds](https://github.com/barca-orc/barca/blob/bb5d5f08/specs/behavior/agents.md#L81) are therefore not
established by paging graph keys or capping worker count. Include slow consumers, queued
requests and long-lived event streams in the resource audit; do not repeat historical peak
measurements as if they were measured again here.

### V6. Server drain/restart policy differs from approved lifecycle intent

Known approved implementation gap; high confidence from source and existing scenario tests.
[Operations](https://github.com/barca-orc/barca/blob/bb5d5f08/specs/behavior/operations.md#L83) rejects new work during drain and recovers
assets while leaving tasks interrupted. [Admission](https://github.com/barca-orc/barca/blob/bb5d5f08/crates/barca-server/src/queue.rs#L219)
continues recording requests; [run completion](https://github.com/barca-orc/barca/blob/bb5d5f08/crates/barca-server/src/runs.rs#L573)
can requeue deadline-cancelled work without an asset/task distinction. The existing
[queue tests](https://github.com/barca-orc/barca/blob/bb5d5f08/python/tests/test_serve_queue.py#L305) explicitly accept requests during
drain and verify requeue behavior.

This is an intentional current-versus-target difference, not proof of a new regression.
Change admission, durable transitions and task retry semantics together in a behavior PR.

### V7. Invocation recovery is not client-request deduplication

Known approved implementation gap; high confidence. [Operations](https://github.com/barca-orc/barca/blob/bb5d5f08/specs/behavior/operations.md#L50)
requires immutable invocation target/arguments and distinguishable response recovery versus
explicit retry. [RunRequest](https://github.com/barca-orc/barca/blob/bb5d5f08/crates/barca-core/src/outcome.rs#L132) supplies execution
options, while [admission](https://github.com/barca-orc/barca/blob/bb5d5f08/crates/barca-server/src/queue.rs#L220) generates a new run ID.
A completed-result ledger's `client_run_id` is not a submission idempotency mechanism.

Do not present existing run IDs or step attempt counters as this contract. Define allocation,
validation, retry/retrieve actions and retention before adding automatic transport retries.

### V8. Proactive freshness recovery differs from the sensor scheduling engine

Known approved implementation gap; high confidence from source, no source-outage runtime
suite run here. [Operations](https://github.com/barca-orc/barca/blob/bb5d5f08/specs/behavior/operations.md#L33) specifies a one-minute
managed default, exponential polling backoff capped at 15 minutes and immediate direct-check
sharing. [Scheduled jobs](https://github.com/barca-orc/barca/blob/bb5d5f08/crates/barca-core/src/schedule.rs#L102) include explicit
Schedule declarations only; [scheduler eligibility](https://github.com/barca-orc/barca/blob/bb5d5f08/crates/barca-server/src/scheduler.rs#L105)
can stop a repeatedly failing job under `max_failures`. This is not the proposed sensor
backoff/recovery policy. Existing per-step linear retry delay is a separate mechanism.

The existing outage result also does not establish the proposed explicit possibly-stale
alternative. Its selection/deadline contract remains openly unresolved.

### V9. Version retention differs from current size-based eviction

Known approved implementation gap; high confidence. [Operations](https://github.com/barca-orc/barca/blob/bb5d5f08/specs/behavior/operations.md#L99)
requires three retained versions by default and cleanup by requested reclaimed space with
selected/active protection. [GC](https://github.com/barca-orc/barca/blob/bb5d5f08/crates/barca-core/src/gc/mod.rs#L1) enforces a cache
size threshold and eviction tiers. [Cache configuration](https://github.com/barca-orc/barca/blob/bb5d5f08/crates/barca-core/src/config.rs#L108)
contains a size limit, not retained-version identity or count.

History preservation and pinning are useful existing pieces, not evidence of the proposed
retention policy. Resolve version recurrence/shared-byte/remote-scope questions before replacing it.

### V10. Shared execution, ordered publication and scalable navigation remain intent

Known approved gaps, not newly introduced runtime failures. The
[ownership owner](https://github.com/barca-orc/barca/blob/bb5d5f08/specs/behavior/current-state-and-execution.md#L21) requires matching
identity sharing, revision-safe publication, independent checking and bounded state.
[RunSession](https://github.com/barca-orc/barca/blob/bb5d5f08/crates/barca-core/src/execution.rs#L395) still owns its own worker pool;
[artifact paths](https://github.com/barca-orc/barca/blob/bb5d5f08/python/barca/_artifacts.py#L545) name computations and can be overwritten
on recompute. The [receipt owner](https://github.com/barca-orc/barca/blob/bb5d5f08/specs/interfaces/artifact-upload-receipts.md#L63)
explicitly demonstrates existing input-byte isolation limits. The finite model assumes stable
inputs and atomic commits; it does not close these gaps.

[Agent navigation](https://github.com/barca-orc/barca/blob/bb5d5f08/specs/behavior/agents.md#L36) requires continuation, filtering and
completeness for potentially large listings. Current [CLI limits](https://github.com/barca-orc/barca/blob/bb5d5f08/crates/barca-cli/src/bounded.rs#L13)
truncate presentation without a general cursor/offset mechanism; [server node inspection](https://github.com/barca-orc/barca/blob/bb5d5f08/crates/barca-server/src/inspect.rs#L79)
builds a full result. [Log arguments](https://github.com/barca-orc/barca/blob/bb5d5f08/crates/barca-cli/src/args.rs#L894) lack the approved
`--tail`/`--since` conventions. These remain implementation follow-ups; existing `--limit`
is not full pagination. Separate observation/status revision control from public wire design.

## Q — specification defects and misleading structure

### Q1. The Allium owner fails the available language checker

New confirmed defect; fix before merge. [Sink.partition_by](https://github.com/barca-orc/barca/blob/bb5d5f08/specs/interfaces/user-api.allium#L78)
uses `Sequence<String>`. `allium 3.0.4` checks language version 3 and reports:

```text
allium.type.undefinedReference: Type reference 'Sequence' is not declared locally or imported.
1 file(s) checked, 1 error(s), 3 warning(s).
```

The local language reference distinguishes explicit `List<T>` fields from inferred ordered
relationship sequences. Review the intended collection semantics, correct the type and run
`allium check` again. Earlier speculative observations about rule bindings are not included
as compiler failures: the tool reported this one error. Existing grammar tests explicitly do
not execute the model; a disclaimer about no CI compiler gate does not make the file valid.

### Q2. Upload-receipt success wording contradicts fail-soft terminal persistence

New confirmed prose contradiction; fix before merge. The
[receipt lifetime paragraph](https://github.com/barca-orc/barca/blob/bb5d5f08/specs/interfaces/artifact-upload-receipts.md#L48) says
unconfirmed requests never produce success rows, but the
[next paragraph](https://github.com/barca-orc/barca/blob/bb5d5f08/specs/interfaces/artifact-upload-receipts.md#L51) correctly allows a
successful computation with failed upload to produce a terminal success and pending upload.

[Receipt checkpoint handling](https://github.com/barca-orc/barca/blob/bb5d5f08/crates/barca-core/src/persist.rs#L332) and
[terminal persistence](https://github.com/barca-orc/barca/blob/bb5d5f08/crates/barca-core/src/persist.rs#L722) are different paths.
[Fail-soft upload controls](https://github.com/barca-orc/barca/blob/bb5d5f08/python/tests/test_fail_soft_uploads.py#L99) expect success
and pending uploads. Narrow the first guarantee to confirmed receipt-driven checkpoint
publication; preserve the fail-soft exception. This is not a recommendation to fail successful
computations whenever their backup upload fails.

### Q3. Digest-warning rules omit the implemented nondeterministic exemption

New stale-contract finding; fix before merge. The
[digest owner](https://github.com/barca-orc/barca/blob/bb5d5f08/specs/interfaces/materialization-digests.md#L22) warns for changed assets
and exempts Manual assets, but omits `deterministic=False`.
[checks_bytes](https://github.com/barca-orc/barca/blob/bb5d5f08/crates/barca-core/src/cache.rs#L426), the
[manual](https://github.com/barca-orc/barca/blob/bb5d5f08/crates/barca-cli/docs/cache.md#L711) and
[existing regression](https://github.com/barca-orc/barca/blob/bb5d5f08/python/tests/test_asset_cutoff.py#L424) agree on the exemption.
This is a spec omission, not a bug in warning suppression. Include both warning/report and
status exposure rules rather than correcting only the human message.

### Q4. “Attempt” and “explicit retry” need one boundary across owners

New cross-owner ambiguity; resolve wording or record an explicit policy decision before merge.
[Operations](https://github.com/barca-orc/barca/blob/bb5d5f08/specs/behavior/operations.md#L57) says tasks retry only when explicitly
requested, including after crashes; a new attempt captures current code/configuration.
[Execution](https://github.com/barca-orc/barca/blob/bb5d5f08/specs/behavior/execution.md#L69),
[RetryPolicy](https://github.com/barca-orc/barca/blob/bb5d5f08/specs/interfaces/user-api.allium#L29) and the
[coordinator](https://github.com/barca-orc/barca/blob/bb5d5f08/crates/barca-core/src/coordinator.rs#L650) describe configured automatic
physical attempts with linear backoff and worker replacement.

A `retries=N` declaration might itself count as explicit authorization, or the new operations
policy might deliberately replace it. The spec does not say. Distinguish an invocation attempt
from a worker/step retry, say which policy governs tasks versus sensors/assets, and state what
is superseded. This audit does not infer an unapproved removal of existing retry support.

### Q5. Two broken cache links escape the checker through multiline labels

New regression introduced by the structure cleanup; fix before merge. Links at
[execution](https://github.com/barca-orc/barca/blob/bb5d5f08/specs/behavior/execution.md#L62) and
[freshness](https://github.com/barca-orc/barca/blob/bb5d5f08/specs/behavior/freshness.md#L97) still target the removed
`../interfaces/cache-decisions.md`; their owner is now `behavior/cache-decisions.md`.
Both labels wrap onto a second line. The [link regex](https://github.com/barca-orc/barca/blob/bb5d5f08/scripts/check-specs.py#L35)
excludes newlines inside labels, so those Markdown links are never checked or traversed.

An independent scan allowing multiline labels finds both. The ordinary checker passes on
the audited head, which demonstrates a blind spot, not valid navigation. Correct both links
and add a multiline-link negative control. The prior “all links valid” summary was too strong;
this finding explicitly corrects it. The common heading structure is useful, but formatting
validation alone cannot establish a coherent or complete behavioral model.

## Owner inventory

“Sampled, no new mismatch” means source/evidence reviewed, not full conformance certification.
Flags can overlap; an approved owner may be well formed while its implementation is incomplete.

| Owner | Flags / audit result |
| --- | --- |
| [Experience](../../specs/behavior/experience.md) | Partial acceptance; no new concrete mismatch found in sampled CLI/progress/log evidence. “Excellent”, “compact” and “prompt” still require scenario-based evaluation. |
| [Freshness](../../specs/behavior/freshness.md) | V2 known historical-selection defect; Q5 broken link; 13 selected runtime controls cover parts of freshness/cache/inspection. |
| [Execution](../../specs/behavior/execution.md) | V4/V6/V10 target lifetime gaps; Q4 retry boundary; Q5 broken link. Existing run-local semantics must remain distinct from approved future sharing. |
| [Inspection](../../specs/behavior/inspection.md) | V5/V10; checking and cold-cost limitations explicitly disclosed. |
| [Performance](../../specs/behavior/performance.md) | V5; warm budgets are correctly scoped. No performance benchmark rerun, and no new hard-bound claim. |
| [Python runtime](../../specs/behavior/python-runtime.md) | V3 concerns future run snapshots, not process-local DuckDB semantics; three worker-lifetime controls pass with supported DuckDB. |
| [Cache decisions](../../specs/behavior/cache-decisions.md) | V2 known selection deviation; persistence/provenance sampled without a new concrete contradiction. |
| [Resources](../../specs/behavior/resources.md) | U2, V5; aggregate enforcement and fairness are approved intent, not shipped quotas. |
| [Agents](../../specs/behavior/agents.md) | U3, V5/V7/V10; mixed implemented pieces and explicit outstanding capabilities. |
| [Operations](../../specs/behavior/operations.md) | V3–V9, Q4; approved lifecycle, retry, snapshot and retention behavior is largely outstanding. |
| [Current state / ownership](../../specs/behavior/current-state-and-execution.md) | V3/V5/V10; revision scope, stable inputs, sharing and bounded work remain implementation obligations. |
| [Artifact formats](../../specs/interfaces/artifact-formats.md) | U1 multiple-sink partial-publication semantics. |
| [Upload receipts](../../specs/interfaces/artifact-upload-receipts.md) | Q2; V10 input isolation is already explicitly disclosed. |
| [Doctor](../../specs/interfaces/doctor.md) | Sampled, no new mismatch; provider-acceptance limits are explicitly stated. |
| [Materialization digests](../../specs/interfaces/materialization-digests.md) | Q3 missing nondeterministic exemption. |
| [Metadata compatibility](../../specs/interfaces/metadata-schema.md) | Sampled, no new mismatch; full DDL is outside its scope, and proofs versus history are distinguished. |
| [Task output](../../specs/interfaces/task-output.md) | V1 false hard cap; live versus retained output distinction otherwise explicit. |
| [Worker protocol](../../specs/interfaces/worker-protocol.md) | Sampled, no new mismatch in protocol-5 framing/handshake/batch ownership. |
| [Python declarations](../../specs/interfaces/user-api.allium) | Q1 compiler failure; Q4 retry vocabulary. |
| [HTTP schema](../../specs/interfaces/server-api.openapi.yaml) | Sampled, no new concrete schema/router contradiction; full router suite not rerun. |
| [Finite model](../../specs/models/README.md) | Declared abstraction/exclusions match the model; no new mismatch found. TLC not rerun because model/config hashes are unchanged. |
| [External CLI contract](../../crates/barca-cli/docs/contract.md) | Sampled current flags/output owners; new agent conventions remain target requirements, not silently added flags. Full snapshot suite not rerun. |
| [Unapproved proposals](../../specs/proposals/api-ideas.md) | Excluded from implementation obligations; three recovery/error proposals remain unanswered. |

## Executed evidence and limits

The [probe script](spec-audit-repro.py) runs five controlled experiments entirely in temporary
projects, reports observations and cleans its owned descendant process. It uses the published
**barca 0.33.0 native binary with this branch's Python helpers**, not a rebuilt native branch.
The branch's production runtime changes relative to that release are source-reference comments;
its native test schema path is relocated. Do not call this a new branch-native build.

```bash
PYTHONPATH=python .venv/bin/python docs/engineering/spec-audit-repro.py \
  --binary .venv/bin/barca
allium check specs/interfaces/user-api.allium
python3 scripts/check-specs.py
```

Observed: historical sequence `2,4,2,4`; retained log line 17,825,792 bytes; preview import side
effect with no node-body execution; downstream helper returns new code value 2 during an
active run; ordinary child survives success and is cleaned by the audit. These are
counterexamples/observations, not five passing conformance tests.

Sixteen selected existing checks pass: all sensor-observation and partition-cache tests,
status/dry-run agreement, failed-update proof publication, needed-artifact recovery, and the
three direct worker lifetime outcomes (success, user failure, materialization failure).
Environment: macOS, Python 3.14.3, DuckDB 1.5.6, pandas 3.1.0, PyArrow 26.0.0, pytest 9.1.1.
An initial worker run accidentally used DuckDB 2.0.0, outside the declared `<2` dependency
range, and failed three fixtures. Those failures were excluded from product findings and
rerun successfully within the supported range.

The structural checker passes after indexing this report; that does not negate Q1 or Q5.
No cloud-backend suite, native rebuild, full performance run, complete CLI/router suite or
new model-checking run was performed. Existing test links and prior results remain evidence
of their stated scope only. Findings are deliberately left visible for the requested
pre-merge decision rather than silently rewriting policy or fixing unrelated engine behavior.
