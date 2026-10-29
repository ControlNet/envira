# envira Rust implementation notes (feature/rust-cli, M1–M4 and the full catalog)

Lessons from building the Rust + ratatui rewrite. Design: `.omo/plans/envira-rewrite-design.md`.

## Process execution
- Run every catalog command in a **new session** (`setsid()` in `pre_exec`), not just a new process
  group. With only `process_group(0)`, a tool that opens `/dev/tty` gets SIGTTIN while the TUI owns the
  terminal and hangs until the timeout. `yazi --version` does this, so yazi showed as missing only inside
  the TUI. The session id doubles as the process group for `kill(-pid)` cancellation.
- Commands run as `bash -eo pipefail -c` with stdin `/dev/null`. `sudo -n` is used inside templates,
  and credentials are refreshed once before the run (`sudo -v`, on the normal screen when it prompts)
  and kept alive every 60 s.
- Under Claude Code's Bash tool there is no controlling TTY, so `envira status` looked fine there while
  the TUI in tmux did not. Check TTY-sensitive behaviour in tmux (`tmux new-session -d ...; tmux capture-pane -p`).

## Vendor install scripts
- bun: `curl -fsSL https://bun.sh/install | SHELL=/bin/sh bash` stops it from appending to rc files.
- fnm: `--skip-shell`; rustup: `-y --no-modify-path`. envira renders PATH/init itself.
- The fnm and bun installers need `unzip`. It is a catalog item that both depend on.
- Fresh Docker images have no apt lists: the apt template retries after `apt-get update`.
- fnm's Node.js 25+ needs `libatomic.so.1` (apt `libatomic1`); without it `node` fails to load on
  minimal images. It is the `libatomic` catalog item, a dependency of `nodejs`.

## Updates and adoption (M4)
- Registry answers verified 2026-10: crates.io `crate.max_stable_version` (403 without User-Agent),
  PyPI `info.version`, npm `https://registry.npmjs.org/<pkg>/latest` → `version` (scoped names work
  unencoded), GitHub `releases/latest` → `tag_name` (`v1.39.0`, `bun-v1.4.4`).
- `rustup check` prints `stable-<triple> - update available: A (..) -> B (..)` or `up to date: A`
  and exits 100 when an update exists.
- `fnm ls-remote --lts | tail -n 1` gives the newest LTS (`--latest` is not on every fnm version).
- `bun upgrade`, `herdr update`, `pipx upgrade`, `npm install -g <pkg>@latest`,
  `apt-get install --only-upgrade -y` are the in-place updaters.
- Never re-plan an update with the machine's scope preference: a `~/.local/bin` tool on a sudo
  machine would be "updated" into `/usr/local/bin` and leave two copies. Use the route.
- Run captures in `$HOME`; the launch directory's `rust-toolchain.toml` changed `rustc --version`.

## Full catalog port (2026-10-29)
- The catalog must follow `run.sh` / `run_user.sh` item by item; a sample catalog was rejected as
  "the wrong software list". `catalog_covers_verify_sh` (unit test) keeps verify.sh and the catalog
  in sync and found the missing `pm2`. Bundle `full` = run.sh, `user` = run_user.sh, `extras` = old tools.
- Detect commands that do not use `--version`: `dolt version`, `gotify version`, `hf version`,
  `screen -v` (non-zero exit on some versions), `tmux -V`, `7z i`, `ping -V`, `ctop -v`.
- Under `bash -eo pipefail`, `cmd | grep -q x`, `| grep -m1` or `| head` fail at random with
  SIGPIPE when `cmd` is still writing after the reader exits: `7z i | grep -m1 7-Zip` and
  `ldconfig -p | grep -q libatomic.so.1` flapped between installed and missing (libfuse2 looked
  absent on a host that had it). Use `grep … >/dev/null`; the test `catalog_pipes_do_not_exit_early`
  rejects these readers. Tools whose `--version` exits non-zero (neofetch, some screen builds) use
  `(cmd --version || true) 2>&1 | grep 'Name [0-9]'`.
- LunarVim's installer pip-installs pynvim with `--user`; PEP 668 (Ubuntu 24.04) blocks that, so
  run.sh's LunarVim step fails there too. The catalog sets `PIP_BREAK_SYSTEM_PACKAGES=1`.
- Release asset names change: lazygit went from `lazygit_<v>_Linux_x86_64.tar.gz` to
  `..._linux_x86_64.tar.gz` (run.sh still has the old URL). Asset params take a list of globs.
- oh-my-opencode's installer now writes the plugin as `oh-my-openagent@latest` in
  `~/.config/opencode/opencode.json`.
- oh-my-zsh's installer replaces `~/.zshrc` with its template; envira edits single lines
  (`fs line`) and the managed block is re-added after every sync.
- GitHub API: 60 requests/hour per IP without a token. Two parallel full syncs in local containers
  used it up within ~15 minutes; vendor scripts (superfile) then download `…-v-…` and fail
  oddly. Run full syncs in CI (`.github/workflows/rust.yaml`, job token passed as GITHUB_TOKEN) or
  with a read-only token; never pass the broad `gh` login token into containers that run vendor scripts.
- Do not rebuild `target/release/envira` while a container that bind-mounts it is running: the
  mount keeps the old inode, `current_exe()` becomes `/usr/local/bin/envira (deleted)` and every
  `{envira} fs …` step fails with 127. envira now strips the ` (deleted)` suffix.
- Go: ControlNet/golang-tools-install-script pins a version and edits rc files with no opt-out, so
  the catalog downloads `https://go.dev/VERSION?m=text` → `go.dev/dl/<v>.linux-<arch>.tar.gz`.

## Crate versions (resolved 2026-10)
- toml 1.x: `toml::Table` (Map) has no `values_mut`; use `iter_mut`. `toml::Value::try_into::<T>()` deserializes.
- reqwest 0.13: TLS feature is `rustls` (not `rustls-tls`); `default-features = false, features = ["blocking", "json", "rustls"]`.
- zip 9: `ZipFile::name()` returns `Result`.
- schemars 1.x: `schemars::schema_for!(T)`; `serde_json::Value` works for free-form params, `toml::Value` does not implement `JsonSchema`.
- ratatui 0.30: `ratatui::init()` / `ratatui::restore()`, `Layout::...areas()`, `TestBackend` buffer indexed by `buf[(x, y)]`.
- clap `env = "..."` needs the `env` feature.

## Testing
- `cargo test`: unit tests plus a fake `Runner` (substring rules; `on_install` flips detection after a
  successful install) for executor flows, and `TestBackend` renders for TUI screens.
- `tests/integration/run.sh`: Docker (ubuntu:24.04, user with passwordless sudo). `MODES="sudo user"`;
  user mode sets `ENVIRA_PRIVILEGE=none`. It never touches the host.
- Run ids are second-resolution timestamps; back-to-back runs (revert then apply) collided and shared
  one log dir until `new_run_id` added `-02`, `-03` suffixes.
- Safe manual TUI runs on a real machine: `ENVIRA_STATE_DIR=<tmp>/state ENVIRA_CONFIG_DIR=<tmp>/config envira`
  keeps state writes out of `~/.local/state`. Do not confirm plans on the host.
