# TUI wording (2026-11-07 review)

Words on screen, and what they mean:

| On screen | Meaning | Code |
|---|---|---|
| follow (`space: follow`, "Following X") | what the bundle lacks is selected now and on later starts, and shows as wanted; nothing is installed until Enter | `state.bundles` |
| `wanted` ◌ (yellow) | in a followed bundle, not installed | `Label::Missing` |
| `new` ◆ | in the catalog since envira last looked | `Label::New` |
| `available` · | not installed, not wanted | `Label::Available` |
| `To do` filter | wanted, new, failed, drifted or outdated | `Filter::Attention` |
| Selected panel | the selection as Enter would plan it (`+` install, `↑` update, `◇` config, `↺` revert) | `ui::selection` |
| `via command` | a catalog route with a raw `run` | `ui::via_name` |
| `managed by`: `envira` / `not envira · A` / `other · <path>` (unknown route: via left blank) | Ownership Managed / Adoptable / External (the CLI and JSON keep the old words) | `ui::managed_by` |
| why `selected` | a unit's reason `requested` | `ui::why` |
| `+ shell setup` | the run re-renders the managed shell env files | plan summary, run summary |
| "macOS only", "Arch-based only", "not on Linux", "needs an NVIDIA GPU" | an unmet `when` predicate | `template::describe` |

Rules:
- `status --json` keeps the old label values (`"missing"`). Integration scripts, such as
  `tests/integration/full.sh`, depend on them. Only the text shown to people changed.
- Which route notes are "about another platform" is a flag computed from the predicate
  (`template::about_platform`), not matched against the note text. So the wording can change
  freely.
- The run's name comes from the plan title's first word (`install 2 · update 1` → `install`).
- Following a bundle relabels its items (`App::set_wanted`) at once; detection then confirms.
- The update count is shown in the overview only. A toast reports it only to answer a `u`
  pressed while the check was still running.

Wording rule (second review): a hint names the action and its target ("S → user", "enter: password
or user mode"), not a bare key or label ("[S]", "Go"). Shortening must not drop a real condition
("in view", "selected, or this one").

## Small terminals (checked at 80x24, 100x30 and 120x35)

- Header: the machine facts are dropped first. `new installs → … (S → …)` stays.
- Items table (`ui::item_columns`): columns are dropped in this order: via, scope, latest, the
  status word, then version. The glyph still shows the status, and the detail panel shows all of
  them. Names are 12 to 22 wide. The last column (managed by / note) has at least 17 columns
  and is cut with `…`.
- Key bars: the hints listed in `DROP_FIRST` go first, then others from the end. Help, Quit,
  Back, Install, Cancel running and Dashboard always stay.
- Help: when the box is taller than the terminal, ↑↓/j k scroll it and the last line says
  `↓ more · any other key closes`. `App::help_max` is set while drawing.
- Plan and run tables: via, scope and action columns go before the last column gets too
  narrow. Cut text gets `…`.
- Check sizes with tmux (`new-session -x W -y H`), always in the scratch env. Source its env
  script by absolute path, and check that `ENVIRA_STATE_DIR` is set before launching: a
  relative path that fails falls back to the real state silently.
- Items title: when not every filter tab fits, only the active filter is shown, followed by
  `· f` and then the search (`Items Installed · f  /co`). The search is never cut off.
- Below 80 columns the left column is 26 wide. When fewer than 8 columns are left for the
  managed by/note column, it goes before the name is squeezed. The header drops the host name
  when the scope hint would otherwise be cut.

## After the codex review (F01–F15)

- Widths are terminal cells, not chars: `ui::truncate` and `ui::pad` use `unicode-width`, so
  CJK names (two cells each) stay aligned.
- Key bar: the essential hints get their room first. A toast or the retry label is cut to what is
  left. The summary uses `Retry (N): r` below 80 columns.
- Overview counters choose the longest label that fits: `to install · N new in catalog` →
  `to install · N new` → `to install` → `to do`. Failed falls back the same way, ending in
  `failed · t` and then `failed`.
- Selected: when it overflows, the last row reads `… N more · enter: see the plan`, or just
  `… N more` when that does not fit.
- Long search: the title shows the end of the query after `/…`, so the caret stays visible.
- Plan: J/K scroll Commands, and PgUp/PgDn (or the mouse wheel) scroll Left out. Each title
  shows `↓ key: more` while lines are hidden. The limits come from `Paragraph::line_count`
  (ratatui feature `unstable-rendered-line-info`) and are stored in `App::plan_cmd_max` and
  `App::left_out_max` at draw time.
- Opening a plan clears a non-warning toast, so `Run N steps: enter` shows.
- Run Info shows the run-wide shell-setup result first, and the log path with `~`. Log lines
  are cut with `…`; z widens them.
- tmux gotcha: when a tmux server is already running, a new session gets the server's
  environment, not the caller's exports. Start the TUI through a script that sources the
  scratch env itself and refuses to start without `ENVIRA_STATE_DIR`.
