# Real process interruption against artifact emulators

Status: scoped plan and source inventory, before implementation. Owner: S02 / #248
in #344. Baseline: main 748479f (2026-10-09). This is test coverage of the current
artifact visibility contract, not a new recovery policy.

## Remaining invariant

A real coordinator killed during a refresh or a verified download must never make
an incomplete object or staged local file an accepted input. A later machine can
read a completed overwritten artifact under the existing hash-mismatch policy;
it must warn rather than pretend the old recorded SHA matches. A subsequent run
must recover, and unrelated history remains available.

The canonical issue distinguishes a real killed process against MinIO,
fake-gcs-server and Azurite from an injected ConnectionResetError. Most of the
original checklist already has coverage. Do not add another directory-store
kill matrix or duplicate metadata-swap kill tests.

## Existing coverage inventory

| Requirement / boundary | Existing evidence | What it actually proves |
| --- | --- | --- |
| Backend local SHA match, stale copy, changed remote bytes | `python/tests/test_remote_verify_backends.py`: `test_matching_local_copy_is_kept_without_a_download`, `test_stale_local_copy_is_replaced_by_the_stores`, `test_store_copy_with_other_bytes_is_used_and_flagged`, each on S3/GCS/Azure | Real SDK reads/uploads against emulators; matching local copy skips download; completed mismatched bytes are flagged. |
| Interrupted backend download | Same file: `test_a_download_that_dies_midway_leaves_the_local_copy_and_no_temp_file` | Calls the real download, then truncates the stage and raises injected ConnectionResetError. Exception cleanup passes; no real process dies. |
| Reader after overwritten remote object | Same file: `test_an_overwritten_object_is_used_and_flagged_in_the_json_result` | Real CLI and SDK overwrite, downstream result/warnings/mismatch plus repair on refresh. It models a killed refresh's aftermath but never kills the producer. |
| Real killed refresh after upload | `python/tests/test_remote_verify.py::test_a_refresh_killed_after_its_upload_does_not_lock_the_step` | Actual coordinator kill against a directory store; fixed five-second delay does not prove exact upload timing; restart succeeds. Not emulator coverage. |
| Coordinator killed at transfer/state boundaries | `python/tests/test_remote_cancel.py::test_helpers_exit_on_their_own_when_barca_is_killed` (`upload`, `push`, `fetch`, `pull`) | Marker-controlled real kill, helper lifeline cleanup, no partial directory object/local temp, subsequent run and shared-history recovery. Existing directory-store coverage; retain it. |
| Helper signals, staging, coordinator EOF | `python/tests/test_transfer.py::TestSignals`, `TestStaging`, `TestCoordinatorGone` | Helper SIGTERM/closed socket/stdin cleanup and staging race; directory/memory clients, not emulator refresh execution. |
| Two processes fetching one path | `python/tests/test_transfer.py::TestCrossProcess::test_two_processes_fetching_the_same_local_path_both_succeed` | Two real helpers stage concurrently and install complete copies atomically; each owns its own temp. No duplicate test needed. |
| Mixed lazy/eager readers | `crates/barca-core/src/dispatch.rs` tests `readers_of_a_partitioned_upstream_name_it_by_base_id_so_one_eager_reader_wins` and mixed phase cases; `python/tests/test_remote_lazy_local_first.py::test_one_eager_reader_in_the_phase_downloads_it_for_both` | Base-ID partition matching unit checks and actual S3 phase with one eager reader force one local copy for both. No new runtime policy. |
| Schedule catch-up across downtime | `python/tests/test_serve_catchup.py::test_a_missed_tick_runs_once_when_the_server_comes_back` | Actual stopped/restarted server reconciles a missed tick. |
| TTY fetch/warning output | `python/tests/test_remote_verify.py::test_fetch_and_warning_lines_reach_a_terminal_through_the_progress_bar` | Actual pseudo-terminal progress path preserves fetch/warning messages. |
| Large-file verification cost | `benchmarks/transfer_hash/bench.py` | Existing parameterized warm SHA/keep-local benchmark and optional `--max-seconds-per-gb`; no automatic CI performance threshold is selected here. |
| Metadata pull/swap and killed-run carry | Rust `db` swap/interruption tests; `python/tests/test_state_pull.py`, `test_state_validation.py` | Separate authoritative-history integrity/recovery ownership; does not substitute for artifact-client interruption. No duplication. |

This inventory is based on test source, not a claim that the entire suite has
been rerun for this plan. Required backend CI starts the three pinned emulators
and runs all Python tests with `BARCA_TEST_*`; `emulators.require` turns declared
unreachable emulators into failures. Existing local containers are available on
19100 (S3), 19200 (GCS), 19210 (Azurite); tests must use these explicit endpoint
overrides rather than change shared container configuration.

## One bounded implementation PR

1. Reuse the existing per-backend setup, isolated machine roots, unique buckets,
   real CLI entry point and `python/tests/hold/sitecustomize.py` test-only shim.
   No package hook, CLI option, public API or production dependency changes.
2. Add test-only markers scoped to artifact helper operations:
   - **Before upload**: existing `put` gate, after local materialization but before
     the real SDK upload. Kill the coordinator and prove the old remote object is
     still complete and readable by another machine.
   - **Confirmed upload before receipt**: invoke the real SDK `put_file`, then
     hold before returning to `_transfer`/sending Done. Kill the coordinator;
     independently read the object and require exact complete new bytes. Shared
     metadata still describes the prior run. A fresh reader must report the
     recorded-hash mismatch and produce the complete new value; refresh repairs
     the mismatch using current behavior.
   - **Partial local SDK download**: gate only the concrete staged destination's
     first write. Write and flush the first bounded fragment of actual bytes
     received from the real SDK, publish the marker, then await release before
     completing that write. The shim delegates all other file operations and
     all network/storage calls unchanged. This avoids synthesizing an exception,
     truncating a completed file or relying on inconsistent SDK progress callback
     timing. Seed a complete stale local destination whose SHA differs from the recorded
     expected SHA, forcing the real download rather than the keep-local fast
     path. Require a nonempty stage smaller than the actual complete object
     before killing the coordinator. The old local destination must remain exact;
     no reader may consume the staged path. Lifeline cleanup removes the stage,
     and a normal rerun installs the complete object.
3. Parameterize the three windows over S3/GCS/Azure (nine concrete cases). Refresh
   fixtures reuse nondeterministic output under a stable source/run hash so a
   genuine overwrite is observable. Keep payload bounded, with exact old/new
   byte hashes and distinct values. Gate upload and download separately to avoid
   treating an unconfirmed artifact as a persisted success.
4. Wait for a marker and validate its disk/object state before sending SIGKILL;
   no fixed delay stands in for transfer progress. Start runs in isolated process
   groups. Kill only the coordinator first, wait boundedly for transfer-helper
   lifeline exit, then clean remaining workers in `finally`. Failure cleanup must
   never signal an unrelated process or leave a child pipe unread.
5. Assert three boundaries in every relevant case: visibility while held,
   visibility after kill, and successful next reader/run. Query the emulator
   through its actual client to verify exact complete object bytes. Preserve
   unrelated baseline history. Do not assert that SIGKILL executes a Python
   finally block or promises rollback of a remotely confirmed overwrite.
6. Add the tests to existing automatically collected backend coverage; no new CI
   job or real-provider credentials. Revise the backend suite's introduction to
   distinguish exception injection from the new real process interruption cases.

## Verification and completion

First run the new test-only shim regression against a directory store, checking
its marker/partial-write and cleanup mechanics without duplicating the existing
full cancellation matrix. Then execute all nine new actual CLI cases against the
three declared local emulators, retaining backend IDs and failures in output.
Run existing backend verification and focused transfer staging/lifeline tests to
check shim compatibility. Run pinned Ruff, formatting and diff checks. Build only
this worktree's binary with its own cargo target; no full workspace rebuild is
required by a Python-test-only change. Required backend CI must execute all nine
cases, with no emulator skips, before #248 is claimed complete.

If an actual new integrity defect is exposed, record the reproduction and split
its production fix from this coverage slice for review. Hardware power-loss
fsync durability, real-provider multipart/version semantics (#86), global
artifact immutability/download elision (#297), terminal-history durability (#319)
and minute publication (#214) remain separate. The benchmark already exposes an
optional threshold; selecting an automatic CI performance gate is an independent
measurement decision and is not required to close the current canonical #248
scope.

## Prepared implementation and execution evidence

Implementation prepared on main ff91122, following this committed plan. The
child-only hold shim adds `uploaded` and `partial-get`; production package code
is unchanged. The partial-write gate selects one concrete destination from the
artifact helper's first get, flushes at most 64 KiB of actual SDK bytes, and
preserves the original write count on release. A concurrent unrelated download
and metadata-file write proceed without being intercepted. Fresh child clients
perform object verification because fsspec captures environment configuration
when first imported; inherited cloud options are scrubbed by the backend fixture.

All nine marker-controlled real coordinator SIGKILL cases pass against explicitly
declared MinIO/fake-gcs/Azurite endpoints, with no skipped backends. The child-only
normal-release/scoping regression also passes (10 cases in 39 seconds). Existing
backend SHA/mismatch/injected-failure coverage, transfer signals/staging/lifeline
and concurrent-fetch tests, and the four directory-store coordinator-kill cases
pass (31 cases). These are 41 distinct checks; the standalone initial shim
regression rerun is not counted again. Pinned Ruff 0.11.13 check/format and
whitespace checks pass. The correct checkout binary was built in this worktree's
dedicated target, never another worktree's shared target.

No new runtime integrity defect appeared. Keep #248 open until required backend
CI executes the nine new cases and the inventory is accepted as its completion
evidence. This patch leaves the existing optional benchmark threshold unchanged;
an automatic CI performance gate and real-cloud-provider acceptance remain
separate from the canonical real-process-interruption scope.
