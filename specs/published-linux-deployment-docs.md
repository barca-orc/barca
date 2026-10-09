# Published Linux deployment documentation (#107)

## Scope and publication gate

Prepare this docs-only follow-up on the reviewed packaging stack (#372 → #375 →
#379). Keep it local until the release owner confirms the official v0.22.0 tag's
four native Linux published-artifact checks. Do not infer publication from CI
builds or replace an unavailable official artifact with a worktree build.

After that acceptance, document the core GNU/musl × x86_64/aarch64 wheel and native
archive matrix verified on native runners. Update the main deployment recipe to
Python 3.13 Debian slim, remove forced amd64 for v0.22.0, and show Alpine as a
native core deployment alternative. Preserve the explicit v0.21.0 Debian/amd64
fallback and earlier shutdown compatibility notes. Core package verification does
not establish availability of a pipeline's third-party dependencies or optional
SQL/parquet/cloud extras on every platform; state that boundary explicitly.

No runtime, API, config, workflow, dependency or release-version changes. Limit
product documentation changes to the deployment page. Strip merged packaging
prerequisites and integrate against actual current main at admission.

## Verification

- Build the complete documentation site and check whitespace.
- Review commands/version/matrix against the actual accepted tag and its official
  verification logs before publication; record that evidence in the PR body.
- Preserve the historical v0.21.0 fallback, current health response shape, native
  image selection and core-versus-extra distinction.
- Require both fresh normal CI gates at final current-main admission. No merge or
  #107 closure until the release owner confirms official artifact acceptance.
