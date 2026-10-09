# Remote startup preflight (#308, P04)

The shared execution path must check configured artifact storage before starting
Python workers, including state-off and serve runs. Reuse the transfer helper's
read-only probe; do not introduce another authentication implementation or flag.
Bound the probe to min(10 seconds, transfer_timeout), honour cancellation, and
abort the helper before returning an infrastructure error. Record an already
created run as failed (or cancelled), with zero executed steps.

Cloud probes list the bucket/container, reusing missing-cache recovery semantics.
A directory store may not exist on its first run: probe its nearest existing
ancestor without creating it. This verifies reachability, not write permission;
uploads still determine whether results may be recorded. URI credentials and
storage-option secrets must be removed from diagnostic text.

Before draining uploads print the count, sanitized store and per-attempt timeout.
While waiting emit a bounded periodic progress line; failures name the artifact,
store and attempts. Keep machine stdout and public APIs unchanged.

Evidence: real CLI and serve runs using denied/stalled probe shims must execute no
user code, return infra diagnostics (CLI exit 3), terminate within the bound and
leave failed durable runs. A delayed upload must show waiting before completion;
a failed upload must report attempts. Existing transfer and remote regressions
must retain first-run directory-store behavior and cancellation cleanup.

## Selected policy — user delegated the simpler choice

Reuse the existing read-only bucket/container probe for startup. It is the
smallest implementation and avoids new modes, flags, sentinel objects or
provider-specific authentication classifiers. A configured remote artifact
store must permit listing its bucket/container and the backend's existence
check. This is intentionally stronger than read/write access to known objects
or prefix-only listing; deployments with those narrower permissions must grant
store listing before upgrading. There is no claim that listing proves writes
will succeed. Shared-history pull keeps its existing timing and ordering; the
10-second limit is specifically the artifact probe, not the entire startup.

| Existing access | Selected startup behavior |
| --- | --- |
| Read/write/list whole bucket/container | probe succeeds when reachable |
| Read/write/list configured prefix only | fails if root listing is denied |
| Write objects, no list | fails before user code |
| Read known objects, no list | fails before user code |
| Invalid credentials or denied listing | infrastructure failure before workers |
| Stalled store | bounded infrastructure failure before workers |
| Empty/nonexistent artifact prefix in reachable bucket | accepted; probe checks bucket/container |
| New directory store | check nearest existing ancestor; create nothing during probe |

Permissions follow the existing backend probe: S3 `s3:ListBucket`; GCS
`storage.objects.list`; Azure permission to list blobs (Storage Blob Data Reader
or Contributor). The helper also calls the backend's existence check after a
successful listing, which may require bucket/container metadata access. Prefix
conditions restricting the bucket-root listing do not satisfy this probe.

## Verification

- Rebased onto main `851c576`: workspace Rust tests, 771 passed.
- Workspace Clippy: passed with warnings denied.
- CLI/manual/serve/preflight/config/transfer Python tests: 158 passed.
- Remote staging/verification/inspection/lazy/config tests with local emulators: 60 passed.
- Real S3/Azure/GCS transfer fault tests: 30 passed; 1 expected skip because GCS emulator does not authenticate.
- First Rust run exhausted tmpfs quota; repeat with task-local TMPDIR passed.
- Initial CLI contract run lacked pandas; repeat after installing dependency passed.

The new CLI/serve auth and timeout regressions inject faults at the storage
boundary while using the real executable and coordinator/helper protocol.
Pinned MinIO, Azurite and fake-gcs-server emulators were then started locally.
Actual emulator credential failures, missing/denied store access, stalled stores
and transfer retries passed. Transfer reset injection waits for healthy preflight
before arming faults, so it exercises uploads/fetches rather than the startup
check. No claim of live-cloud credential testing is made.
