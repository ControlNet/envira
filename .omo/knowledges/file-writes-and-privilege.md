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
- Detection and config checks (`capture`) always run as the user (a root-only config check
  cannot pass under sudo envira yet: open item R2).

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
- Open items: R2 root-only config checks, R3 retry of a partially failed revert (needs per-step
  progress), cgroup v1/nonstandard mounts, capture output caps, explicit binstall compile policy
  for self-update.
