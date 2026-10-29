# envira Rewrite Design (Rust + ratatui)

Status: draft for review (2026-10-29). Supersedes the architecture on `origin/feature/cli-app`.

## Intent
A single static binary that installs, updates, and tracks the user's Linux toolset on any machine:
fresh or long-lived, VM or bare metal, GPU or not, single- or multi-user, with or without sudo.

Problems it must solve that the legacy scripts (`run.sh`, `run_user.sh`, `verify.sh`) cannot:
1. **Updates**: show which installed tools are outdated and update them, without manual checking.
2. **Incremental adoption**: after a tool is added to the list (e.g. herdr), an old machine
   provisioned by the legacy scripts can install just the new tools.
3. **Failure isolation and recovery**: one failing tool never stops the others; retry only what failed;
   re-running never duplicates rc lines, env vars, or leaves stray files.
4. **Visibility**: a TUI that shows the state of every tool and streams install logs live.
5. **Structured catalog**: tools are data (detect / install / update / depends), not script fragments.

## Decisions
| Topic | Decision |
|---|---|
| Stack | Rust, ratatui + crossterm, tokio, clap, serde/toml, reqwest (rustls), semver |
| Distribution | musl static binaries for `x86_64` and `aarch64` (cargo-zigbuild) |
| Catalog delivery | Fetched from the published site at runtime, cached; embedded copy as offline fallback |
| Legacy scripts | Kept and hand-maintained in parallel long term; a CI parity check flags drift |
| v1 scope | Tools (install / update / sync / retry), configs (apply / revert) + managed shell environment (PATH, env, alias, init) |
| Out of v1 | dotfiles (zsh theme, oh-my-zsh plugins, tmux, git config), tool uninstall, parallel installs |
| Item kinds | `tools` (versioned, updatable; toolchains included) and `configs` (check / apply / revert) |
| Dependencies | Explicit: item-level and method-level `depends`; method `expects` is a lint, never injected |
| Methods / actions | Data templates in `catalog/templates.toml`; Rust only provides generic primitives |
| Install scope | `prefer_scope` system when sudo else user; no symlinking user installs by default |
| PyPI | Keep `envira` on PyPI as maturin `bindings = "bin"` wheels carrying the Rust binary; also crates.io and the curl bootstrap |

Lessons from the old branch, applied here:
- No version gate / auto-handoff, evidence artifacts, or verifier "profiles". Keep the model small.
- A satisfied dependency is satisfied regardless of scope; never fail planning because a
  system-scope dependency cannot be *installed* when it is already *present*.
- TUI must never block: all process I/O goes through an event channel.

## Core model

### Facts (probed once per run)
- `distro`: `/etc/os-release` `ID` plus `ID_LIKE` family (`mint`/`popos` -> `ubuntu`/`debian` family).
- `arch`: `x86_64` | `aarch64`.
- `privilege`: `root` | `sudo` (passwordless or prompt-able) | `none`.
- `gpu`: NVIDIA present (`nvidia-smi` or `/proc/driver/nvidia`).
- `container` (`/.dockerenv`, `/run/.containerenv`), `wsl`.
- `shells`: which of bash / zsh exist (for rc integration).
- `target_user`: whose home user-scope installs, state, and shell files belong to (see below).

### Invocation contract (root, sudo, multi-user)
- Normal use: run envira as the normal user; it calls `sudo` per system-scope step.
- `sudo envira` (EUID 0 with `SUDO_USER` set): user-scope installs, state, and shell files target
  `$SUDO_USER`'s home, and user-scope steps run as that user (`setpriv`/`runuser` drop), so no
  root-owned files end up in the user's home.
- Plain root (Docker, no `SUDO_USER`): user scope = root's home.
- Multi-user machines: system-scope installs affect everyone; state is per user, so another user's
  envira sees those tools as present (`external` or `adoptable`), never as missing.

### Effective PATH
Detection, backend queries (`npm ls -g`, `pipx list`), and execution all run with an effective PATH,
not the caller's: the managed dirs plus the dirs legacy installs use, mirroring `verify.sh`
(`~/.local/bin`, `~/.cargo/bin`, `~/go/bin`, `~/.go/bin`, `~/.fzf/bin`, `~/.local/share/fnm`,
`~/.pixi/bin`, `~/.bun/bin`, `~/.opencode/bin`, `~/.nvim/bin`), plus `fnm env` output when fnm exists.
Otherwise fnm-managed node and npm-backed CLI agents show as missing under `bash -lc` / Docker.

### Catalog items
Two item kinds, both plain TOML data, listed together in the TUI:
- `tools.<id>`: has an executable and a version, can be updated. Toolchains (rustup, fnm + node, go,
  pipx) are ordinary tools that other items depend on, so they install first.
- `configs.<id>`: a customization with no version (codex sandbox network, docker group, inotify watches).
  Status is applied / not applied / drifted; may depend on tools; can be reverted.

Dependencies are **explicit** (user decision, 2026-10-29). Nothing is injected automatically:
- item-level `depends`: needed whichever method is chosen;
- method-level `depends`: needed only when that method is chosen (yazi via github-release needs nothing,
  via cargo needs `rust-toolchain`).
- A method template may declare `expects` (cargo expects `rust-toolchain`); `envira catalog check`
  **warns** when an item uses that method without declaring it. Lint only, never injection.
- Inference is limited to ordering (topological sort), method choice (first feasible method whose
  dependencies are present or installable) and scope.

```toml
[tools.yazi]
desc     = "Terminal file manager"
category = "terminal"
detect   = { cmd = "yazi --version", version = 'Yazi (\S+)' }
install  = [                                   # ordered; first feasible method wins
  { method = "github-release", repo = "sxyazi/yazi", asset = "yazi-{arch}-unknown-linux-musl.zip", bins = ["yazi", "ya"] },
  { method = "cargo", package = "yazi-build", depends = ["rust-toolchain"] },
]
shell    = { alias = { y = "yazi" } }

[tools.texlab]
install = [{ method = "cargo", package = "texlab", depends = ["rust-toolchain"], portable = true }]

[tools.nvitop]
when    = ["gpu"]                              # e.g. ["gpu"], ["!container"], ["distro:ubuntu"]
install = [{ method = "pipx", package = "nvitop", depends = ["pipx"] }]

[configs.codex-sandbox-network]
desc    = "Allow network access in the codex workspace-write sandbox"
depends = ["codex"]
steps   = [{ use = "toml-set", file = "~/.codex/config.toml",
             key = "sandbox_workspace_write.network_access", value = "true" }]

[configs.inotify-watches]                      # legacy run.sh appends to /etc/sysctl.conf on every run
scope = "system"
steps = [{ use = "file", path = "/etc/sysctl.d/90-envira-inotify.conf",
           content = "fs.inotify.max_user_watches=524288\n" },
         { run = "sysctl --system" }]          # raw run steps execute on apply only

[configs.my-tweak]                             # raw form: any shell, no template
check  = "grep -qx 'foo=1' ~/.barrc"
apply  = "echo 'foo=1' >> ~/.barrc"
revert = "sed -i '/^foo=1$/d' ~/.barrc"

[bundles.terminal]
desc  = "Daily terminal tools"
items = ["bat", "lsd", "zoxide", "yazi", "herdr"]
```
A config's check is the conjunction of its template steps' checks (or its raw `check`).
Catalog header: `schema = 1`, `min_envira = "0.1.0"`. A catalog that needs a newer binary is
rejected with a clear message (fall back to cache/embedded); no automatic self-update.

### Methods and actions are data templates
Rust knows no install method or config action by name. Install methods (`[methods.*]`) and config
actions (`[actions.*]`) are templates in `catalog/templates.toml`, shipped with the catalog and
editable or extendable like any catalog file (overlay layers apply). Adding `uv tool`, `brew` or a new
action kind is a TOML edit, not a release.

```toml
[methods.cargo]
expects   = ["rust-toolchain"]                 # lint only
scope     = "user"
install   = "cargo binstall -y {package} || cargo install --locked {package}"
update    = "cargo binstall -y --force {package} || cargo install --locked --force {package}"
installed = { cmd = "cargo install --list", regex = '^{package} v(\S+):' }
latest    = { http = "https://crates.io/api/v1/crates/{package}", json = "crate.max_stable_version" }

[actions.toml-set]
check  = "envira fs toml-get {file} {key} --eq {value}"
apply  = "envira fs toml-set {file} {key} {value}"
revert = "envira fs restore {file} --key {key}"
```
- Parameters are substituted with shell quoting. `envira catalog check` expands every template use, so
  missing or unknown parameters fail at check time, not at install time.
- `installed` / `latest` take `{ http, json }`, `{ http, regex }` or `{ cmd, regex }`; GitHub requests
  use `GITHUB_TOKEN` when set. Without `latest` the tool shows "update: manual".
- Raw form is always allowed: a tool may give `install` / `update` strings directly (vendor installers:
  oh-my-zsh, fnm, miniconda); a config may give `check` / `apply` / `revert` directly. Raw `apply`
  without `check` is rejected; without `revert`, reverting reports "manual revert needed".
- The plan view and logs show the fully expanded commands.
- Default library (v1): methods `apt`, `dnf`, `pacman`, `zypper`, `cargo`, `pipx`, `npm`, `go`,
  `github-release`, `file`; actions `file`, `block`, `toml-set`, `ini-set`, `link`, `user-group`.

Generic Rust primitives the templates call:
- `envira fs block|toml-get|toml-set|ini-set|link|restore`: idempotent edits. The first modification of
  any file backs it up to `~/.local/state/envira/backups/` and records prior values for revert.
- `envira fetch-release --repo R --asset PATTERN --bin B... --to DIR`: picks the asset for the current
  arch, downloads with reqwest, extracts `.tar.gz` / `.zip` in-process (`flate2`, `tar`, `zip`), atomic
  move; bare machines need neither curl nor tar/unzip.
- HTTP fetch + JSON path / regex extraction for `installed` / `latest`.

### Scope and system-wide installs
- Machine preference `prefer_scope = "system" | "user"`: `system` by default when sudo is available
  (legacy `run.sh`: install for everyone), `user` otherwise (legacy `run_user.sh`); the TUI can override
  it per run.
- System-wide availability comes from methods that really install system-wide (`github-release` into
  `/usr/local/bin`, `pipx install --global`, `npm -g` with a system node, distro packages), **not** from
  symlinking user installs: links into `/home/<user>` break for other users (home permissions,
  `#!/usr/bin/env node` scripts, venvs, data paths relative to the executable). No linking by default.
- `portable = true` on a method marks a self-contained binary (static Rust/Go); only those may be built
  as the user and copied with `sudo install -m755` into `/usr/local/bin`.
- Self-built tools: a local overlay item with `method = "file"`, `source = "/path/to/bin"`,
  `scope = "system"` is installed, detected and tracked like any other tool.

### Status
Computed per tool from facts + detection + state + catalog:
`Installed{version, ownership}` · `Outdated{current, latest}` · `Missing` (desired, absent) ·
`Available` (not desired) · `New` (not yet seen on this machine) · `Failed` (last attempt) ·
`Blocked{dep}` · `NotApplicable{reason}` (e.g. needs sudo, `when` unmet, no method for distro/arch).
Configs: `Applied` · `NotApplied` · `Drifted` (applied by envira before, check fails now) · `Failed` ·
`Blocked{dep}` · `NotApplicable{reason}`.

Ownership:
- `managed`: installed by envira (recorded in state).
- `adoptable`: present but not in state, and envira can safely take it over. This is the normal case
  on legacy-provisioned machines, so update support depends on it. A tool is adoptable when either:
  - the method's `installed` query finds it (`pipx list`, `cargo install --list`, `npm ls -g`, `go version -m`); or
  - for `github-release` / raw-script tools: detect succeeds, the resolved binary lives in a user-writable
    managed dir (`~/.local/bin`, `~/.cargo/bin`, `~/go/bin`, or the method's declared `install_dir`),
    and no system package owns it (`dpkg -S` / `rpm -qf` / `pacman -Qo` negative).
  The first update overwrites it in place and records it as managed.
- `external`: present but neither managed nor adoptable (e.g. admin-installed under `/usr`, owned by a
  system package without sudo); shown, never touched.

### Machine state (`$XDG_STATE_HOME/envira/`, default `~/.local/state/envira/`)
- `state.toml`: selected bundles, explicit `include` / `exclude`, `seen` tool ids, and per tool
  `{method, version, installed_at, last_result}`. Written atomically after every unit.
- `logs/<run-id>/<tool>.log` plus `logs/<run-id>/run.toml` (plan + per-unit outcome). Keep last N runs.
- Desired set = expand(bundles) + include - exclude, filtered by `when` / feasibility.
- First run on a machine without state (including legacy-provisioned ones): detect everything,
  ask for bundles (TUI picker or `--bundle`), mark the whole catalog as seen.

## Reconcile and execution
1. Load catalog (remote -> cache -> embedded), probe facts, read state.
2. Detect all tools in parallel (bounded concurrency, per-command timeout).
3. Optionally check `latest` in parallel (network; cached with TTL; skipped with `--offline`).
4. Plan: target set by command (`sync` = desired & missing; `update` = outdated & managed/adoptable;
   `install X` = X), add missing deps, choose a method per tool, topologically sort.
   Present-but-unmanaged dependencies count as satisfied.
5. Privilege: if any unit is system-scope and not root, run `sudo -v` once **before** entering raw
   mode, then refresh with `sudo -n -v` every 60 s. No sudo -> system units become `NotApplicable`
   and the planner picks a user-scope method when one exists.
6. Execute units sequentially (v1). Each step runs via `bash -euo pipefail -c` in its own process
   group with: effective PATH (includes managed dirs), `DEBIAN_FRONTEND=noninteractive`,
   `ENVIRA_TMP` = per-unit temp dir (deleted afterwards). Downloads go to temp, then atomic move.
7. Post-condition: re-run detect; a unit succeeds only if the tool is now detected.
8. Failure: mark `Failed`, mark dependents `Blocked`, continue with independent units.
9. Regenerate the managed shell environment from the resulting state.
10. Crash or Ctrl-C mid-run: state is per-unit, so the next run simply re-detects and replans.
    `envira retry` replans the failed + blocked set of the last run.

## Managed shell environment
Tools never append to rc files. Catalog `shell` fields (`path`, `env`, `alias`, `init.{bash,zsh}`)
of **present** tools are rendered into:
- `~/.config/envira/env.sh`: POSIX PATH and env exports.
- `~/.config/envira/init.zsh`, `~/.config/envira/init.bash`: source env.sh, then aliases and init lines.

Simple shell side effects (bun PATH + init, zoxide init, aliases) stay as a tool's `shell` field, not
separate configs. Leftovers can never break a login shell, by two layers:
1. Generation time: only tools detected as present are rendered, so the next regeneration after an
   uninstall drops their lines.
2. Run time: every rendered block is guarded (`[ -d "$HOME/.bun" ] && ...`,
   `command -v zoxide >/dev/null && eval "$(zoxide init zsh)"`, aliases shadowing core commands behind
   `command -v`), so a tool removed by hand before regeneration is simply skipped.

Files are fully regenerated each time (idempotent by construction). One marked block is inserted into
`~/.zshrc` / `~/.bashrc` once:
```sh
# >>> envira >>>
[ -f "$HOME/.config/envira/init.zsh" ] && . "$HOME/.config/envira/init.zsh"
# <<< envira <<<
```
Legacy-script lines already in rc files are not edited automatically; `envira doctor` reports
duplicates (e.g. an existing `alias ls='lsd'`) so the user can clean them up.

## Command line
```
envira                      # TUI
envira status [--json] [--check-updates]
envira sync   [--bundle B] [-y] [--dry-run]
envira install <tool>... [-y] [--dry-run]
envira update  [<tool>... | --all] [-y]
envira retry
envira log [<tool>] [--run <id>]
envira env                  # regenerate shell files only
envira doctor               # facts, PATH issues, rc duplicates, catalog source
envira catalog [--refresh]
Global: --offline, --catalog <path|url>, --json, -y
```
Headless commands are the same engine as the TUI; `sync -y --bundle full` is the Docker / CI path.

## TUI
Visual direction (chosen 2026-10-29, design "BC1"; mockups live in a private design canvas, not in the
repo): lazygit-style numbered panels (`[1]`..`[5]`, digits jump focus, focused panel border
green) combined with btop-style gauges: an `[1] Overview` strip of counts + line gauges on top, per-bundle
progress bars in the bundles panel, a selection panel that previews the plan, lazygit-style key hint bar
at the bottom. Default palette Tokyo Night; theme is a config key, with automatic Catppuccin Latte on light
terminals and ANSI-16 fallback without truecolor. Status always carries a glyph (✓ ↑ ✗ ◆ ! ⊘ –), never color alone.

Architecture: tokio runtime; one `mpsc<AppEvent>` fed by crossterm `EventStream`, executor output,
detection/latest results, and a tick. The UI thread only folds events into `AppState` and redraws.

Screens:
1. **Dashboard**: left = categories/bundles with counts; main = `Table` (select, name, status, version,
   latest, scope, via) with `/` filter and status filters (missing / outdated / new / failed);
   bottom = detail of the focused tool (desc, deps, method chosen, shell changes, why N/A).
2. **Plan review** (modal): ordered units, commands per unit, files touched, sudo needed; confirm.
3. **Run**: left = queue with per-unit state and elapsed time; right = live log of the focused unit
   (follow mode, scrollback); cancel current unit (kills process group); summary with `r` = retry failed.
4. **History / logs**: previous runs and per-tool logs.
5. **First-run picker**: choose bundles for this machine.

Keys (dashboard): `space` toggle, `s` sync missing, `u` update selected/outdated, `i` install selected,
`/` filter, `tab` switch pane, `l` logs, `R` refresh/check updates, `?` help, `q` quit.

## Data-driven catalog
Principle: **the binary contains no per-tool knowledge.** Tools are nouns in TOML data; Rust only
implements generic verbs (backends, post-install action kinds, executor, TUI). No `if tool == "..."`
anywhere in code.
- Adding or changing a tool (versions, commands, deps, bundles) = edit TOML only, no rebuild.
- Adding a new backend or action kind = code change + release.

Format: TOML (comments, multi-line strings for scripts, mature serde support). One section per tool;
repo source split by category: `catalog/<category>.toml`.

Load order, later layers override earlier ones (merge by tool id, field-level override):
1. Embedded snapshot: the same TOML packed at release time; used only when offline with no cache.
2. Remote: published `catalog.toml` from `boot.controlnet.space`, fetched with ETag into
   `~/.cache/envira/catalog.toml`; failure falls back to cache, then embedded.
3. Local overlay: `~/.config/envira/catalog.d/*.toml` for machine-specific tools or field overrides,
   effective immediately, never published.
4. Explicit: `--catalog <path|url>` (dev loop points at the repo's `catalog/` dir).
`doctor` and the TUI header show which layers are active.

Editing ergonomics:
- A JSON Schema generated from the Rust catalog types (`schemars`), referenced from the TOML files, so
  taplo / Even Better TOML give completion and inline errors.
- `envira catalog check [path]`: schema, unknown deps, cycles, tools with no feasible method per
  distro/arch, reported with file:line; runs in CI.
- TUI `r` reloads all catalog layers without restarting.
- Release: `release_web.yaml` publishes the merged `catalog.toml` next to the binaries; `build.rs`
  embeds the same merged file as the snapshot.

## Legacy script parity (long-term parallel maintenance)
CI check (`tests/legacy_parity.rs` or a small script) that:
- collects command names from `verify.sh` `check_cmd` / `check_optional_cmd` lines;
- collects detect binaries from the catalog;
- fails on any difference not listed in `catalog/legacy-parity-allow.toml` (with a reason per entry).
This is a drift alarm, not a translator; both sides stay hand-written.

## Repo transition and release pipeline
The old branch broke releases by deleting `release.yaml` without a replacement. Here:
- Python package (`envira/`, `cli.py`, `pyproject.toml`, `pixi.lock`, `test/test_impl_dispatcher.py`,
  `test/test_modular_cli.py`) is removed when the Rust crate replaces it. The PyPI `envira` name is kept:
  maturin `bindings = "bin"` builds per-platform wheels carrying the Rust binary (as ruff / uv do), so
  `pipx install envira` / `uv tool install envira` get it; `cargo install envira` too (crates.io name
  availability unverified). `test/test_legacy_scripts.py` and `test/test_ci_workflows.py` keep running
  until ported to Rust tests, so the Python test job needs a minimal environment until then.
- `release.yaml` is rewritten: `cargo test` job, then a `cargo-zigbuild` matrix
  (`x86_64-unknown-linux-musl`, `aarch64-unknown-linux-musl`) producing `envira-<arch>` +
  `envira-<arch>.sha256` and the merged `catalog.toml`, uploaded as the artifact `release_web.yaml`
  consumes; a maturin job publishes the wheels to PyPI from the same build. The legacy test matrix keeps gating releases.
- `envira.sh` downloads the binary for `uname -m`, verifies the checksum, installs to `~/.local/bin`.
- All of this lands in M5 together; until then, main's existing release flow stays untouched.

## Crate layout
```
src/
  main.rs, cli.rs
  catalog/   (schema, load, merge, validate)
  facts.rs
  template/  (method/action templates: expand, shell quoting, installed/latest queries)
  fs.rs      (`envira fs` idempotent edit primitives, backups, revert records)
  fetch.rs   (`envira fetch-release`: arch asset selection, download, extract)
  detect.rs, status.rs
  state.rs   (state.toml, run records, logs)
  plan.rs    (target selection, deps, method choice, topo sort, blocking)
  exec.rs    (step runner, process groups, temp dirs, sudo keepalive)
  shellenv.rs
  tui/       (app.rs event loop, dashboard.rs, plan.rs, run.rs, history.rs, widgets.rs)
catalog/*.toml, catalog/templates.toml
```

## Testing
- Unit: catalog validation (unknown deps, cycles, missing methods), plan (topo order, blocking,
  present-dep satisfaction, method fallback by privilege), shellenv rendering (snapshot), state round-trip,
  backend step construction and version parsing.
- Executor through a `Runner` trait with a fake implementation for failure/blocking/retry flows.
- TUI: `ratatui::backend::TestBackend` for key flows only (dashboard filter, plan confirm, run summary).
- Integration: reuse `.github/integration-test/` Docker images: `envira sync -y --bundle core` then
  `envira status --json` must report all core tools installed; run twice to prove idempotence.

## Milestones
1. **M1 status (headless)**: catalog schema + templates + facts + detect + `status` / `doctor` /
   `catalog check`; port ~12 representative items covering every method and action template.
2. **M2 execute**: plan + exec + state + logs; `sync`, `install`, `retry`; configs apply / revert;
   `envira fs` primitives; shellenv + rc block.
3. **M3 TUI**: dashboard, plan review, run view with live logs, first-run picker.
4. **M4 updates**: `latest` for all backends, `update`, outdated filter, adoption of existing installs.
5. **M5 delivery**: remote catalog + cache + overlays, musl release CI + maturin wheels (see Repo
   transition), Python package removal, `envira.sh` bootstrap, legacy parity check; port the remaining catalog.

## Open questions
- Is the `envira` crate name free on crates.io?
- How many log runs to keep, and should `status` check updates by default (network cost)?
