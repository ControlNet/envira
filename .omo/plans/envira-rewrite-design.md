# envira Rewrite Design (Rust + ratatui)

Status: M1–M4 implemented on `feature/rust-cli` (2026-10-29); M5 open. Supersedes the architecture on
`origin/feature/cli-app`.

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
  The logs directory is 0700. Each unit log starts with `# envira <v> · run <id> (<command>) ·
  <machine>` and `# started <time>`, prefixes every line with `HH:MM:SS`, and ends with
  `# finished <outcome>[: detail] in <duration>`. Values of secret-looking environment variables
  (`*TOKEN*`, `*SECRET*`, `*PASSWORD*`, `*API_KEY*`, ...; 8+ chars) are replaced by `***` in logs and
  the TUI. `run.toml` lists every planned unit as `pending` before the first step and records the
  envira version and machine summary.
- Run saves take the selection (bundles, include, exclude) from disk before writing, so a bundle
  subscribed elsewhere during a run survives it.
- `lock`: `flock` held for the length of a run (sync, install, update, retry, config, adopt, TUI
  runs). A second run fails at once with "another envira run is in progress". `envira fs` and
  `fetch-release`, which steps invoke, never take it.
- A `state.toml` that does not parse is moved to `state.toml.broken-<time>` with an error that says
  so; the next start begins fresh. Writes are fsync'd before the atomic rename.
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
   A step running longer than the step timeout (30 min; `--step-timeout` / `ENVIRA_STEP_TIMEOUT`)
   is cancelled like `c` and fails with "timed out after ...". Release downloads and latest
   queries retry connection errors, timeouts, 429 and 5xx with backoff (3 tries from 2 s for
   downloads, 2 tries for queries; `Retry-After` up to 30 s); 403/404 are never retried.
   `fetch-release` checks the download's SHA-256 against GitHub's asset `digest` (recorded for
   uploads since 2025), else against a published `<asset>.sha256` or `checksums.txt` /
   `SHA256SUMS`; a mismatch fails the step, a release without either is noted in the log.
   A failed step's last 40 output lines are diagnosed (`src/diagnose.rs`): network, busy
   dpkg/apt lock, GitHub rate limit, not found, disk full, permission, sudo, PEP 668, missing
   command. The log gets `[envira] hint (<kind>): <what to do>`, the run record a `hint`, the
   failure detail the kind. Transient kinds (network, busy package manager) retry the step
   (`--retries` / `ENVIRA_RETRIES`, default 2; 5 s then 15 s, 6x for a busy apt; the wait ends on
   stop/cancel; the step timeout is per attempt).
   Plans warn when the home, temp dir or (system units) `/usr/local` / `/var` has < 500 MB free.
9. Regenerate the managed shell environment from the resulting state.
10. Crash, Ctrl-C, SIGTERM or SIGHUP mid-run: state is per-unit and `state.last_run` points at the
    run from its start. Ctrl-C stops after the current step (twice cancels it); SIGTERM/SIGHUP
    cancel the current step and stop, in the CLI and the TUI, so no installer outlives envira.
    `envira retry` (TUI: `r` on the summary, `t` on the dashboard) replans everything the last run
    did not finish: failed, blocked, cancelled, skipped after a stop, and `pending` units of a run
    that died.

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
envira                      # TUI on a terminal, `status` otherwise
envira status [--json] [--latest [--refresh]]   # latest versions: cached 6 h, network only with --latest
envira sync   [--bundle B]... [-y] [--dry-run] [-q]
envira install <item|bundle>... [--force] [-y] [--dry-run] [-q]
envira retry  [-y]          # every unit the last run did not finish
envira config apply|revert <config>... [-y]
envira plan   [<item|bundle>...] [--script]
envira env                  # regenerate shell files and rc blocks only
envira log    [<item>] [--run <id>] | --list   # --list: kept runs, finished/running/interrupted
envira explain <item>       # detect/check commands with exit codes and output, ownership,
                            # last result, and what (re)installing would run
envira doctor               # facts, PATH, rc integration and duplicates, catalog layers
envira catalog check|schema|show
envira update [<item|bundle>...] [--check] [--refresh | --offline] [-y] [--dry-run] [-q]
                            # no items: every outdated managed/adoptable tool
envira adopt  [<item|bundle>...] [--dry-run]   # record adoptable tools as managed, no reinstall
Global: --catalog <path>, --no-overlay, --scope user|system, --step-timeout <seconds>, --retries <n>
Hidden, used by templates: envira fs ..., envira fetch-release ...
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

## Catalog format spec (v1, as implemented)
The source of truth is the Rust types in `src/catalog/schema.rs`; `envira catalog schema` prints the
JSON Schema (committed as `catalog/schema.json`, referenced by `#:schema ./schema.json` in every file).

**Files and layers.** `catalog/*.toml` are merged in name order into one document; every table is
merged per field (arrays are replaced, not appended). Items without `category` get the file name.
Base layer: `--catalog <file|dir>` (or `ENVIRA_CATALOG`, `:`-separated) if given, else the embedded
snapshot; then `~/.config/envira/catalog.d/*.toml` (skip with `--no-overlay`). The remote layer is M5.
The same id defined twice inside one layer is a warning.

**Top level.** `schema = 1`, `min_envira = "0.1.0"`, `[env]`, `[methods.*]`, `[actions.*]`,
`[tools.*]`, `[configs.*]`, `[bundles.*]`. Ids of tools and configs share one namespace.

**`[env]`.** `path`: dirs prepended to PATH for detection and execution; `shell_path`: dirs always
rendered into `env.sh`; `bin_dir = { user, system }`: the `{bin_dir}` builtin per scope.

**Templates.** Plain shell with `{name}` placeholders, filled from the entry's parameters (after the
template's `defaults`) and the builtins `arch home user bin_dir sudo envira id bin`.
- Shell context quotes values (safe words stay bare); `installed.regex` regex-escapes them.
- `{sudo}` expands to `sudo -n` for system scope when not root, else to nothing (one adjacent space is
  dropped); checks always expand it to nothing, since checks are read-only.
- Parameter values may use builtins (`asset = "yazi-{arch}-…"`) and a leading `~/`. An array value
  becomes several words; a table keyed by arch (`{ x86_64 = "…", aarch64 = "…" }`) picks one, and a
  missing arch makes that entry infeasible.
- `${…}` belongs to the shell; `{{` / `}}` produce literal braces (`rpm --qf '%{{VERSION}}'`).
- Raw commands (`run`, `check`, `apply`, `revert`, `detect.cmd`) get builtins only.

**`[methods.<name>]`.** `desc`, `when`, `scope = "user" | "system" | "any"`, `expects` (lint only),
`defaults`, `install`, optional `update` (default: `install` again), `latest` and
`installed = { cmd, regex }` (adoption; its first group is the version the method installed).

**Queries** (`latest`, `installed`): `{ cmd | http, json?, regex? }`. `http` is a GET (User-Agent set;
`GITHUB_TOKEN` for api.github.com) whose placeholders are inserted verbatim; `json` is a dotted path
(numbers index arrays) into the answer; `regex`'s first group is the version, default a generic
version pattern (so tags like `v0.25.0` and `bun-v1.4.4` need no regex). Answers are cached per tool
and expanded query in `latest.toml` for 6 hours (failures for 30 minutes). Install entries may set
their own `update` and `latest`, which override the method's; raw `run` entries only get builtins.

**Updates go where the tool lives.** An update runs the `update` template of the install entry the
tool came from, in that entry's scope (the route):
- managed: the method and scope recorded in state;
- adoptable via an `installed` query: that method, in its own scope;
- adoptable by path (binary under `$HOME`, no query matched): the first usable user-scope entry whose
  method has no `installed` query and, if its install template uses `{bin_dir}`, whose user bin dir
  holds the binary.
Routes need the entry's dependencies present and, for system scope, sudo; otherwise the tool is
`external` and never touched. "Outdated" compares `latest` with the version the method reports,
falling back to the detected version.

**`[actions.<name>]`.** `desc`, `defaults`, `check` (exit 0 = in place), `apply`, optional `revert`.
The library calls the `envira fs` primitives: `write`, `block`, `toml-set`, `ini-set`, `line`, `link`
(each with `--check`) and `restore [--key K]`. `line FILE --match RE --line TEXT [--after RE]`
replaces the first line matching `RE`, or inserts after the first `--after` match (appends when
there is none); the previous line is recorded under the key `RE` for `restore --key`.

**`[tools.<id>]`.** `desc`, `category`, `bin` (default: id), `bins` (default: `[bin]`; used for
`command -v` and portable copies), `detect = { cmd, version }` (default `{bin} --version` and a
generic version regex), `depends`, `when`, `shell = { path, env, alias, init = { zsh, bash } }`, and
`install = [ … ]` in order of preference. An entry is either `{ method = "…", <params> }` or
`{ run = "…" }`, plus optional `depends` (needed only for this entry), `when`, `scope`, `portable`.

**`[configs.<id>]`.** `desc`, `category`, `depends`, `when`, `scope` (default user), and either
`steps = [ { use = "<action>", <params> } | { run = "…", check = "…", revert = "…" } ]` or raw
`check` / `apply` / `revert`. Status = all checks pass. A step whose check passes is skipped on apply.
Raw `apply` needs `check`, and a config without any check is rejected.

**`[bundles.<id>]`.** `name`, `desc`, `items` (tool or config ids), `bundles` (other bundles included
whole; flattened in order without duplicates, cycles rejected).

**`when` predicates** (all must hold; `!` negates): `gpu`, `container`, `wsl`, `sudo`, `root`,
`distro:<ID>`, `family:<ID or ID_LIKE>`, `arch:<arch>`, `shell:<name>`.

**Choosing a method** (per tool, first feasible entry wins): the entry's and the method's `when` hold;
scope resolves (`any` → system when sudo is available, unless `--scope user`); system scope needs root
or sudo; every dependency is present or itself plannable; the template expands (e.g. there is an asset
for this arch). Otherwise the tool is "n/a" with the most specific reason. A `portable` entry with
user scope on a sudo machine gets an extra step that copies its `bins` into the system bin dir.

**`envira catalog check`** rejects: unknown methods, actions, dependencies and bundle items; a dependency
cycle (item and entry `depends` together); a missing template parameter; non-builtin placeholders in raw
commands; invalid regexes or predicates; a tool without install entries; a config without any check.
It warns about parameters that no template uses, about `expects` not covered by `depends` (directly or
through dependencies of dependencies), and about duplicate ids.

### Format findings from porting the first 14 tools and 5 configs
1. Vendor scripts edit rc files on their own. bun's installer appends to `~/.zshrc` unless
   `SHELL=/bin/sh`, so `bun` uses a raw `run`. Each new vendor-script tool needs this check; record the
   flag in a comment (fnm: `--skip-shell`, rustup: `--no-modify-path`).
2. Vendor scripts have prerequisites: the fnm and bun installers need `unzip`. That is now an ordinary
   item that both depend on, so these explicit dependencies are not limited to toolchains.
3. Arch naming differs per project (`x86_64` / `x64` / `amd64`). Tables keyed by arch cover this
   without new builtins; bat also differs in libc (musl vs gnu on aarch64).
4. Distro-renamed binaries (`batcat`) need both `bins` and a detect fallback
   (`bat --version || batcat --version`). An alias renders only when the target exists.
5. Fresh images have no apt lists, so the apt template retries after `apt-get update`.
6. Stateful vendor commands such as `ya pkg add` need a raw step with its own `check`, which the step
   form supports. Without it, re-applying a config would fail.
7. Effective-value checks and file checks differ. The inotify config first used its own
   `/etc/sysctl.d` file, so machines set up by run.sh (line in `/etc/sysctl.conf`) showed "not
   applied". It now sets that line in `/etc/sysctl.conf` (`fs line`), plus the live value.
8. Without a TTY, tools still open `/dev/tty`: `yazi --version` blocked inside the TUI. All commands now
   run in a new session (`setsid`), so they have no controlling terminal.
9. Ownership comes from the method's `installed` query or the binary path. A tool built from source
   into `~/.cargo/bin` (yazi) is external: the github-release entry installs into `~/.local/bin`, and
   the cargo entry's query does not list it.
10. Shared libraries are items too. Node.js 25+ from fnm links against `libatomic.so.1`, which minimal
   images lack (the node binary installs but does not start). `libatomic` has `bins = []`, detects
   through `ldconfig -p`, installs the distro package, and `nodejs` depends on it.
11. The detected version can belong to another copy. A host had an old `bat` in `~/.local/bin`
   shadowing apt's `batcat`; comparing that version with apt's candidate reported apt's bat as
   outdated forever. Outdated now uses the version from the method's `installed` query.
12. Detection depends on the working directory: `rustc --version` inside a repo with
   `rust-toolchain.toml` reports the pinned toolchain. Captures now run in `$HOME`.
13. Vendor tools use exit codes as answers: `rustup check` exits 100 when an update exists, so its
   query is `rustup check || [ $? -eq 100 ]`.
14. A binary in `$HOME` is not proof of the method: node from nvm matched the fnm-based `nodejs`
   entry by path. Routes now require the entry's dependencies (fnm) to be present.
15. `latest` sources are per method where possible (crates.io, PyPI, npm registry, GitHub releases,
   `apt-cache policy`, `pacman -Si`); dnf and zypper only have `update` for now, as their query
   formats are unverified. apt and pacman answer from the local package lists (no network).
16. Not modelled yet: the uninstall of tools (out of v1); herdr has an updater (`herdr update`) but
   no latest source. `curl` is now an item: every entry that downloads with it depends on it, and the
   `script` method expects it (lint).

## Full catalog port (from run.sh / run_user.sh, 2026-10-29)
The first catalog held 14 sample items. The user asked for the real software list, so every install,
config and rc edit of `run.sh` (sudo) and `run_user.sh` (no sudo) is now an item. User decisions:
category bundles plus `full` (= run.sh) and `user` (= run_user.sh); old or redundant tools in an
opt-in `extras` bundle; `.zshrc` handled with single `line` edits plus envira's managed block; only
static binaries are copied into `/usr/local/bin`.

| Bundle | Items (from the legacy scripts) |
|---|---|
| `base` | git git-lfs curl wget zsh vim tmux screen build-tools python3 unzip p7zip xsel mosh nmap iperf3 iputils-ping net-tools pciutils openssh-server libfuse2 fontconfig apt-tools yay |
| `shell` | oh-my-zsh, zsh-autosuggestions, zsh-syntax-highlighting, zsh-theme (mzz-ys + ZSH_DISABLE_COMPFIX), zsh-plugins, zsh-settings (DISABLE_UPDATE_PROMPT, TERM), default-shell (chsh), tmux-colors, git-settings (alias.lsd, credential.helper store) |
| `toolchains` | rust-toolchain, cargo-binstall (+ BINSTALL_* env), cargo-cache, go (GOROOT ~/.go, GOPATH ~/go), fnm, libatomic, nodejs, npm-prefix (~/.npmrc prefix ~/.local), pm2, pipx, uv, pixi + pixi-config, bun |
| `terminal` | bat + bat-link, ncdu, fzf, lazygit, zellij + zellij-config (fix_zellij_kdl.py), herdr, lsd, delta, duf, dust, fd, ripgrep (pcre2), gping, procs, xh, viu, rustscan, scc, gotify, zoxide, micro, superfile + superfile-config, yazi + yazi-theme, tldr, rich-cli, speedtest-cli, gdown, gh |
| `editors` | neovim (~/.nvim), lemonade, texlab |
| `ai-agents` | codex + codex-sandbox-network, antigravity (agy), cursor-agent (agent), claude, opencode, oh-my-opencode, openchamber, omp |
| `monitoring` | btop, bottom, nvitop, nviwatch, bandwhich |
| `containers` | docker, docker-service (Arch, openSUSE), docker-group, lazydocker, dive, ctop |
| `data-science` | miniconda, conda-settings (changeps1), jupyter (TOS + jupyterlab), hf (+ hf_transfer) |
| `secrets` | infisical (+ INFISICAL_* env, ienv init), ienv |
| `system` | inotify-watches (`/etc/sysctl.conf` line + live value) |
| `extras` | lunarvim, gedit, tigervnc, meslo-font, screenfetch, neofetch, fastfetch, archey4, genact |

`full` nests every bundle except `extras`; `user` lists the run_user.sh items (no system packages,
docker, ctop, bandwhich, chsh or inotify).

**Modelling rules.**
- Package lines become one item per user-visible command, with the package names run.sh uses per
  family; a family run.sh does not cover gets no entry. Groups (build-tools, python3, apt-tools,
  tigervnc) use an array `package`; the first package's version is reported.
- System scope first, user fallback, as the two scripts do: bat/btop/ctop from GitHub releases,
  ncdu's static build, neofetch from source, pipx via `pip --user`.
- Vendor scripts are kept where run.sh uses them; every curl-using entry depends on `curl`.
- Aliases, env and init lines that run.sh appended to `.zshrc` are `shell` fields of the tool, so
  they render only while the tool is present.
- Crates, Go modules and release binaries are `portable` (copied to `/usr/local/bin` with sudo);
  pipx and npm tools stay in `~/.local/bin`.
- Versions follow the latest release, except LunarVim (fixed branch), ncdu's static build (no
  "latest" link) and neofetch (archived at 7.1.0).

**Deliberate differences from run.sh.**
| run.sh | catalog | Why |
|---|---|---|
| `goinstall.sh` (Go 1.25.5, edits rc files) | download the newest Go from go.dev into `~/.go` | the script pins a version and appends to rc files with no opt-out |
| `curl https://sh.rustup.rs \| sh -s -- -y` | adds `--no-modify-path` | PATH comes from envira's env.sh |
| `conda init zsh` | condabin on PATH + `conda shell.zsh hook` init | no rc edits |
| fzf `install --all` | `--all --no-update-rc` + init sources `~/.fzf.zsh` | same |
| pixi installer | `PIXI_NO_PATH_UPDATE=1` + `shell` | same |
| bun/fnm rc lines | `SHELL=/bin/sh`, `--skip-shell` + `shell` | same |
| `webi.sh/gh` | GitHub release `cli/cli` | webi edits rc files through envman |
| `cargo install genact`, binstall for the rest | cargo method (binstall when present) for all crates | except ripgrep (pcre2) and yazi-build, built from source |
| copies of rustup, cargo, go, gofmt to `/usr/local/bin` | not copied | proxies only work for the user whose `~/.rustup` / `~/.go` they read |
| symlinks of pipx/npm tools (uv, gdown, tldr, hf, pm2, …) | none | user decision: static binaries only |
| neovim symlinked from `/usr/local/bin` into `~/.nvim` | linked in `~/.local/bin` | a system path pointing into one user's home |
| `sed` of the commented `# export PATH=…` line in .zshrc | not done | PATH comes from envira's env.sh |
| `sudo update-pciids`, `cargo cache -a` | not done | refreshes and cleanups, not machine state |
| aliases `top=btop`, `du=dust`, `ping=gping`, `ps=procs`, `nano=micro` | not set | user decision 2026-10-29: only `zel`, `ls`, `df` for now; the rest is to be revisited |
| `cargo install --force yazi-build` | prebuilt `sxyazi/yazi` release first, `yazi-build` as fallback | user decision 2026-10-30: compiling took 6 of the 14 minutes of a CI full sync and minutes on real machines; the release has both `yazi` and `ya` |
| pinned bat 0.26.1, btop 1.4.7, nvim 0.12.5, ctop 0.7.7 | latest | updates are handled by `envira update` |

## Legacy script parity (long-term parallel maintenance)
The unit test `catalog::load::tests::catalog_covers_verify_sh` parses `verify.sh` (`check_cmd`,
`check_optional_cmd`, `check_any_cmd`) and fails when a checked command is not in any tool's `bins`.
It also fails when an item is in neither `full` nor `extras`, or `user` has an item outside them.
This is a drift alarm, not a translator; both sides stay hand-written. It found the missing `pm2`.

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
  main.rs, cli.rs (clap), commands.rs (headless commands), app.rs (session: catalog, facts, runner)
  catalog/   (schema.rs, load.rs layers + merge, validate.rs)
  facts.rs, template.rs (expansion, quoting, `when`), plan.rs (method choice, deps, order)
  runner.rs  (Runner trait; real runner: bash -eo pipefail in its own session; fake for tests)
  status.rs  (detection, ownership, labels), exec.rs (units, blocking, state per unit, shell env)
  state.rs, fs.rs (`envira fs`), fetch.rs (`envira fetch-release`), shellenv.rs
  tui/       (mod.rs event loop, app.rs state + keys, ui.rs BC1 rendering, theme.rs)
catalog/*.toml, catalog/templates.toml, catalog/schema.json
tests/integration/ (Docker: run.sh, check.sh, full.sh, Dockerfile.{ubuntu,fedora,arch,opensuse})
```

## Testing
- Unit: catalog validation (unknown deps, cycles, missing methods), plan (topo order, blocking,
  present-dep satisfaction, method fallback by privilege), shellenv rendering (snapshot), state round-trip,
  backend step construction and version parsing.
- Executor through a `Runner` trait with a fake implementation for failure/blocking/retry flows.
- TUI: `ratatui::backend::TestBackend` for key flows only (dashboard filter, plan confirm, run summary).
- Integration (`tests/integration/run.sh`, in CI): `check` installs a few items, then tests status,
  idempotence, shell integration, update, adopt and recovery (lock, SIGTERM, retry, timeout, a flaky
  download); `full` syncs whole bundles and runs `verify.sh`. Both run with and without sudo, on
  Ubuntu, Fedora, Arch and openSUSE images (`DISTRO`).

## Milestones
1. **M1 status (headless)** (done): catalog schema + templates + facts + detect + `status` / `doctor` /
   `catalog check`; port ~12 representative items covering every method and action template.
2. **M2 execute** (done): plan + exec + state + logs; `sync`, `install`, `retry`; configs apply / revert;
   `envira fs` primitives; shellenv + rc block.
3. **M3 TUI** (done): dashboard, plan review, run view with live logs, first-run picker. History screen
   deferred; `envira log` covers it.
4. **M4 updates** (done): `latest` queries per method and entry with a cache, `update` in place along
   routes, `adopt`, outdated label / filter / `u` key in the TUI, background check on start.
5. **M5 delivery**: remote catalog + cache + overlays, musl release CI + maturin wheels (see Repo
   transition), Python package removal, `envira.sh` bootstrap. (The full catalog port and the
   legacy parity test were done early, see "Full catalog port".)

## Open questions
- Is the `envira` crate name free on crates.io?
- How many log runs to keep (20 now)? `status` uses cached latest versions only; the TUI checks in the
  background on start (6 h cache).
