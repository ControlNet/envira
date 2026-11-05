# File writes, privilege and process lifetime (rules from five deep reviews, 2026-11-05)

## Writing files
- Every file envira writes goes through `files::replace_with` / `replace` / `replace_copy`
  (state, backups `meta.toml`/`orig`, `envira fs` edits, catalog cache + ETag, fetch-release
  binaries). It creates a unique temp name with O_EXCL (a planted symlink at a predictable name
  is never followed), keeps it 0600 while filling (a private config is never readable mid-write),
  gives it the old file's mode (or the umask mode for a new file, probed by creating the temp
  with 0666 while still empty), as root chowns it to the old file's owner (a new file: to its
  directory's owner when that is not root), then renames.
- Do not write `std::fs::write`, `File::create` or `fs::copy` to a destination that already
  exists or sits in a directory someone else can write; use `files::*`.
- A destination that is a symlink (a dotfile linked from a dotfiles repo by stow/chezmoi) is
  edited where it points (`fs::edit_target`, used by every `envira fs` edit, `restore` and the
  rc blocks): the link stays, the repo's file gets the edit, like `>>`. The path is resolved
  one component at a time like the kernel (`fs::resolve`): links in parent directories are
  followed too and `..` goes up from where a link led. As root, every link on the way that
  root did not make must point at something its owner owns (`metadata`, following; for a
  missing target, the closest existing ancestor); otherwise the edit is refused and the link
  left alone. Checking only the final link let `~/.config -> /etc` or a relative `../../etc`
  link redirect root's edit. `restore` first looks for a backup of the path itself (links envira
  made, older backups), then of the file it points to. `files::replace_with` itself still
  replaces a link at its destination: callers resolve first.
- The walk is for checking only: `edit_target` returns the path as given with only its final
  links replaced (backup keys stay stable; macOS `/var -> /private/var` must not leak in).
  `..` after a missing directory is an error (ENOENT, as for the kernel): `missing/../rc` was
  backed up as absent while the write replaced the existing `rc`, and a restore then deleted
  it. Every writing edit, `fs link`, a restore (also of an ini/toml key: edited through the
  link like any edit) and the generated `env.sh`/`init.*` go through the walk
  (`edit_target(.., true)` / `guard_parent`). Directories envira creates for a new file in a
  user's directory belong to that user (`create_dirs`: only those it really made, never an
  existing one), and backups record the original's owner so a restore as root gives it back.
- Accepted risk: a user who swaps a link between the check and the write (TOCTOU) still wins.
  Closing it needs openat2/O_NOFOLLOW walks for every write. Under `sudo envira` SUDO_USER is
  root-equivalent anyway (their `~/.config/envira/catalog.d` overlay runs commands as root), so
  the link rules are defense in depth for mistakes, not a privilege boundary.
- Restore stages the original first and renames it over; a missing backup leaves the current
  file. Backup dirs: legacy `%`-for-`/` name, or `h-<sha256 prefix>` when the path has `%` or
  the legacy name belongs to another path (meta.toml `path` is checked).
- Privileged installs through a command (`sudo install` in self-update and fetch-release
  `--install-with`, the portable copy template): install to `<dir>/.NAME.envira-new[.pid]`,
  then `mv -f` over the target; remove the temp on failure. The fixed name is acceptable only
  because those directories are not writable by the user. A directory (or link to one) at the
  target is refused first: `mv -f` would move the binary into it and succeed.

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
  sees the same files. Root's PATH there is `runner::ROOT_PATH` (system dirs only, as sudo's
  secure_path gives `{sudo}` steps), not the user's: a `~/.local/bin/grep` must not answer for
  root. Limitation: a plain user without passwordless sudo still sees a private
  system file as "not applied" (no third "cannot tell" state yet).

## Process lifetime
- Cancelling (timeout, `c`, Ctrl-C twice, SIGTERM) sends TERM to the step's process group and
  waits up to 3 s for the whole group, not only the shell, then KILLs what is left.
- A step's processes can leave its group: `sudo` (use_pty, default on current distros) runs
  its command in a new session, and a plain user cannot signal root's processes anyway
  (`kill(-pgid, 0)` then fails with EPERM, which must count as "alive"). Cancel notes the
  step's descendants from `/proc/*/stat` (ppid walk, pid + start time) before TERM orphans them
  and again before KILL, then KILLs what is left one by one, through `sudo -n kill -KILL` for
  EPERM. Only on cancel: a step that exits normally may leave a daemon running. Without /proc
  (macOS, hidepid=2) this falls back to the group kill. A probe that times out gets the same.
  Known limit: a helper that double-forks and calls setsid *before* the cancel (already
  reparented to PID 1 when the first snapshot is taken) is not found. Closing that needs a
  per-step cgroup (or a subreaper that also reaps), with a policy for services a successful
  step starts on purpose.
- The sudo ticket is refreshed every minute during CLI and TUI runs (`app::sudo_keepalive`),
  also when `sudo -n true` passed at the start: that may be a cached ticket that runs out
  before a long build reaches its sudo steps.
- After a run is recorded, the CLI waits for its output thread in `spawn_blocking` and selects
  on SIGINT/TERM/HUP: a blocked stdout must not make the process unkillable (the run's signal
  handlers keep the default action off), and exiting then (128+signal) loses nothing.
- The TUI's redraw (`term.draw`) blocks on a terminal that stopped reading (TCOOFF, stalled
  SSH), so nothing that must happen on a signal may wait for the UI loop: the signal task holds
  its own clone of the run's control (`watch::Sender` is Clone), cancels directly, and once the
  run is recorded (and self-update is not using the terminal) gives the UI loop 3 s before
  `exit(128+signal)`. The block_on thread runs the UI loop; spawned tasks run on the workers.
- Ctrl-C in the TUI: first stops after the running steps, second cancels them (as the CLI).
- A cancel also ends a running check (the probe before a step) and no step starts after it. A
  probe future dropped before it ends kills its group and its descendants (`ProbeGuard`; the
  child is spawned without `kill_on_drop`, so the shell is still alive to find them by
  parentage).
- The self-update probe (`<new binary> --version`): once the binary exited, its output is in
  the pipe; never wait for EOF (a process it left behind can hold the pipe).
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
  The retried unit carries the left-out steps (`Unit.resumed`) and counts them as done, so a
  second failure does not lose them. A revert also writes each finished step to
  `<run dir>/<id>.steps` (JSON string per line, inherited ones first in one write); a unit
  whose record still says `pending` (envira killed mid-unit) is resumed from the longer of that
  file and the record. A revert whose `.steps` cannot be opened does not start, and one whose
  step cannot be written there stops after it. A step envira was running when it died
  may still finish on its own (steps run in their own sessions): reverts should be idempotent
  (`rm -f`), since the resumed step runs again as it is.
- A retried install whose tool is present now (the build worked, the portable system copy
  failed) gets only the steps after its `done_steps` (`Planner::finishing`), when the route is
  the very one that ran: `plan::route_id` (sha256 of method, scope and every step in full) is
  kept in the unit and its record. `via` alone is not a route identity (raw entries are all
  `run`; a catalog changed between failure and retry would copy the old build). Records without
  a route are installed again in full. The TUI's `r` waits for a fresh detection before planning (its snapshot
  is from before the run); `t` (retry the last run) splits by action like `envira retry`.
- No unit starts unless the run dir, its pending record and state pointing at it are on disk
  (else every unit is reported "not started" and state keeps the previous run). Once units ran,
  a failed save of record or state stops further units ("not started: the run cannot be
  recorded"); `RunRecord.unrecorded` (not saved) carries why, the CLI exits 1, the TUI says so.
  The CLI reads the state again once it holds the run lock (as the TUI does): a run that
  finished while it waited at the confirmation prompt keeps its records. The pending
  record already carries each unit's inherited `resumed` steps: a unit that never started
  before envira died has no progress file. Run-dir files (unit logs, `.steps`) open with
  O_NOFOLLOW|O_NONBLOCK, must be regular files, and are fchown'd by descriptor.
- Open items: a third status for a check root could not run ("cannot tell" for a private
  system file seen by a plain user without passwordless sudo).
