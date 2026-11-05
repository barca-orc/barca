## Summary

<!-- What changed and why. -->

## Testing

<!-- Commands run and results (`scripts/check-fast.sh` runs CI's fast and merge gates locally). -->

## Checklist

<!-- See CLAUDE.md: "Commit messages" (the CHANGELOG) and "CLI and manual are part of every feature". -->

- [ ] `CHANGELOG.md`: one bullet under `## Pending` (`### Features`, `### Bug Fixes`, `### Performance`, `### Documentation`, ...), or N/A: nothing user-visible changed
- [ ] `--help` and `--examples` updated for any changed command or flag (`crates/barca-cli/src/args.rs`)
- [ ] `barca docs` topic updated or added (`crates/barca-cli/docs/`)
- [ ] CLI contract: `scripts/update-cli-snapshots.sh` run and its diff reviewed for any surface change (a command, flag, environment variable, exit code, JSON key or `--agent` line): `crates/barca-cli/snapshots/`, `python/tests/snapshots/cli_contract/`, `crates/barca-cli/docs/contract.md`
- [ ] Output contract kept: JSON on stdout (`--json` for inspection commands), progress and errors on stderr
- [ ] Site docs (`site/src/content/docs/`) and the README CLI table updated
- [ ] N/A: no user-facing CLI, decorator, output, or caching change
