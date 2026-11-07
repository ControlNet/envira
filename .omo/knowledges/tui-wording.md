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
