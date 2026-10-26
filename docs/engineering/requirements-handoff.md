# Requirements discussion handoff

Status: resumable discussion, not a completion claim. Updated 2026-10-26.

## Where to resume

The user requested questions in batches of two or three, with the agent proposing likely
answers and concentrating on genuine tradeoffs. Avoid repeatedly asking for consequences
already established. Prefer low overhead when unspecified; preserve freedom over internal
algorithms, module boundaries, storage and rendering.

Approved requirements live in [product experience](../../specs/behavior/experience.md),
[resources](../../specs/behavior/resources.md), [agent interaction](../../specs/behavior/agents.md),
[operations](../../specs/behavior/operations.md), and [current-state ownership](../../specs/behavior/current-state-and-execution.md).
They describe intent, with incomplete implementation coverage. Existing behavior/wire owners
remain indexed in [the specification index](../../specs/README.md).

## Next three proposals — unanswered

Resume with the three [recovery and structured-outcome proposals](../../specs/proposals/api-ideas.md#recovery-and-structured-outcomes--unanswered).
They remain unapproved. The proposal file owns their wording; do not treat this handoff as
another requirements source or reopen already approved machine-information parity.

## Remaining meaningful choices

- Retention: what constitutes a distinct version when the same content recurs? Does cleanup
  target local bytes only or optionally remote copies? How are shared bytes counted and
  protected? When does retroactive cleanup run without adding startup latency or long locks?
- Freshness failure: what is the bounded direct-check wait, and how does a caller explicitly
  accept a possibly stale alternative? How are unavailable artifacts distinguished from
  present-but-possibly-stale data? Concurrent retries should share checks.
- Lifecycle: default drain/cancellation deadlines, diagnostics for uncooperative children,
  and recovery when a prior owner or remote worker may still be alive. Automatic asset
  recovery must not duplicate ownership. Client wait deadlines differ from job deadlines.
- Invocation: how callers obtain/reuse an ID before acknowledgement; how explicit retry is
  distinguished from response recovery; how long deduplication records live. Target and
  arguments stay fixed; each new attempt captures current code/configuration.
- Snapshots: verify the existing runner implementation before claiming it freezes all
  project imports/configuration; define the environment/dependency boundary and what identity
  is recorded without prescribing a packaging mechanism.
- Resources: aggregate memory accounting and CPU burst intervals; effective enforcement
  diagnostics across supported platforms; temporary disk and file/connection pressure.
  Best-effort fallback with a clear limitation is already approved.
- Navigation: default page sizes, consistency while state changes, bounded round trips and
  exact log/event reconnection guarantees beyond Docker-style tail/follow/since.
- Remote lifecycle: consequences of lost connectivity, eventual cancellation acknowledgement,
  and local/server parity without pretending an unreachable remote job has stopped.

## Implementation follow-up, not more product confirmation

The [architecture map and cleanup sequence](architecture-cleanup.md) now traces existing
owners and proposes a first behavior-preserving bookkeeping refactor, followed by separately
gated publication, input lifetime, sharing and bounded-work changes. No runtime refactor has
shipped in this documentation branch. Use it to start implementation without reopening
approved product decisions; the unanswered proposals above remain unanswered.

Map approved intent to existing behavior and gaps before implementing. In particular, check
full code/config snapshots, project-wide resource ownership, surviving-worker reconciliation,
process-tree cleanup, stable IDs/attempts and retained-history availability. Add conformance
with each actual public behavior change and update the existing CLI/HTTP/manual owners.
Do not present this documentation push as implementation or performance verification.

Known audit work is tracked separately in issues #654 (overlapping asset ownership), #655
(cold/full-domain memory), #656 (inspection cost), and #657 (historical selection), with
[PR #658](https://github.com/barca-orc/barca/pull/658) addressing the last issue. Recheck live
status before resuming; this list is a handoff, not a claim that those issues stay open.
