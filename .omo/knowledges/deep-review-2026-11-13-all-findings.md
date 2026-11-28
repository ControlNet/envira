# Consolidated review inventory (2026-11-13)

**Status (2026-11-28):** every finding below was fixed in `573f824`, and the gaps the fix verification found (F1-F7) in `dca939d`; both shipped in 0.1.6. See `deep-review-fix-verification-2026-11-13.md`.

Reviewed commit: `491f7e3`, envira 0.1.5. The three passes described
here added only review knowledge notes. Production source, catalogs, project rules and real user configuration are unchanged.

The inventory contains **23 reproduced defects: 4 P1 and 19 P2**, plus **2 code-inspection
design issues**. P1 means high-priority risks to data, confidentiality or reliable execution;
P2 means a reproducible functional defect or an important failure/recovery boundary. This is
the complete list of established findings from these passes, not a claim to have found every
possible defect in the project. Investigative hypotheses without sufficient evidence are not
listed as confirmed bugs.

Detailed evidence, conditions, limitations, repair directions and executable reproductions:

- [Round 1, R1-R10](deep-review-2026-11-13.md)
- [Round 2, R11-R18](deep-review-2026-11-13-round2.md)
- [Round 3, R19-R23](deep-review-2026-11-13-round3.md)

## All reproduced defects

| ID | Priority | Trigger and observed consequence | Main location |
| --- | --- | --- | --- |
| R1 | P1 | A private file's previous key value is copied into metadata with default permissions; a 0600 source produced 0664 metadata. Other users can read it if ancestors are traversable. | `src/fs.rs:94`, `src/files.rs:79` |
| R2 | P1 | Apply skips a pre-existing resource, but revert runs its inverse anyway. The real zsh-theme reproduction deleted a pre-existing theme; git-settings attempted to restore a key never backed up. | `src/exec.rs:700`, `src/plan.rs:976` |
| R3 | P2 | A partly applied or drifted config fails current presence checks, so normal revert skips it as not applied, leaving earlier changes behind. | `src/plan.rs:960` |
| R4 | P1 | TUI independently builds and concatenates install/update plans, losing cross-plan dependency edges. A dependent install ran before its dependency's update finished. | `src/tui/app.rs:612`, `src/plan.rs:183` |
| R5 | P2 | Detecting external Go emits GOROOT pointing to the envira layout. Sourcing the generated environment broke a working external Go with a missing-GOROOT error. | `catalog/toolchains.toml:42`, `src/shellenv.rs:69` |
| R6 | P2 | Package version extraction/comparison loses revisions and epochs. Synthetic APT 1.2.3-1 to 1.2.3-2 was reported current. | `catalog/templates.toml:64`, `src/version.rs:8` |
| R7 | P2 | Shell PATH dedup leaves a managed directory at its old position while detection prepends it. The interactive shell still selected the system copy. | `src/shellenv.rs:60`, `src/status.rs:254` |
| R8 | P2 | A binary anywhere under HOME can be assigned an incompatible raw installation route. A fixture at ~/go/bin/fzf was updated through nonexistent ~/.fzf. | `src/plan.rs:382`, `src/status.rs:466` |
| R9 | P2 | Shell environment generation fails after successful units, but the command exits 0 and records no failed stage for retry. | `src/exec.rs:430`, `src/commands.rs:1140` |
| R10 | P2 | Changing the remote catalog URL reuses another source's cached catalog/ETag; the new source need not be contacted. | `src/app.rs:236`, `src/catalog/remote.rs:44` |
| R11 | P1 | Go update deletes the old installation before extracting the replacement. A successfully transferred invalid archive left no previous Go binary. | `catalog/toolchains.toml:41` |
| R12 | P2 | Corrupt backup metadata is treated as missing; a subsequent edit overwrites the original backup with already-modified content. | `src/fs.rs:89`, `src/fs.rs:131` |
| R13 | P2 | Exported scripts omit native execution's temporary cwd. The same action preserved caller data natively and overwrote it when exported. | `src/plan.rs:1065`, `src/runner.rs:765` |
| R14 | P2 | Normal-user apply and sudo-entrypoint revert resolve different backup namespaces. Revert failed with no backup although the root-side backup existed. | `src/runner.rs:167`, `src/fs.rs:14` |
| R15 | P2 | Retry uses ordinary update presence requirements. When a failed update removed the executable, retry skipped it as not installed. | `src/commands.rs:814`, `src/status.rs:139` |
| R16 | P2 | Concurrent TUI refreshes have no generation check. A late old response changed the saved version from 3.0.0 back to 2.0.0. | `src/tui/mod.rs:184`, `src/tui/mod.rs:212` |
| R17 | P2 | Line checks inspect only the first match. A later conflicting shell assignment remained effective although check and edit both succeeded. | `src/fs.rs:616`, `catalog/shell.toml:49` |
| R18 | P2 | Bootstrap mv treats an existing destination/staging directory as a directory target. It exited 0 with installed output while envira remained a directory. | `envira.sh:78` |
| R19 | P2 | Managed state stores the method but no stable install-entry identity. After raw-route reordering, an A-installed tool ran B's updater. | `src/plan.rs:357`, `src/state.rs:89` |
| R20 | P2 | Restoring the final key removes the entire per-file backup, even when an earlier whole-file modification still requires it. Whole-file restore then failed. | `src/fs.rs:723` |
| R21 | P2 | Revert progress uses nonunique display labels. Reordering same-file steps after failure caused retry to skip the outstanding change and repeat an already-restored key. | `src/plan.rs:482`, `src/plan.rs:1021` |
| R22 | P2 | A missing reverse command is treated as completed manual work. Revert exited 0, cleared applied state, and left the actual change present. | `src/exec.rs:710`, `src/exec.rs:785`, `src/exec.rs:1027` |
| R23 | P2 | APT candidate discovery uses stale local indexes before deciding whether to invoke apt-refresh. Even explicit --refresh reported current until an external metadata refresh. | `src/commands.rs:632`, `catalog/templates.toml:65` |

## Additional design issues established by code inspection

These are not included in the reproduced-defect count and were not exercised on real user
accounts or credentials.

- **D1: default-shell revert does not restore the original shell.**
  `catalog/shell.toml:63` hardcodes `/bin/bash`. A former fish or another shell is not restored.
  Record the actual old login shell and restore that value. This is related to the ownership
  and prior-state problem in R2, but also affects resources envira really did change.
- **D2: git-settings couples an alias with a persistent credential-helper change.**
  `catalog/shell.toml:75` sets the Git helper to `store`, replacing the prior setting. This
  selects plaintext credential storage for future Git authentication. It is described in the
  config text but bundled with unrelated shell/alias setup. Preserve the existing helper or
  separate this into an explicit opt-in setting. No real credential content was inspected,
  generated, printed or written during this review.

The earlier route-identity follow-up is now R19; the earlier Go fault-injection follow-up is
now R11/R15. They are not additional uncounted findings. R8 and R19 concern external adoption
and managed update routing respectively; R6 and R23 concern version identity and metadata
freshness respectively; R12 and R20 concern corrupt metadata and normal overlapping edits.

## Repair order and verification

1. Address P1 findings R1/R2/R4/R11. Include R12/R20 when repairing backup ownership/lifetime,
   because these two also destroy recovery information under their stated conditions.
2. Repair the recovery model together: R3/R14/R15/R21/R22 and D1. Persist actual effects,
   prior values, stable identities, and unsupported/partial outcomes rather than deriving
   rollback solely from the latest catalog and current detection results.
3. Unify route and execution context: R5/R7/R8/R13/R19, then R6/R23's package update discovery.
4. Address result/cache/bootstrap correctness R9/R10/R16/R17/R18 and make D2 explicit.

The first pass's exact baseline commands and observed signals were:

```sh
cargo test --locked
# 269 passed, 0 failed.
cargo clippy --locked --all-targets -- -D warnings
# Exit 0.
cargo fmt --all -- --check
# Exit 0.
python3 -m unittest discover -s test -v
# 11 passed; standard library only.
bash -n envira.sh run.sh run_user.sh verify.sh tests/integration/*.sh
# Exit 0.
ENVIRA_NO_UPDATE_CHECK=1 target/debug/envira --no-remote --no-overlay catalog check
# 100 tools, 19 configs, 14 bundles, 12 methods, 7 actions; 0 errors, 0 warnings.
```

These broad checks passed before the targeted reproductions. They do not establish coverage
of the bugs above, and were not needlessly rerun in later passes against unchanged production
code. Each round's note provides exact diagnostic replay commands and expected failing signals.
Reproductions explicitly use synthetic fixtures and isolated temporary directories; the
privilege case uses an offline disposable container. Native macOS, the full distro matrix,
current vendor endpoints, disk exhaustion and power loss were not comprehensively tested.
