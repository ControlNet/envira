# README TUI screenshots

How the README images (`assets/tui-*.webp`) were made (2026-11, envira 0.1.2), so they can be
re-shot after UI changes.

## Setup

- A scratch Docker container from the integration image (`.github/integration-test/`), run with
  `--hostname workstation`, user `user`, `LANG=C.UTF-8`, `COLORTERM=truecolor`. Never the host:
  screenshots must show only `user` / `workstation` / `/home/user`.
- Put the binary at `~/.local/bin/envira` and start it from there. Steps show commands with the
  binary's own path (`sudo -n <exe> apt-refresh`), so a dev path like `~/envira-dev` leaks into
  the plan screenshot.
- Make the dashboard interesting: install the `shell` bundle plus a few terminal tools, and
  downgrade one github-release tool (lazygit) so a row shows as outdated.
- tmux at 132 columns; 40 rows for the dashboard, 30 rows for plan and run screens.

## Driving the TUI

- `/` search matches descriptions too, so after each search check which row is under the cursor
  (cursor background is SGR `48;2;40;52;87` in `capture-pane -e`) before pressing space.
- A failed run that shows isolation well: select codex (pulls fnm, nodejs, npm-prefix) plus a
  pipx and an apt tool, and block GitHub downloads by adding
  `release-assets.githubusercontent.com` and `objects.githubusercontent.com` to `/etc/hosts`.
  fnm fails with network retries, its three dependents show as blocked, the rest succeed.
  Restore `/etc/hosts` right after.
- Do not pick cargo tools for that run: with GitHub blocked they fail too (network, retried),
  giving extra failures in the picture. Since 2026-11-05 the cargo method no longer lets
  cargo-binstall compile on its own; it compiles only when `cc` exists and otherwise says a C
  compiler is needed (diagnosed as `no C compiler`, hint `envira install build-tools`).

## Rendering

1. `tmux capture-pane -e -p` (keeps truecolor SGR).
2. Convert SGR to HTML spans (Tokyo Night `#1a1b26` / `#c0caf5`, JetBrains Mono, line-height
   1.2, a window frame with traffic-light dots).
3. Headless Chrome: `--force-device-scale-factor=2 --default-background-color=00000000
   --screenshot`, then ImageMagick `-trim +repage -bordercolor none -border 40`.
4. WebP for the README: `convert x.png -strip -define webp:lossless=true -define
   webp:method=6 x.webp`. Lossless beats lossy q90 on terminal frames (flat colours): about
   70-120 KB per image versus 230-500 KB as PNG, with no blur on text.

README links use absolute `raw.githubusercontent.com/.../main/assets/...` URLs because PyPI and
crates.io render the same README; `assets/` is not in the crate's `include`.
