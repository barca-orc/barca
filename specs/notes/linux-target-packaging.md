# S07: Linux arm64 and Alpine package support

Status: technical plan before implementation. Refs #107, #344 S07.
Baseline: published v0.21.0 at 52e286c has Linux x86_64 manylinux and macOS arm64.
No additional target is advertised as shipped until its release artifacts and
fresh published installation pass. This plan changes packaging/CI only.

## Target matrix and bounded ownership

Retain x86_64-unknown-linux-gnu (manylinux2014 / glibc 2.17) and
aarch64-apple-darwin. Add aarch64-unknown-linux-gnu (manylinux2014 / glibc 2.17),
x86_64-unknown-linux-musl and aarch64-unknown-linux-musl (musllinux_1_2).
Use the existing Depot x86_64 runner and its documented native
`depot-ubuntu-24.04-arm` counterpart. Native architecture execution avoids an
emulation-only support claim. Build GNU with the existing maturin action's native
manylinux container. For musl, use explicit PyPA musllinux_1_2 build containers
matching the runner architecture; verify available toolchain and install only
build-time Rust/maturin prerequisites if the image does not supply them.
All targets keep locked production dependencies and embed the same UI artifact.
Do not replace Turso, worker IPC, hashing or package APIs to make targets compile.
A dependency incompatibility must be reported with the exact compiler/runtime
failure before choosing an architectural change.

The release workflow owns target entries, cache keys and existing wheel/native
artifact uploads. One small stdlib smoke script owns the shared runtime contract;
no target-specific implementation variants. Distinct target triples are cache
keys and native archive names, including libc, to prevent collisions. Prefer two
reviewable slices if musl toolchain setup differs materially: native GNU arm64
with the reusable smoke first, then both musl architectures. Neither slice closes
#107 until the full matrix is built, published and verified.

## Real installation and runtime evidence

Every Linux build must run its wheel in a minimal matching runtime container:
Python slim for GNU, Python Alpine for musl, at the runner's native architecture.
Install the locally built wheel using uv with `--only-binary :all:` so a missing
or incompatible wheel cannot fall back to a Rust source build. A clean uv project
must `uv add` the actual wheel, then run installed `barca get` on a two-node
pipeline. Assert version matches package metadata, exact output 42, cold execution
2 and warm execution 0 with stable run hashes, and an import-free dry-run. Verify
installed source is the wheel rather than checkout PYTHONPATH. Inspect the runtime
for absent cargo/rustc; no daemon or service is started. Also invoke the separately
packaged native binary against the same Python environment and execute a pipeline,
then verify server health and actual embedded UI assets with bounded startup and
termination. GNU and musl runtime containers must have no compiler toolchain.

Keep this contract executable without network access after uv and the wheel have
been copied in. Container limits and command timeouts must terminate failed tests,
remove only their own containers/temp projects, and preserve useful stdout/stderr.
Do not accept `--version` as the sole architecture/runtime check. Release and PyPI
publish jobs depend on all target builds AND runtime smoke success; dry-run PR and
workflow_dispatch runs never publish. No missing matrix cell is silently skipped.

## Compatibility, documentation and acceptance

No user configuration, command shape, interpreter requirement, version bump or
runtime dependency changes. Document the explicit libc/architecture matrix in
installation/deployment guidance, daemonless uv installation and existing amd64
fallback. Retain fallback guidance until the target is actually published; a
successful PR dry-run is build evidence, not a shipped package. Optional SQL/remote
extras retain their separate dependency platform support and are not promised by
the stdlib-only core wheel smoke. Future tags publish the added assets through the
existing authorized release workflow. Verify fresh PyPI wheel-only installs on
both native architectures and both libcs after that release before closing #107.

## Investigation evidence and implementation sequence

1. Read issue107 and current release workflow. Published v0.21.0 confirms only
   manylinux x86_64 and macOS arm64 wheels. Local host is x86_64 with Docker and no
   registered arm64 binfmt interpreter; local arm execution cannot prove support.
2. Official Depot documentation lists native `depot-ubuntu-24.04-arm` runners;
   repository already uses its x86_64/macOS runners. Exact repository scheduling
   access remains a CI feasibility gate, not an assumption that a label works.
3. Maturin official distribution documentation supports bin cross targets and
   audited manylinux/musllinux tags; maturin-action documents native aarch64
   manylinux2014 containers. Check explicit musllinux images and toolchain/linking
   locally on x86_64 before changing the release matrix.
4. Implement shared installed-artifact smoke plus native GNU arm64 matrix entry;
   execute actual native CI runtime and all existing release targets.
5. Add both musl targets only after x86_64 toolchain/runtime proof and native arm CI
   feasibility. Preserve old targets and source-distribution build.
6. Review full workflow diff and artifact names, locked dependency equality, YAML
   validation, smoke behavior and both required CI jobs. Publish through a normal
   version-only release, then verify official PyPI installs and native archives.

References: https://depot.dev/docs/github-actions/runner-types;
https://www.maturin.rs/distribution.html;
https://github.com/PyO3/maturin-action .

## First implementation slice

This PR adds the native GNU arm64 release matrix entry and the shared Linux
installed-artifact/runtime smoke. Musl is intentionally a following slice: the
actual PyPA musllinux_1_2 x86_64 image reports musl 1.2.5 and GCC 14.2 but contains
no cargo, rustc or maturin executable. Its build-toolchain installation needs
separate proof, rather than pretending the existing manylinux action covers it.
The shared script is validated locally against the published v0.21.0 x86_64 wheel
and native archive in a clean Python slim container; ARM evidence comes from the
new native CI job. Publication still uses only normal version tags.
