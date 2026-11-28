# Machine-readable output (`--json`)

Added for issue #10. The code is in `src/json.rs`, and the commands call it from `src/commands.rs`.

## Shape

There are three kinds of output:

- **Query commands** print one pretty JSON document:
  - `status`, `plan`, `doctor`, `explain`, `log`, `log --list`, `update --check`, `adopt`, `catalog check`, `catalog show`.
- **Run commands** print compact NDJSON, and every line has an `event` key:
  - the commands are `install`, `update`, `retry`, `config apply`, `config revert`;
  - the events are `plan` → `unit_started` / `line` / `unit_finished` → `shell_env` → `finished`, with `notice` in between.
- **Errors** are printed as `{"event":"error","code":..,"message":..}` on stdout, and the exit code is 2.
  - Clap parse errors stay on stderr. Don't wrap them.

## Rules worth keeping

- `doctor` and `explain` first collect a Serialize struct, then render text or JSON from it.
  - The text output was diffed byte for byte against a pre-change baseline.
  - New fields go into the struct; never into a second, parallel printer.
- `Plan` keeps its derive: `skipped` serializes as tuples.
  - `json::plan()` rewrites it to `{id, reason}` objects and adds `needs_sudo` and `low_space`, both of which need facts.
- Error codes come from `json::coded(code, msg)`. `json::code_of` searches the whole anyhow chain, so a `.context()` added later keeps the code.
  - Codes in use: `unknown_item`, `not_a_config`, `no_runs`, `run_locked`, `sudo_required`, `confirmation_required`, `aborted`.
- In JSON mode a run without `-y` fails with `confirmation_required` before anything asks, including the sudo password prompt.
  - With `-y`, the sudo prompt still goes to /dev/tty, which keeps stdout clean.
- Signal handlers are installed before the executor is spawned.
  - Otherwise a signal in that gap kills envira while its steps keep running in their own sessions.
- JSON never serializes `PathBuf`: `serde_json` errors on non-UTF-8 paths, and `json!` panics on that error.
  - Convert paths with `display().to_string()`.
  - `exec::secrets()` reads `vars_os`, because `std::env::vars()` panics on any variable that isn't UTF-8.
- `log`: a run is `running` only when the run lock is held and the lock file contains that run's id.
  - Taking the lock empties the file (so `adopt`, the TUI and self-update holding it make no run look active), and the executor writes its run id there (`state::mark_active_run`).
  - Don't use the newest run id: ids are local times and go back with the clock.
  - Don't use `State::load` either: it moves a broken state.toml aside, and read-only `log` must not do that.
- After a SIGTERM/SIGHUP, `run_plan` waits at most 5 s for a stalled stdout reader. `main` ends the runtime with `shutdown_background()`, because a dropped runtime waits for the stuck blocking task forever.
  - The run lock is released as soon as the run is recorded, before the end is shown.
- Text `doctor` prints the machine first and the rc duplicates after the tool snapshot, so slow probes don't hold back the rest.
- `Facts.home` and `Facts.state_dir` serialize through `path_text`. A non-UTF-8 home must not break `doctor --json`.
- `json::print` (query documents) returns write errors, so `> /dev/full` exits 2. `json::emit` (run events) ignores them, so a run always finishes.
- The executor reads a unit's cancel baseline when it sends `UnitStarted`, not after the unit has opened its log and temp dir.
  - Otherwise a SIGTERM in that gap was missed and the unit ran to the end.
- The executor sets `RunRecord.unrecorded` before it sends `Event::Finished`, so the `finished` event can report it.
- Hints are not a separate field: they are `[envira] hint ...` lines and also appear in the run record.

## Review history

- On 2026-11-28 codex (default model) reviewed the feature; the report is under the session scratchpad.
- Fixed:
  - the `-y` check now runs before the sudo prompt;
  - the dry-run writes in `retry` (shell env only) and `config revert --forget`;
  - the signal gap;
  - the non-UTF-8 path panic;
  - text `doctor` keeps its report when state.toml is unreadable;
  - text `explain` keeps trailing empty probe lines;
  - `log --run` showing an old run as running.
- Round 2 fixed:
  - the SIGTERM race right after `unit_started`;
  - non-UTF-8 home/state paths in `doctor --json`;
  - `log` status after a clock rollback;
  - query JSON ignoring write errors.
- Round 3 fixed:
  - a stalled reader after SIGTERM kept envira (and the run lock) alive;
  - `log` moved a broken state.toml aside;
  - an unrelated lock holder made an old run look `running`;
  - text `doctor` waited for the probes before printing anything.
- Left as is:
  - one Ctrl-C lets steps finish (by design);
  - `status --json` errors on stdout (the issue asked for that);
  - `install <bundle>` records the bundle before confirmation (pre-existing, also in text mode);
  - `config revert <tool>` is a plan skip, not `not_a_config`.

## Testing a run end to end

- Use a dummy catalog of raw `run` items in a `docker run --rm` container, as a non-root user.
  - Use items that succeed, fail, are blocked by a failed dependency, and one that sleeps for the Ctrl-C check.
- Write stdout and stderr to separate files and check that every stdout line parses.
- zsh pitfalls when scripting the checks:
  - `$c` does not word-split; use `${=c}`.
  - `echo "$out"` interprets backslashes, so pipe the output directly instead.
- A background job in a non-interactive bash still receives `kill -INT`: tokio installs its own handler.
- Steps run with `bash -eo pipefail`, so a dummy `yes | head` fails with 141. Generate output with a loop.
- To stall a reader: `mkfifo f; (exec 3<f; sleep 30; cat <&3) &`, then run envira with `>f`.
