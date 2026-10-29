# envira Rust implementation notes (feature/rust-cli, M1–M3)

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
