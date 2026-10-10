# S07 publication acceptance: four Linux runtime targets

Status: scoped technical plan before implementation. Depends on #372/#375;
refs #107 and specs/linux-target-packaging.md. Existing prepublication native
build/runtime gates stay intact. No public API/config/dependency/version change.

## Why a publication check is separate

Both native GNU and musl architectures have passed actual build/runtime CI, but
that proves CI artifacts, not official PyPI wheel selection or the published
GitHub native archive. Local x86 verification cannot establish ARM publication.
During v0.21.0 acceptance, PyPI JSON already listed the uploaded version while
the official simple index still omitted it briefly. Availability checks must
bound that observed propagation rather than blindly rerun a failed installer.

## Smallest delivery boundary

Add one tag-only postpublication job to the existing release workflow, dependent
on both successful PyPI publication and GitHub Release creation. Four native
Linux cells are the Cartesian product of existing x86/ARM Depot runners and
Python slim/Alpine runtimes. Assert actual host/container machine identity.
Each job reads the exact tagged checkout version and validates its tag identity,
downloads the matching native archive from that actual GitHub release, then
uv-adds the exact version from official https://pypi.org/simple with no cache,
local wheel source or source builds. The compiler-free runtime reuses the exact
shared smoke: installed-source prefix/version, preview, cold/warm pipeline and
hashes, real published native executable, health and actual embedded UI assets.
Do not substitute a rebuilt binary or CI wheel. GNU/musl native archive names
are the existing architecture/libc names; no public target selector vocabulary.

Use a small stdlib availability helper with fixed bounded polling for official
PyPI version JSON and the simple index's exact relevant wheel filename. Refuse
yanked files and wrong architecture/libc; install only after the expected wheel
is actually listed in both official surfaces. Each request has a timeout and
overall propagation wait is bounded. Read-only transient404/network failures
can be retried within that boundary, but forbidden/auth/invalid metadata errors
fail visibly. The actual uv installer runs once after availability; an install
or pipeline failure is not blindly retried or converted into success.
Job/command deadlines bound download/runtime failures, and existing finally
cleanup terminates the owned server and --rm containers.

## Failure semantics and acceptance

Postpublish failure is visible as a failed release-verification job/workflow.
Assets may already be uploaded, so this cannot undo publication and must never
retag, delete/yank assets, or silently call the release verified. Release notes
and #107 closure wait for successful acceptance, or name the precise unresolved
platform failure. PR/dry-run executions never publish or run tag-only official
install checks. Validate workflow syntax/labels and helper/runtime logic using
the actually published v0.21.0 GNU x86 wheel/native assets locally; exercise
propagation/yanked/missing-platform/deadline cases deterministically without
external writes. Native ARM and both musl official installs are proved by the
subsequent normal tag, not by an existing version with no matching wheels.
