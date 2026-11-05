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
| `$CARGO_HOME/bin` (default `~/.cargo/bin`) | cargo | with cargo-binstall: `cargo binstall -y --force --disable-strategies compile envira[@v]` (download only), and if that fails, `cargo install --locked --force [--version v] envira` (shown as "if no prebuilt" before asking); without it, that `cargo install` directly. Both get `runner::build_env()` (`CARGO_BUILD_JOBS` one per GiB free). |
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
- Write the new binary into the same directory and `rename`: a rename across filesystems fails.
  The temp file has a unique name created with O_EXCL (`tempfile_in`), never a fixed
  `.envira.new` that a planted symlink could redirect; it is closed (`into_temp_path`) before
  `--version` runs it, or exec fails with ETXTBSY.
- Ownership: the new file gets the old file's uid/gid when envira is root (`sudo envira
  self-update` on a root-owned /usr/local/bin/envira must stay root-owned; the first version
  chowned it to SUDO_USER).
- Non-writable directory: stage and check it in a tempdir as the user, then
  `sudo install -m 755 -o <uid> -g <gid> staged <dir>/.envira.new.<pid>` and `sudo mv -f` over
  the binary (a rename in one directory: the old binary stays until the swap).
- A package-manager update that leaves `--version` at the old version returns `Unchanged(1)`, not
  `Updated` (the TUI would offer a restart into the old version). A pip install outside the
  home is refused (`pip --user` would install a second copy elsewhere).
- Only `https://api.github.com/` gets `GITHUB_TOKEN`/`GH_TOKEN` (`net::github_token`), never a
  mirror set with `ENVIRA_RELEASES_URL`.

## Testing without a new release

The dev build has the released version, so nothing real is "newer". A tiny HTTP server that
answers `/latest` with `302 Location: …/tag/v9.9.9` and serves
`/download/v9.9.9/envira-<arch>-linux` (a shell script printing `envira 9.9.9`) plus its
`.sha256` exercises the notice, `--check` (exit 1), the TUI header/toast, `U` and the restart
(`exec` of the script prints its version and exits). Point envira at it with
`ENVIRA_RELEASES_URL=http://127.0.0.1:<port>/rel`, and copy the binary out of `target/` first, or
it is a source build. On the host use scratch `ENVIRA_STATE_DIR`/`XDG_*` and
`ENVIRA_NO_REMOTE=1`.

Sudo paths in a `--rm` container (`--user root`, a user `alice` with NOPASSWD sudo, the fake
server inside the container): `su alice -c 'envira self-update -y'` on a root-owned
`/usr/local/bin/envira` (sudo install + mv), `su alice -c 'sudo envira self-update -y'` (root
acting for alice; the file must stay `root:root 755`), and an alice-owned `~/.local/bin/envira`.

Real downloads: in a `--rm` container, `self-update --version 0.1.1 -y` from the dev binary
exercises the GitHub download, checksum and swap (downgrade uses the same path); a root-owned
`/usr/local/bin/envira` exercises the sudo path. For pipx: `pipx install envira==0.1.1`, copy the
dev binary over the venv's `bin/envira`, then `--version 0.1.1` (pinned) or the fake server
(upgrade; it warns that the installed version is not the fake one).
