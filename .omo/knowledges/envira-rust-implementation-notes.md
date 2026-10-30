# envira Rust implementation notes (feature/rust-cli, M1–M5 and the full catalog)

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
- Parallel units (2026-10-30, `exec::execute`): a `FuturesUnordered` of unit futures polled by the
  coordinator, so they borrow the plan and `ExecCtx` without `tokio::spawn`; each gets a clone of
  the control `watch::Receiver` and keeps its own `base_seq`. Lanes (`exec::lanes`) are scanned from
  the step commands by word, not taken from the method name: raw `run` entries hide apt calls
  (fastfetch's PPA), and in root mode `{sudo}` expands to nothing, so sudo is no signal. Concurrent
  `pipx install`s race to create pipx's shared venv and `npm -g` has no lock, hence their lanes.
  A dependency must have *succeeded*, not just started (the plan order alone no longer ensures it).
  Integration tests that need a unit to still be queued at a signal use `-j 1`.
  The first parallel CI run found what the sequential order had hidden: undeclared dependencies
  (yazi-theme's `ya pkg` needs git, the LunarVim installer needs pip from python3, Fedora's chsh
  came with another package), and lanes no word scan can see: yay's `makepkg -si` calls pacman
  (`makepkg` is now a packages word), the LunarVim script runs `npm install -g` (declared with
  `lanes = ["npm"]` on its entry).

## Vendor install scripts
- bun: `curl -fsSL https://bun.sh/install | SHELL=/bin/sh bash` stops it from appending to rc files.
- fnm: `--skip-shell`; rustup: `-y --no-modify-path`. envira renders PATH/init itself.
- The fnm and bun installers need `unzip`. It is a catalog item that both depend on.
- Fresh Docker images have no apt lists: the apt template retries after `apt-get update`.
- apt list age (2026-10-30, `envira apt-refresh`): the mtimes of `/var/lib/apt/lists/*` are not the
  refresh time (apt keeps the server's Last-Modified, a 304 touches nothing); use stamps. An
  unresolvable source makes `apt-get update` warn (`W: Failed to fetch`) and exit 0; a source
  without a Release file or a bad signature exits 100, which must not fail every apt install.
  `add-apt-repository` refreshes the lists itself (no `apt-get update` after it).
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

## Recovery, tracing and safety (2026-10-29)
- `cargo test` and `cargo clippy` do not rebuild `target/debug/envira`; a manual end-to-end check
  after them runs the old binary (a SIGTERM test "failed" that way). Run `cargo build` first.
- A stopped run used to leave its never-started units out of `retry` (only failed/blocked/cancelled
  counted), and a killed run never set `state.last_run`. Records now list every unit as `pending`
  first and state points at the run from its start.
- Steps run in their own session (setsid), so a SIGTERM/SIGHUP that kills envira with the default
  action leaves the installer running orphaned. Both the CLI and the TUI handle the signals and
  cancel the step; the TUI keeps winding down even when drawing fails after SIGHUP.
- `flock` locks belong to the open file description: a second `open` + `flock(LOCK_NB)` in the same
  process conflicts, which makes the lock unit-testable without a second process.
- GitHub's release API gives every asset uploaded since 2025 a `digest` (`sha256:<hex>`), so
  release downloads can be verified even when the project publishes no checksum file (bat, fd).
  goreleaser projects (lazygit) publish `checksums.txt` lines `<hex>  <asset>`.
- Retry only transient HTTP failures. A 403 from GitHub is the rate limit; retrying spends more of it.
- Safe end-to-end checks on the host: a throwaway catalog with raw `run` entries (`sleep`, `touch`)
  plus `HOME`, `ENVIRA_STATE_DIR`, `ENVIRA_CONFIG_DIR` in a scratch dir and `ENVIRA_PRIVILEGE=none`;
  `shell_env` regeneration then writes rc blocks into the scratch home, not the real one.
- ratatui: `Line::styled` puts the style on the line (`line.style`), not on its spans.

- Diagnose before retrying: only network errors and a busy dpkg lock are worth repeating; a 404,
  PEP 668 or a full disk fails the same way every time. Skip `[envira]` and `$ cmd` lines when
  matching, or the echoed command (`$ curl ...`) looks like a curl failure.
- Latest-version queries run 6 at a time (`buffer_unordered(6)`); the 1 s query retry costs about
  10 s in total when completely offline.
- Free space changes between two `statvfs` calls on a busy disk; tests compare the device only.
- Tests that fork children race with `flock` tests: until exec, a child holds a copy of every
  descriptor (O_CLOEXEC closes it only at exec), so a released lock can look held for a moment.

- CI full sync (sudo, 112 items) took ~14 min, 6 of them compiling `yazi-build`; the next were
  lunarvim (68 s) and ripgrep's pcre2 build (32 s). Profile with
  `gh api repos/<owner>/<repo>/actions/jobs/<id>/logs` (`gh run view --log` returned nothing for
  older jobs) and the `✓ <id> <version> (<time>)` lines.
- yazi 26.x prints `Yazi` then `    Version: 26.9.1 (...)` on separate lines; the detect regex covers
  both forms.
- CI clippy flagged `st.f_bavail as u64` (u64 -> u64) while the local clippy on the same
  toolchain did not: libc's statvfs field types depend on the target and libc's build config.

## Release delivery (M5, 2026-10-30)
- Static binaries: build inside `rust:1.93-alpine`, where musl is native. Install
  `musl-dev cmake make perl gcc g++ python3`; aws-lc-sys (rustls' crypto) needs cmake and a C++
  compiler. With no cross toolchain, aarch64 builds on GitHub's free `ubuntu-24.04-arm` runners
  (public repos).
  - x86_64 links as `static-pie`; `file` says "static-pie linked" there and "statically linked" on
    aarch64. A check must accept both (`static(-pie|ally) linked`).
- One `maturin build --compatibility manylinux_2_17 musllinux_1_1` with `bindings = "bin"` gives a
  `py3-none-manylinux…musllinux…` wheel that installs on glibc and musl systems alike. The binary is
  copied out of the same target dir, so the wheel and the release asset are the same build.
- Static musl has no NSS: `getpwnam` does not see LDAP/SSSD users. Fall back to the system
  `getent passwd <name|uid>`, which uses the host's glibc NSS, then to `$USER`/`$LOGNAME`.
- HTTPS from the static binary was checked on Ubuntu and Fedora. reqwest 0.13's `rustls` feature
  uses rustls-platform-verifier, which reads the system CA store. A machine without
  `ca-certificates` therefore cannot fetch; the catalog then falls back to the embedded snapshot.
- `envira catalog show` prints the merged catalog. Loaded back as a single file it is
  byte-identical, categories included, so the published `catalog.toml` is simply that output.
- clap: `bool` flags with `env = …` accept only `true`/`false`. Use
  `value_parser = BoolishValueParser::new()` so `ENVIRA_NO_REMOTE=1` works. Tests that need the
  flag unset use `env -u`.
- `envira.sh` tests run in CI as a `bootstrap` job: `python3 -m http.server` serves the `dist-x86_64`
  artifact, and `ENVIRA_BASE_URL` points the script at it. Appending to the binary must make the
  script exit non-zero and install nothing.
- PyPI trusted publishing is bound to the workflow file name (`release.yaml`) and the environment
  names (`pypi`, `testpypi`). Renaming either breaks publishing silently until the next tag.

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
- `tests/integration/run.sh`: Docker (user with passwordless sudo). `MODES="sudo user"`;
  user mode sets `ENVIRA_PRIVILEGE=none`. It never touches the host. `DISTRO=ubuntu|fedora|arch|opensuse`
  picks `Dockerfile.$DISTRO` (ubuntu:24.04, fedora:44, archlinux, opensuse/tumbleweed); CI runs the
  check suite on all four.
- Multi-distro lessons (2026-10-30):
  - `sudo -n -v` needs *every* matching sudoers rule to be NOPASSWD. openSUSE ships
    `ALL ALL=(ALL) ALL` with `Defaults targetpw`, so it asked for a password despite the user's own
    NOPASSWD rule. Probe with `sudo -n true` (`app::sudo_without_password`).
  - Package renames: p7zip → `7zip` (dnf/pacman/zypper), ping → `iputils`, libfuse2 → `fuse-libs` /
    `fuse2` / `libfuse2`, sshd → `openssh` on Arch, net-tools → `net-tools-deprecated` on openSUSE.
  - The Arch image ships without a sync database and goes stale fast: pacman installs fall back to
    `pacman -Syu` when `-S` fails.
  - `get.docker.com` rejects Arch and openSUSE; they install `docker` from their own repos and need
    `systemctl enable` (configs.docker-service).
  - Tests that wait for a step to start must match the log's `│ begin` line, not "begin": the plan
    output printed earlier contains the word.
  - zsh does not word-split an unquoted `$e`, so `docker run $e …` passes `-e A=B` as one argument;
    run such loops under bash.
  - Python: `dnf install python3` has no pip; Fedora and openSUSE need `python3-devel` for pipx tools
    that compile (archey4's netifaces also needs gcc, hence `build-tools`), and openSUSE splits
    `python3-curses` out (nvitop fails at import). Ubuntu gets python3-dev via python3-pip's
    recommends, Arch's python ships headers.
  - Arch's btop has file capabilities (`cap_perfmon`), and exec fails with EPERM in a Docker container;
    the pacman route is `!container`, so containers get the static release.
  - openSUSE's TigerVNC has no `vncserver` script (vncsession + Xvnc from `xorg-x11-Xvnc`).
  - Fedora 44 has no neofetch; the git + make route covers it.
- Run ids are second-resolution timestamps; back-to-back runs (revert then apply) collided and shared
  one log dir until `new_run_id` added `-02`, `-03` suffixes.
- Safe manual TUI runs on a real machine: `ENVIRA_STATE_DIR=<tmp>/state ENVIRA_CONFIG_DIR=<tmp>/config envira`
  keeps state writes out of `~/.local/state`. Do not confirm plans on the host.
