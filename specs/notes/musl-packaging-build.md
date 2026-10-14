# S07 second slice: native Alpine/musl packages

Status: technical plan recorded before workflow changes. Refs #107, #372,
specs/notes/linux-target-packaging.md. This branch builds on PR372's GNU arm64 matrix
and shared compiler-free installed-artifact smoke. It cannot merge until that
prerequisite is merged and this slice is freshly integrated onto current main.

## Concrete feasibility evidence

The native PyPA musllinux_1_2_x86_64 image supplies musl1.2.5, GCC14.2 and Python
interpreters but no Rust/maturin. Installing the stable native x86_64-musl Rust
toolchain and maturin, then `maturin build --release --locked --target
x86_64-unknown-linux-musl --compatibility musllinux_1_2` compiled the unchanged
production Cargo.lock and produced an audited wheel in6m26s. A clean Python3.13
Alpine image uv-added that wheel with no index and source builds disabled. Both
wheel CLI and the separate native binary passed installed-source-prefix and
absent-C/Rust-compiler assertions, import-free preview, exact cold/warm42 with
execution2/0 and equal hashes, health, fetched embedded UI and clean shutdown.
Logs: /tmp/barca107-musl-build.log and /tmp/barca107-musl-runtime.log.
This is local x86_64 feasibility, not a published musl package or ARM proof.

## Smallest implementation

Extend the existing release matrix with x86_64-unknown-linux-musl and
aarch64-unknown-linux-musl, using matching native Depot runner architectures.
Keep every existing GNU/macOS entry, UI, sdist and publication boundary. Pass
explicit native PyPA musllinux_1_2 containers and musllinux_1_2 compatibility
through the existing maturin-action. Its current primary-source implementation
installs Rust/maturin in custom containers, so no bespoke toolchain framework or
new build image is needed. The musl runtime image is Python3.13 Alpine; GNU
continues to use Python slim. Reuse PR372's exact smoke and compiler refusal.
Native archive names include libc and architecture; target-specific cache keys
already prevent target collisions. Build with the existing locked dependencies.
No alternate Turso/worker/runtime implementation, compiler fallback or new API.

## Failure, cancellation and publication

Keep fail-fast false to collect independent target evidence, but require every
wheel/runtime job to succeed before either existing publication job proceeds.
Use the existing bounded smoke step and finally-owned server termination.
Runtime failure reports stdout/stderr and fails the target rather than uploading
an unsupported artifact. PR/dry-run jobs do not publish. If native ARM musl
compilation/runtime fails, capture its concrete error and fix a bounded platform
bug separately; do not call cross-compilation alone support. No conditionally
omitted target, unverified PyPI tag or silent dependency upgrade.

## Compatibility and verification

Package/command/result/version contracts stay unchanged. No release version bump
is included. Retain current v0.21.0 amd64 fallback and name targets as build/test
evidence until a normal tagged release publishes them. Optional SQL/parquet/
remote extras have independent platform availability; this guarantees only the
stdlib-based core wheel. Validate YAML with exact Depot labels, Ruff/format,
unchanged lock files, both required CI and all five release-target jobs. Native
ARM CI must build the audited musl wheel and execute both installed artifacts in
Alpine. After the subsequent release, fresh official-PyPI wheel-only installs
on GNU/musl x86_64/ARM64 and native artifact smoke are the closure gate for #107.
