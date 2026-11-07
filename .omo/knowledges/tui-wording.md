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
| "macOS only", "Arch-based only", "not on Linux", "needs an NVIDIA GPU" | an unmet `when` predicate | `template::describe` |

Rules:
- `status --json` keeps the old label values (`"missing"`). Integration scripts, such as
  `tests/integration/full.sh`, depend on them. Only the text shown to people changed.
- Which route notes are "about another platform" is a flag computed from the predicate
  (`template::about_platform`), not matched against the note text. So the wording can change
  freely.
- The update count is shown in the overview only. A toast reports it only to answer a `u`
  pressed while the check was still running.

Wording rule (second review): a hint names the action and its target ("S → user", "enter: password
or user mode"), not a bare key or label ("[S]", "Go"). Shortening must not drop a real condition
("in view", "selected, or this one").
