---
title: Releases
description: How a barca release is cut, what the release workflow builds and publishes, and where release notes are.
---

Merging to `main` publishes nothing. A release is published when a `v*` tag is pushed. This
page describes the steps as they were done for 0.17.0, 0.17.1 and 0.18.0.

## Version numbers

Barca is pre-1.0 and stays on `0.x.y`.

- **Minor** (`0.x.0`): a new area of functionality, new public surface, or a breaking change.
- **Patch** (`0.x.y`): fixes, performance work and refactors within what exists.

The version is written in several files that must agree. `scripts/check-version-sync.sh`
compares the three crate manifests, `pyproject.toml` and `python/barca/__init__.py`, and runs as
a pre-commit hook.

## Cutting a release

1. Branch `release/v<major>.<minor>.<patch>` off `main`. The `release/` prefix keeps the branch
   name from colliding with the tag.
2. Commit only the version bump. For 0.18.0 that was nine files:

   ```
   Cargo.lock
   README.md
   crates/barca-cli/Cargo.toml
   crates/barca-core/Cargo.toml
   crates/barca-server/Cargo.toml
   pyproject.toml
   python/barca/__init__.py
   site/src/content/docs/reference/server-api.md
   uv.lock
   ```

   `README.md` and `server-api.md` are there because they quote the version in example output.
3. Rename `## Pending` in `CHANGELOG.md` to `## <x.y.z> (<date>)` in the same commit. Open a
   pull request into `main` titled `release: v<x.y.z>`.
4. When CI passes, squash-merge and delete the branch.
5. Create an annotated tag `v<x.y.z>` on the merge commit and push it:

   ```bash
   git tag -a v0.18.0 -m "v0.18.0" <merge-commit>
   git push origin v0.18.0
   ```

   Pushing the tag publishes to PyPI. It cannot be undone: a version number on PyPI cannot be
   reused.

## What the release workflow does

`.github/workflows/release.yml` runs on a `v*` tag:

1. **Builds the web UI** (`pnpm --dir ui install`, `test`, `build`) and passes `ui/dist` to the
   later jobs, so the binary embeds it.
2. **Builds wheels** with maturin for two targets: Linux x86_64 (manylinux) and macOS arm64.
   Each wheel contains the `barca` binary and the Python package.
3. **Packages a standalone binary** for the same two targets as `barca-linux-x86_64.tar.gz` and
   `barca-macos-arm64.tar.gz`, after checking that the binary contains the web UI.
4. **Builds an sdist** (`barca-<version>.tar.gz`), which carries the built UI so a source build
   needs no Node.
5. **Creates the GitHub Release** with those five files. The body is the version's
   `## <version> (<date>)` section of `CHANGELOG.md`
   (`python3 scripts/changelog-section.py <version> --install`), followed by an install line. A
   `release-notes` job extracts it alongside the builds, and the GitHub release and the PyPI
   upload both wait on it, so a version with no section publishes nothing. A tag containing
   `rc`, `alpha`, `beta` or `dev` is marked as a pre-release.
6. **Publishes the wheels and the sdist to PyPI.**
7. **Deploys the docs site**, [barca.sh](https://barca.sh), from the tag: a `deploy-site` job
   runs `npm ci`, `npm run build` and `npx wrangler deploy` in `site/` (Node 24; the Worker is
   configured in `site/wrangler.jsonc`). It starts after the GitHub Release and the PyPI upload
   and nothing waits on it, so a failed deploy leaves the release published; rerun the job once
   the cause is fixed. It needs two repository secrets, `CLOUDFLARE_API_TOKEN` (a Cloudflare
   API token from the "Edit Cloudflare Workers" template) and `CLOUDFLARE_ACCOUNT_ID`, and
   fails with an error naming whichever is missing.

There are no wheels for Windows, Linux arm64 or macOS x86_64. On those platforms `pip` builds
from the sdist, which needs a Rust toolchain.

The workflow also runs on a pull request that changes `release.yml`; it then builds everything,
checks that `CHANGELOG.md` has a section for the version in `pyproject.toml`, and skips the
release, the PyPI upload and the site deploy.

To deploy the site by hand, for example to publish a docs fix between releases, run
`cd site && npm ci && npm run deploy` with the same two variables set, or after
`npx wrangler login`.

## Release notes and the changelog

Release notes are written in `CHANGELOG.md`, one bullet per change under `## Pending`, and the
release workflow publishes each version's section as its
[GitHub release](https://github.com/barca-orc/barca/releases). See
[Changelog](/contributing/changelog/). Commits follow the
[conventional commits](https://www.conventionalcommits.org/) form (`feat:`, `fix:`,
`refactor:`, `polish:`, `doc:`, `test:`, `perf:`, `remove:`).

## Compatibility between releases

Before 1.0 a release may break the command line, with a minor bump and a "Breaking" line in
the release notes. The [CLI contract](/reference/cli-contract/) lists what is stable and what
is experimental, and the policy from 1.0 on.
