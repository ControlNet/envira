# envira self-update: how it works and how to test it

Implemented 2026-11-05 (`src/selfupdate.rs`). Design decisions are in
`.omo/plans/envira-rewrite-design.md` under "Command line".

## Install channel detection

From the canonical path of the running binary (`current_exe()` + `canonicalize`):

| Path | Channel | Update |
|---|---|---|
| `…/target/{debug,release}/envira` (or `target/<triple>/release`) | source build | refused |
| contains `/pipx/venvs/envira/` | pipx | `pipx upgrade envira`, pinned `pipx install --force envira==v` |
| contains `/uv/tools/envira/` | uv tool | `uv tool upgrade envira`, pinned `uv tool install --force envira==v` |
| `$CARGO_HOME/bin` (default `~/.cargo/bin`) | cargo | `cargo binstall -y --force envira[@v]`, else `cargo install --locked --force [--version v] envira` |
| `<root>/bin/envira` with `<root>/lib/python*/site-packages/envira-*.dist-info/RECORD` listing `/bin/envira,` | pip (`pyvenv.cfg` in root: venv python; else `python3 -m pip install --user`) | pip install --upgrade |
| anything else | release binary | download + replace |

pipx and uv also have a RECORD, so they are checked first. Copying a release binary over a
pipx-managed one still counts as pipx (the path decides).

## Gotchas found while testing

- After the binary is replaced, Linux reports the running process's exe as
  `<path> (deleted)`; `canonicalize` then fails. `live_path` strips the suffix. This hit the TUI
  when `U` was pressed a second time after declining the restart.
- The TUI process stays the old version until restarted, so after an update it sets
  `self_updated` and `U` only says to restart (otherwise it would download again).
- `reqwest::blocking` panics inside the tokio runtime: the notice and the TUI check run in
  `spawn_blocking`; `self-update` itself is dispatched before the runtime/session.
- Write the new binary into the same directory (`.envira.new`) and `rename`: a rename across
  filesystems fails. For a non-writable directory, stage it in a tempdir as the user, then
  `sudo install -m 755`.

## Testing without a new release

The dev build has the released version, so nothing real is "newer". A tiny HTTP server that
answers `/latest` with `302 Location: …/tag/v9.9.9` and serves
`/download/v9.9.9/envira-<arch>-linux` (a shell script printing `envira 9.9.9`) plus its
`.sha256` exercises the notice, `--check` (exit 1), the TUI header/toast, `U` and the restart
(`exec` of the script prints its version and exits). Point envira at it with
`ENVIRA_RELEASES_URL=http://127.0.0.1:<port>/rel`, and copy the binary out of `target/` first, or
it is a source build. On the host use scratch `ENVIRA_STATE_DIR`/`XDG_*` and
`ENVIRA_NO_REMOTE=1`.

Real downloads: in a `--rm` container, `self-update --version 0.1.1 -y` from the dev binary
exercises the GitHub download, checksum and swap (downgrade uses the same path); a root-owned
`/usr/local/bin/envira` exercises the sudo path. For pipx: `pipx install envira==0.1.1`, copy the
dev binary over the venv's `bin/envira`, then `--version 0.1.1` (pinned) or the fake server
(upgrade; it warns that the installed version is not the fake one).
