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
- In JSON mode a run never asks `Proceed?`: without `-y` it fails with `confirmation_required`.
  - The sudo password prompt still goes to /dev/tty, which keeps stdout clean.
- The executor sets `RunRecord.unrecorded` before it sends `Event::Finished`, so the `finished` event can report it.
- Hints are not a separate field: they are `[envira] hint ...` lines and also appear in the run record.

## Testing a run end to end

- Use a dummy catalog of raw `run` items in a `docker run --rm` container, as a non-root user.
  - Use items that succeed, fail, are blocked by a failed dependency, and one that sleeps for the Ctrl-C check.
- Write stdout and stderr to separate files and check that every stdout line parses.
- zsh pitfalls when scripting the checks:
  - `$c` does not word-split; use `${=c}`.
  - `echo "$out"` interprets backslashes, so pipe the output directly instead.
- A background job in a non-interactive bash still receives `kill -INT`: tokio installs its own handler.
