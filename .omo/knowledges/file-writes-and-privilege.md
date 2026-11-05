# File writes, privilege and process lifetime (rules from two deep reviews, 2026-11-05)

## Writing files
- Every file envira writes goes through `files::replace_with` / `replace` / `replace_copy`
  (state, backups `meta.toml`/`orig`, `envira fs` edits, catalog cache + ETag, fetch-release
  binaries). It creates a unique temp name with O_EXCL (a planted symlink at a predictable name
  is never followed), keeps it 0600 while filling (a private config is never readable mid-write),
  gives it the old file's mode (or the umask mode for a new file, probed by creating the temp
  with 0666 while still empty), as root chowns it to the old file's owner, then renames.
- Do not write `std::fs::write`, `File::create` or `fs::copy` to a destination that already
  exists or sits in a directory someone else can write; use `files::*`.
- A destination that is a symlink (a dotfile linked from a dotfiles repo by stow/chezmoi) is
  edited where it points (`fs::edit_target`, used by every `envira fs` edit, `restore` and the
  rc blocks): the link stays, the repo's file gets the edit, like `>>`. As root a link is
  followed only if root owns it or its owner owns the file it points to (checked per hop, with
  lstat through linked directories; for a new file, the directory's owner); otherwise the edit
  is refused and the link left alone. `restore` first looks for a backup of the path itself
  (links envira made, older backups), then of the file it points to. `files::replace_with`
  itself still replaces a link at its destination: callers resolve first.
- Restore stages the original first and renames it over; a missing backup leaves the current
  file. Backup dirs: legacy `%`-for-`/` name, or `h-<sha256 prefix>` when the path has `%` or
  the legacy name belongs to another path (meta.toml `path` is checked).
- Privileged installs through a command (`sudo install` in self-update and fetch-release
  `--install-with`, the portable copy template): install to `<dir>/.NAME.envira-new[.pid]`,
  then `mv -f` over the target; remove the temp on failure. The fixed name is acceptable only
  because those directories are not writable by the user.

## Who runs a step
- `sudo envira` = EUID 0 + SUDO_USER: user-scope steps run as the user via `runuser` (or
  `sudo -u` where there is no runuser: macOS, Fedora images with only util-linux-core), system
  scope steps and steps marked `as_root` (the system copy of a portable build) stay root with
  root's HOME. `{sudo}` expands to nothing for root.
- Backups made as root for SUDO_USER are chowned to SUDO_UID/SUDO_GID.
- Detection and config checks (`capture`) run as the user. A check of a system-scope config
  that fails is run again as root (`Runner::capture_as`, `status::check_passes`) when root is at
  hand without a password prompt: in status/explain only for `Privilege::Root` or passwordless
  sudo (a failing `sudo -n` lands in the auth log), in a run whenever envira can elevate (the
  unit's own steps already authorized sudo). Non-root goes through
  `sudo -n -- env HOME=… PATH=… ENVIRA_STATE_DIR=… bash -eo pipefail -c '<check>'`, so the check
  sees the same files. Limitation: a plain user without passwordless sudo still sees a private
  system file as "not applied" (no third "cannot tell" state yet).

## Process lifetime
- Cancelling (timeout, `c`, Ctrl-C twice, SIGTERM) sends TERM to the step's process group and
  waits up to 3 s for the whole group, not only the shell, then KILLs what is left.
- After a step exits, the output pumps get 2 s to drain, then are aborted: a background daemon
  holding stdout does not keep the executor waiting.
- CLI output after the executor starts ignores write errors (`envira install … | head` must
  finish the run and record it).
- Run events go through `exec::events()`: log lines waiting for a slow reader are capped at
  4 MiB (then left out, counted, and reported as "N lines not shown here" before the unit's next
  line or its end); other events always arrive. The executor never waits for the display.
  The CLI prints from its own thread and handles signals in the async task, so a stalled
  `| less` holds neither the run nor Ctrl-C. The TUI reads the run's receiver itself (no
  forwarding into its unbounded app queue), folds at most 2048 events per redraw, and keeps per
  unit at most 20,000 lines / 2 MiB of log. Measured: RSS flat at ~24 MB while the disk log grew
  to 89 MB with stdout unread (before: 25 -> 107 MB in 4 s).
- Runs hold the envira binary with a shared flock (`/proc/self/exe`), self-update takes it
  exclusively: no swap while any run of any user/state dir uses it.

## Testing these paths
- The second review's reproduction scripts (Python, run as root in `docker run --rm
  --network none` containers with the debug binary mounted read-only) are the fastest way to
  check: they plant symlinks, close stdout mid-run, kill helpers, fake releases. Re-run them
  after changes and compare markers (e.g. `DESCENDANT WROTE AFTER CANCEL`, `TARGET EXISTS`).
- Probes (`capture`) keep at most 1 MiB per stream and drain the rest. A terminal error in the
  TUI during a run cancels the run (like SIGHUP) before envira leaves. A catalog reload rebuilds
  the runner (PATH from the new catalog).
- Run records keep `done_steps` (labels of the steps a unit got through) for units that did not
  succeed. `envira retry` / TUI `r` plan a failed revert even when the config no longer passes
  its full check (`Planner::retrying`): the leading steps matching `done_steps` are left out,
  the step that failed runs again as it is (it may have undone part of its change), and the
  steps after it carry `only_if` = their apply check (run only while their change is in place;
  `as_script` writes `if check; then cmd; fi`). Plain `config revert` is unchanged.
- Open items: explicit binstall compile policy for self-update.
