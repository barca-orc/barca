---
title: Changelog
description: Where to find what changed in each barca release.
---

[`CHANGELOG.md`](https://github.com/barca-orc/barca/blob/main/CHANGELOG.md) is the source of
barca's release notes. Each release has a `## <version> (<date>)` section with `### Breaking`,
`### Features`, `### Bug Fixes`, `### Performance`, `### Removed` and `### Documentation`
subsections, and the release workflow publishes that section as the version's
[GitHub release](https://github.com/barca-orc/barca/releases) body, followed by an install line.

A pull request that changes behaviour adds one self-contained bullet under `## Pending` at the
top of the file; a breaking change starts its bullet with `**Breaking:**` and names the
replacement. The release branch renames `## Pending` to the version and date. To preview a
release body:

```bash
python3 scripts/changelog-section.py 0.26.1 --install
```

Releases 0.1.2 to 0.23.x have their notes only on GitHub Releases.

## Behavior changes noted in the docs

Changes that affect how you call barca are also noted where the feature is documented, as
"Behavior change" notes in the [CLI reference](/reference/cli/) and in the manual
(`barca docs`). Run hashes that change on upgrade, which cause a one-time recompute, are
listed in `CHANGELOG.md` under the release that changed them (for example 0.19.0).
