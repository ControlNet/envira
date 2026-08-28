# Legacy scripts deprecation audit (2026-08-28)

Scope: `run.sh`, `run_user.sh`, and `verify.sh`. The installer scripts were not executed.

## User-directed retention decisions

- Do not install Beads. Perles and Dolt remain unchanged unless separately requested.
- Keep installing and verifying Neofetch even though its upstream repository is archived.
- Keep installing and verifying `speedtest-cli` even though its upstream repository is archived.
- Keep `git config --global credential.helper store` in both installers despite its plaintext-storage risk.
- Keep installing and verifying LunarVim from its existing Neovim 0.9 / release 1.4 branch.
- Keep a conditional `~/.local/bin/spf` to `/usr/local/bin/spf` link for the installer's user-level fallback without replacing a successful system-level install.
- Keep the existing CentOS btop `v1.4.5` pin and EndeavourOS `yay -Sy --noconfirm docker` command; these were not part of the approved update scope.

## Confirmed conclusions

- Gemini CLI has **not** been discontinued. The official `google-gemini/gemini-cli` repository is active, its latest stable release is `v0.57.0` (2026-08-25), and npm package `@google/gemini-cli` is current at `0.57.0`.
- Antigravity CLI is a separate official Google CLI, not a rename or replacement package for Gemini CLI. Its official repository is `google-antigravity/antigravity-cli`; latest release was `1.1.22` (2026-08-27). The official Linux/macOS installer is `curl -fsSL https://antigravity.google/cli/install.sh | bash`, and the command is `agy`. There is no official `@google/antigravity-cli` npm package. The unscoped npm package `antigravity-cli` is only a `0.0.1` placeholder and must not be installed.

## Confirmed deprecated, removed, or archived items

- `huggingface-cli` has been removed from `huggingface_hub` v1.x and replaced by `hf`. `run.sh` still creates a system symlink to `~/.local/bin/huggingface-cli`; `verify.sh` correctly accepts either command but should now prefer or require `hf`.
- PyPI package `neovim` is a transition package for `pynvim`, last released as `0.3.1` in 2018. Both installers should use `pynvim`, not `neovim`.
- Yazi deprecated `ya pack` in favor of `ya pkg`. The current official crates.io method is `cargo install --force yazi-build`; its build script installs `yazi` and `ya` as side effects, so the script's unforced first-install form can work but will not reliably refresh an existing install. The old `[flavor] use = "onedark"` form also changed to `dark` / `light` configuration. In addition, `BennyOe/onedark` no longer exists on GitHub. The current Yazi flavor block and its verification assertions are obsolete.
- `dylanaraps/neofetch` is archived. `run_user.sh` still installs pinned Neofetch `7.1.0`; `run.sh` still installs it on distributions where a package remains available. Fastfetch is already installed and is the suitable maintained replacement.
- `sivel/speedtest-cli` is archived, and its latest release is `2.1.3` from 2021. It should be removed or replaced by the maintained official Ookla Speedtest CLI if this capability remains desired.
- Oh My Zsh's current update configuration uses `zstyle ':omz:update' mode ...`. The legacy `DISABLE_UPDATE_PROMPT=true` compatibility variable means automatic update without a prompt; it does not disable updates. The current comment and behavior contradict each other.
- Codex's current configuration reference does not define a top-level `network_access` key. Network access in the workspace-write sandbox is `sandbox_workspace_write.network_access`; web search is configured separately with top-level `web_search`. The installers append `network_access = true`, and `verify.sh` only checks for this stale text rather than effective configuration.

## Stale pins or redundant setup (not necessarily formally deprecated)

- Neovim is pinned to `v0.9.5`; current stable is `v0.12.5`.
- LunarVim is pinned to its Neovim 0.9 / release 1.4 branch. Its latest release is `1.4.0` from 2024-05-15 and its latest repository commit is from 2025-06-05. The project is not archived, so classify it as dormant/stale rather than formally deprecated.
- Nerd Fonts Meslo is pinned to `v2.1.0`; current Nerd Fonts release is `v3.5.1`.
- `run_user.sh` pins bat `v0.25.0` while current is `v0.26.1`; the CentOS branch in `run.sh` pins the much older `v0.7.1`.
- `run_user.sh` pins ncdu `2.3`; the upstream site currently publishes `2.9.2` source and `2.9.1` Linux binaries.
- ctop `v0.7.7` is still its latest release, but the release dates from 2022 and the repository has had no push since 2024. It is stale, not superseded by a newer ctop release.
- Conda has used libmamba as its default solver since conda 23.10. Installing `conda-libmamba-solver` and setting `solver libmamba` after installing the latest Miniconda is redundant.
- The old Superfile URL redirects successfully to the current official `https://superfile.dev/install.sh`; it works but should use the canonical URL.
- The current Superfile installer installs an executable named `spf`. `run.sh` subsequently creates a `superfile` symlink pointing to the nonexistent `~/.local/bin/superfile`, so that compatibility symlink is dangling even though `spf` may work.
- The old Beads repository path redirects to the current `gastownhall/beads` repository; it currently works but is no longer canonical.

## Reliability and security findings

- Both installers lack `set -e` / explicit failure aggregation. Earlier failed installs can be hidden by later successful commands. `bash -n` only proves syntax validity.
- `verify.sh` checks command presence, not version, provenance, or functional behavior. It cannot detect stale/deprecated tools.
- Several installed tools are absent from verification, including Antigravity (not installed yet), Herdr, OpenChamber, Oh My Pi, Beads, Perles, Dolt, Texlab, Infisical, zoxide, micro, and btop.
- `git config --global credential.helper store` persists Git credentials in plaintext and should not be enabled by a general environment installer.
- Many remote installer scripts are executed directly from mutable branches or URLs. This is current for some upstreams but is not reproducible and increases supply-chain risk.
- Repeated runs are not idempotent: unguarded clones, appended shell configuration, `groupadd docker`, fixed target directories, and unforced symlink creation can fail or duplicate configuration.
- `run.sh` copies Rust and Cargo-installed executables into `/usr/local/bin`; subsequent toolchain updates leave stale system-wide copies.
- The Arch branch uses `pacman -Sy`, which risks partial upgrades. The EndeavourOS branch appears after a broad `grep "Arch"` test and may be unreachable because EndeavourOS advertises Arch compatibility in `/etc/os-release`.
- The CentOS package list includes Debian package `build-essential`, so that branch is not reliable.
- Pixi `0.59.0` reports that `[tool.pixi.project]` and `[tool.pixi.build-dependencies]` in `pyproject.toml` are deprecated; use `workspace` and regular `dependencies`, respectively. This is outside the three shell scripts but was exposed by their repository verification command.

## Verification performed

```bash
bash -n run.sh run_user.sh verify.sh
pixi run test
```

Expected signals: all scripts parse successfully and the repository unit tests pass. These checks do not execute the installers or prove remote install compatibility.

## Primary evidence

- Gemini CLI: https://github.com/google-gemini/gemini-cli and https://www.npmjs.com/package/@google/gemini-cli
- Antigravity CLI: https://github.com/google-antigravity/antigravity-cli and https://antigravity.google/docs/cli/overview
- Codex configuration: https://developers.openai.com/codex/config-reference/
- Hugging Face v1 migration: https://github.com/huggingface/huggingface_hub/blob/main/docs/source/en/concepts/migration.md
- Yazi changelog: https://github.com/sxyazi/yazi/blob/main/CHANGELOG.md
- Oh My Zsh update configuration: https://github.com/ohmyzsh/ohmyzsh#manual-updates
- Neofetch: https://github.com/dylanaraps/neofetch
- Speedtest CLI: https://github.com/sivel/speedtest-cli
