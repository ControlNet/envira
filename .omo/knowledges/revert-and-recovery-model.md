# Revert, recovery and update-routing model (2026-11)

How envira decides what to undo, how it resumes, and which install route an update takes. Written
after fixing the 2026-11-13 deep-review findings (R1–R23, D1, D2) and the gaps its verification
found (F1–F7).

## Revert undoes only what envira changed

- Every config step has a stable `key` (`plan::step_key`): a hash of its revert command, or of
  its apply command when there is no revert. The key stays when only the value the step sets
  changes. Labels are not identities, because two `ini-set ~/.gitconfig` steps share a label.
- The key is taken from the command expanded with `Vars::identity`: `{envira}` is the word
  `envira` and `{sudo}` is empty. The expanded command itself holds the binary's path and, under
  sudo, `env ENVIRA_STATE_DIR=…`; a key from it changed with another binary or with `sudo envira`,
  and the revert then found nothing to undo.
- An apply records the keys of the steps whose command ran in `ConfigRecord.changed`. A step that
  was already in place is not recorded. A failed step is recorded too, because it may have
  changed part of what it touches.
- Before a step's command runs, the apply appends its key to `<run dir>/<id>.applying` (synced).
  `State::load` adds the keys of every such file to the config's record, creating one if needed,
  so an apply that envira did not live to record is still envira's to undo. A run removes the
  files once its final state save has succeeded. A key noted for a step that never ran is
  harmless: the revert step's `only_if` check skips it.
- A revert:
  - plans only the recorded steps, and undoes nothing when none are recorded ("envira did not
    change it");
  - removes the keys it got through from `changed`;
  - ends with a failing step (key `plan::STALE_KEY`, no command) when recorded keys match no
    catalog step, so the record stays. `envira config revert --forget <id>` drops the record
    once the user has undone those changes by hand.
- `changed = None` marks a legacy record from before the list existed. Such a config is
  reverted in full.
- A config with no record at all is not reverted. Envira never changed it here, or kept no state
  at that time.
- A config can be reverted while any recorded keys are left, even when it no longer passes its
  checks (partly applied, or drifted since).
- Revert progress (`<run>/<id>.steps`, `UnitRecord.done_steps`) stores keys, and
  `resume_revert` matches on the set of keys rather than position. Older run records hold labels,
  which match nothing, so every step runs again under the `only_if` rule.
- A revert step with no command fails the unit: "no revert in the catalog; undo it by hand". The
  config stays applied. Once the change has been undone by hand, the step's `only_if` check finds
  nothing to undo and the revert goes through.
- `catalog check` warns about every config step without a revert.

## Making a raw step revertible

- **Command edits a file as a whole** (`fix_zellij_kdl.py`, an installer that edits
  `opencode.json`): apply runs `{envira} fs keep <file>` first, and revert runs
  `{envira} fs restore <file>`.
  - `keep` records a whole-file backup, including the fact that the file was absent, so restore
    removes a file the command created.
  - An earlier backup is kept, since it holds the pre-envira content.
  - The remote catalog is deployed on every push to main and read by released envira binaries.
    Until `min_envira` is at least 0.1.6, the step is written as
    `{ {envira} fs keep F || ! {envira} fs keep --help >/dev/null 2>&1; } && <edit>`. That
    stops only when `keep` exists and fails; an older envira, which has no `keep`, edits the
    file without a backup, as it did before.
  - The same rule applies to any new `envira` subcommand that the catalog calls.
- **Command changes a value of the system** (login shell, sysctl): apply saves the old value
  once in `${ENVIRA_STATE_DIR:-$HOME/.local/state/envira}/<name>.orig`, and revert restores it
  and then removes the file. `ENVIRA_STATE_DIR` is set for every step.
- **Downloaded file the user may since have edited**: revert moves it to `<file>.envira-old`
  instead of deleting it.

## Backups (`envira fs`)

- The backup dir, each slot and `meta.toml` are private (0700/0600). Restoring a key copies the
  file's previous values into metadata, so the metadata can be as sensitive as the file itself.
- A corrupt `meta.toml`, or an `orig` without its record, is an error. It must never be treated
  as "no backup", because the next edit would then save already-modified content as the
  original.
- `Meta.whole` marks a whole-file edit (write, block, link, keep). Restoring the last key keeps
  the slot while `whole` is set.
- A `line` check passes only when every matching line equals the wanted text, since the shell
  uses the last assignment. Apply refuses when several matching lines differ.
- When not root, `{envira}` in a system-scope step expands to
  `env ENVIRA_STATE_DIR='<dir>' '<exe>'`. That way a sudo'd `envira fs` uses the same backup
  namespace as the user's run.

## Updates and retries

- `ToolRecord.entry` holds `plan::entry_key`, a hash of method, raw command and params.
  `route_for` prefers the entry with that key, and falls back to the first usable entry with the
  method name. This means reordering `run` entries in the catalog no longer switches a managed
  tool to another installer.
- When the recorded key matches no entry and the method is `run`, status keeps the fallback only
  if `path_route` agrees (the binary is where that entry installs). Otherwise the tool has no
  route, and update says "`envira install --force <id>` installs it again". A named method
  (apt, cargo, …) is the installer itself, so its fallback stays. A record without a key (older
  envira) also falls back as before.
- `path_route` adopts a binary found in the home for a raw entry without `{bin_dir}` only when
  the binary sits under one of the tool's `shell.path` trees (the declared dir's parent). For
  example, `~/go/bin/fzf` is not fzf's `~/.fzf` installer.
- When an install plan is merged after an update plan (TUI selection, `retry`), use
  `Plan::extend_after`. A plan built on its own treats the tools being updated as present, so
  without it the install would not wait for those updates.
- On retry, an update target that is no longer present (a failed update removed it) is planned as
  an install (`RetryTargets::take_gone`).
- A shell-env failure is stored as `RunRecord.shell_env_error` and makes the exit code 1.
  `retry` with nothing else left regenerates the shell env.

## Package versions

- `apt`, `dnf`, `pacman` and `zypper` (`status::PACKAGE_METHODS`) report the version with its
  revision:
  - dpkg: `${Version}` and `Candidate:`;
  - rpm: `%{EVR}`;
  - dnf: `%{evr}`;
  - pacman and zypper: `Version :`.
- The regex drops the epoch (`(?:\d+:)?`), because an older envira compares only the leading
  numbers and `2:9.1-1` would become `[2]`. Epoch bumps are rare, so they are the accepted loss.
- These versions are compared with `version::compare_package` in the package manager's own
  order (`version::Scheme::of(method)`), so `1.2.3-1` → `1.2.3-2` counts as an update:
  - apt: dpkg's algorithm;
  - dnf and zypper: rpm's `rpmvercmp` (`~` before anything, `^` after the base version, so
    `1.0^git` < `1.0.1`);
  - pacman: libalpm's older copy (no `~`/`^`, `1.0a` < `1.0`, release compared only when both
    sides have one).
- The expected orders in the tests were checked against `rpm.vercmp` (Fedora) and pacman's
  `vercmp` (Arch) in `--network none` containers.
- The list is hardcoded, not a catalog field. The catalog schema uses `deny_unknown_fields`, so a
  new field would make every older envira reject the published remote catalog. Add schema fields
  only together with `min_envira`.
- Verified in throwaway `--network none` containers:
  - `rpm -q --qf '%{EVR}'` on Fedora 44 and Leap 15.6;
  - `dnf5 repoquery --qf '%{evr}'`.
- An apt candidate no newer than the installed package is reported as an error when apt's lists
  are behind (`apt::decide`), because stale lists cannot show that a package is current. A newer
  candidate still counts.

## Other

- The remote catalog cache and its ETag are per URL (`catalog::remote::cache_file`). The default
  URL keeps `catalog.toml`.
- The TUI tags detection events with a generation number. A late answer from an older
  latest-version check is dropped and does not overwrite the cache. A cache-only refresh does not
  cancel an online check that is still running.
- PATH order has one source, `status::managed_dirs`: `env.path`, then `env.shell_path`, then
  each tool's `shell.path` (tools by id). Detection searches in that order. env.sh renders the
  dirs it covers (`env.shell_path` and present tools' dirs) in a single "PATH" block, emitted
  last to first because each `envira_path_prepend` goes in front.
- The Go tool sets no `GOROOT` or `GOPATH`. go finds its root from its binary's location, and
  `~/go` is the default GOPATH. An exported `GOROOT=~/.go` broke any other go (apt's, the
  user's) whenever `~/.go` existed.
- A run that left only the shell environment behind (`RunRecord.shell_env_error`) is offered in
  the TUI too: `t` on the dashboard and `r` on the run screen send `Cmd::WriteShellEnv`, which
  writes it and clears the error from the record, as `envira retry` does.
- `envira plan --script` runs each unit in a subshell inside `mktemp -d`, with `ENVIRA_TMP`
  exported, as native execution does.
- `envira.sh` refuses a directory at `<dir>/envira`, and stages the download in a unique
  `mktemp` file inside the install dir before the final rename.
- The Go install unpacks into `~/.go.envira-new` and checks it with `bin/go version` before
  swapping it with `~/.go`. If the swap fails, the old copy is restored.
- `git-settings` no longer sets `credential.helper=store`. The separate config
  `git-credential-store` is in the `extras` bundle only, so it is opt-in.
- `default-shell` revert restores the login shell recorded at apply time. It falls back to
  `/bin/bash` when the record is empty or names zsh.
