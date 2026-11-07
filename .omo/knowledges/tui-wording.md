# TUI wording (2026-11-07 review)

Words on screen, and what they mean:

| On screen | Meaning | Code |
|---|---|---|
| follow (`space follows`, "Following X") | a bundle this machine keeps complete: what it misses is selected now, and later additions show as wanted | `state.bundles` |
| `wanted` ◌ (yellow) | in a followed bundle, not installed | `Label::Missing` |
| `new` ◆ | in the catalog since envira last looked | `Label::New` |
| `available` · | not installed, not wanted | `Label::Available` |
| `To do` filter | wanted, new, failed, drifted or outdated | `Filter::Attention` |
| Selected panel | the selection as Enter would plan it (`+` install, `↑` update, `◇` config, `↺` revert) | `ui::selection` |
| `via command` | a catalog route with a raw `run` | `ui::via_name` |
| "only on macOS", "needs an NVIDIA GPU" | an unmet `when` predicate | `template::describe` |

Rules:
- `status --json` keeps the old label values (`"missing"`). Integration scripts, such as
  `tests/integration/full.sh`, depend on them. Only the text shown to people changed.
- Which route notes are "about another platform" is a flag computed from the predicate
  (`template::about_platform`), not matched against the note text. So the wording can change
  freely.
- The update count is shown in the overview only. A toast reports it only to answer a `u`
  pressed while the check was still running.
