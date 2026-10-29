# `origin/feature/cli-app` branch audit (2026-10-29)

Branch: remote only, last commit `986aff6` (2026-03-25), 19 ahead / 18 behind `main`, forked at `06b9a92`.
It is a Rust + ratatui rewrite (~21k lines added) that deletes the Python `envira/` package.
The user judged it low quality; the redesign is in `.omo/plans/envira-rewrite-design.md`. Do not build on it.

## Verified facts (built in a throwaway worktree)
- `cargo build` and `cargo test` pass; `git merge-tree` against main shows no textual conflicts.
- Catalog: `src/catalog/manifest.toml`, 76 items / 16 bundles, sudo and user recipes per distro.
- **User-mode planning is broken**: as a non-root user, `envira plan` (also `--item uv|zoxide|oh-my-zsh|docker`)
  fails with `item essentials with catalog scope System is not supported in runtime scope User`.
  33 items depend on `essentials`, which only has sudo recipes; the planner (`src/planner/mod.rs`,
  `UnsupportedScope`) errors instead of accepting an already-present dependency. `verify` on the same
  machine reports `essentials` healthy.
- **Release pipeline removed**: `release.yaml` deleted; nothing builds the Rust binary or produces the
  `executable` artifact / `envira.sha256` that `release_web.yaml` and the new `envira.sh` expect.
- **Catalog drift**: none of main's changes since March are in the manifest (herdr, texlab, Infisical,
  GitKraken removal, oh-my-pi entrypoint, btop 1.4.7, Ubuntu 26 fixes, 2026-08 deprecation audit).
- TUI (`src/tui.rs`): single-threaded `event::read()` loop, so installs block the UI with no live output;
  lists are hand-built `Paragraph` text, no `List`/`Table` state, no search.
- CI leftovers: `.sisyphus/evidence/task-11-*` artifacts uploaded by `test.yaml`.

## Worth reusing
- `src/platform/mod.rs` invocation detection (`SUDO_USER` -> target user home) as a reference only.

## Shell gotcha seen while auditing (zsh)
- `git show $B:path` in zsh applies the `:s` history modifier to `$B`; quote as `"${B}:path"`.
- `echo =====` fails in zsh (`=word` expansion); quote it.
