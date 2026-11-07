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
