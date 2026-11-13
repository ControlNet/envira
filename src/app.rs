//! Shared session: catalog, facts, paths and runner used by the CLI and the TUI.

use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use anyhow::{Result, bail};

use crate::catalog::{self, Catalog, CatalogOptions, Loaded, validate};
use crate::cli::{Global, ScopeArg};
use crate::exec::{self, ExecCtx};
use crate::facts::{Facts, Privilege};
use crate::latest::{self, Fetch};
use crate::plan::{Plan, Planner, UpdateTarget};
use crate::runner::{Budget, CmdEnv, RealRunner, Runner};
use crate::state::{Paths, State};
use crate::status::{self, Snapshot};

/// Runs commands with the PATH the catalog's tools need.
fn real_runner(
    cat: &Catalog,
    facts: &Facts,
    paths: &Paths,
    budget: &Arc<Budget>,
) -> Arc<dyn Runner> {
    let env = CmdEnv {
        path: status::effective_path(cat, facts),
        home: facts.home.clone(),
        user: facts.user.clone(),
        drop_to: facts.drop_to.clone(),
        root_home: crate::facts::home_of(0).unwrap_or_else(|| "/root".into()),
        state_dir: paths.state_dir.clone(),
    };
    Arc::new(RealRunner::new(env, budget.clone()))
}

pub struct Session {
    pub global: Global,
    pub loaded: Loaded,
    pub catalog: Arc<Catalog>,
    pub facts: Arc<Facts>,
    pub paths: Paths,
    pub runner: Arc<dyn Runner>,
    /// Memory of envira's own commands, shared by the runners of a reload.
    pub budget: Arc<Budget>,
    /// `facts` without sudo, for plans in user mode.
    pub user_facts: Arc<Facts>,
    /// Plans are made in user mode: chosen when sudo asked for a password.
    user_mode: AtomicBool,
}

/// The facts `--scope` asks for: `user` plans without sudo (user mode); `system` takes sudo
/// to be there when envira could not tell (`has_sudo`: the command exists), as sudo rights
/// can come from a sudoers rule envira cannot see without a password: the plan asks for it.
pub fn scoped(facts: Facts, scope: Option<ScopeArg>, has_sudo: bool) -> Facts {
    match scope {
        Some(ScopeArg::User) => facts.without_sudo(),
        Some(ScopeArg::System) if facts.privilege == Privilege::None && has_sudo => Facts {
            privilege: Privilege::Sudo {
                passwordless: false,
            },
            ..facts
        },
        _ => facts,
    }
}

impl Session {
    pub fn open(global: &Global) -> Result<Session> {
        let mut facts = scoped(Facts::probe(), global.scope, crate::facts::which("sudo"));
        let paths = Paths::new(&facts);
        facts.state_dir = Some(paths.state_dir.clone());
        let loaded = load_valid(global, &paths, &facts)?;
        let budget = Arc::new(Budget::for_scans(paths.state_dir.join("probes.toml")));
        let runner = real_runner(&loaded.catalog, &facts, &paths, &budget);
        Ok(Session {
            global: global.clone(),
            catalog: Arc::new(loaded.catalog.clone()),
            loaded,
            user_facts: Arc::new(facts.without_sudo()),
            facts: Arc::new(facts),
            paths,
            runner,
            budget,
            user_mode: AtomicBool::new(false),
        })
    }

    /// Makes the next plans in user mode (see `Facts::without_sudo`).
    pub fn use_user_mode(&self) {
        self.user_mode.store(true, Ordering::Relaxed);
    }

    /// The facts plans are made with: `user_facts` in user mode.
    pub fn plan_facts(&self) -> &Facts {
        if self.user_mode.load(Ordering::Relaxed) {
            &self.user_facts
        } else {
            &self.facts
        }
    }

    /// Re-reads every catalog layer (TUI `r`). Commands get the PATH of the new catalog (its
    /// tools' bin dirs); a run going on keeps the runner it started with.
    pub fn reload(&mut self) -> Result<()> {
        let loaded = load_valid(&self.global, &self.paths, &self.facts)?;
        self.runner = real_runner(&loaded.catalog, &self.facts, &self.paths, &self.budget);
        self.catalog = Arc::new(loaded.catalog.clone());
        self.loaded = loaded;
        Ok(())
    }

    /// Execution context for a run of `command`.
    pub fn exec_ctx(&self, command: String, shell_env: bool) -> ExecCtx {
        ExecCtx {
            catalog: self.catalog.clone(),
            facts: self.facts.clone(),
            paths: self.paths.clone(),
            runner: self.runner.clone(),
            run_id: crate::state::new_run_id(&self.paths),
            command,
            shell_env,
            step_timeout: self
                .global
                .step_timeout
                .map(Duration::from_secs)
                .unwrap_or(exec::STEP_TIMEOUT),
            retries: self.global.retries.unwrap_or(exec::RETRIES),
            retry_delay: exec::RETRY_DELAY,
            jobs: self.global.jobs.map_or(exec::JOBS, usize::from),
        }
    }

    pub fn state(&self) -> Result<Option<State>> {
        State::load(&self.paths)
    }

    pub async fn snapshot(&self, state: Option<&State>) -> Snapshot {
        status::snapshot(
            &self.catalog,
            &self.facts,
            state,
            self.runner.as_ref(),
            self.prefer_system(),
        )
        .await
    }

    /// Adds latest versions (from the cache, or queried per `fetch`) and marks outdated tools.
    pub async fn apply_latest(&self, snap: &mut Snapshot, fetch: Fetch) {
        let mut cache = latest::Cache::load(&self.paths);
        let answers = latest::check(
            &self.catalog,
            &self.facts,
            snap,
            self.runner.as_ref(),
            &mut cache,
            fetch,
        )
        .await;
        if fetch != Fetch::CacheOnly {
            let _ = cache.save(&self.paths, &self.facts);
        }
        snap.apply_latest(&answers);
    }

    /// Updates `targets`; `present`: what is installed now (a dependency of a user-scope copy
    /// that replaces a system install out of reach is installed only when missing).
    pub fn update_plan(&self, present: &BTreeSet<String>, targets: &[UpdateTarget]) -> Plan {
        let mut p = Planner::new(&self.catalog, self.plan_facts(), present);
        p.prefer_system = self.prefer_system();
        p.update(targets)
    }

    pub fn prefer_system(&self) -> bool {
        match self.global.scope {
            Some(ScopeArg::User) => false,
            Some(ScopeArg::System) | None => self.plan_facts().privilege.can_elevate(),
        }
    }

    pub fn plan(&self, present: &BTreeSet<String>, targets: &[String], force: bool) -> Plan {
        let mut p = Planner::new(&self.catalog, self.plan_facts(), present);
        p.prefer_system = self.prefer_system();
        p.plan(targets, force)
    }

    /// Installs `targets` again after a failed run; `done`: the steps each got through (see
    /// `Planner::finishing`).
    pub fn retry_install_plan(
        &self,
        present: &BTreeSet<String>,
        targets: &[String],
        done: &crate::plan::InstallsDone,
    ) -> Plan {
        let mut p = Planner::new(&self.catalog, self.plan_facts(), present);
        p.prefer_system = self.prefer_system();
        p.finishing = done.clone();
        p.plan(targets, false)
    }

    /// Reverts `targets`; `retrying` reverts that stopped halfway, with the steps each got
    /// through (see `Planner::retrying`).
    pub fn revert_plan(
        &self,
        present: &BTreeSet<String>,
        targets: &[String],
        retrying: Option<&BTreeMap<String, Vec<String>>>,
    ) -> Plan {
        let mut p = Planner::new(&self.catalog, self.plan_facts(), present);
        p.prefer_system = self.prefer_system();
        p.retrying = retrying.cloned();
        // State that cannot be read leaves no record: a config in place reverts in full.
        p.changes = self
            .state()
            .ok()
            .flatten()
            .map(|s| s.config_changes())
            .unwrap_or_default();
        p.revert(targets)
    }

    /// Expands bundle names into their items; other names are kept.
    pub fn expand_targets(&self, names: &[String]) -> Result<Vec<String>> {
        let mut out = Vec::new();
        for n in names {
            if self.catalog.bundles.contains_key(n) {
                out.extend(self.catalog.bundle_items(n));
            } else if self.catalog.tools.contains_key(n) || self.catalog.configs.contains_key(n) {
                out.push(n.clone());
            } else {
                bail!("unknown item or bundle {n:?}");
            }
        }
        Ok(out)
    }
}

pub fn catalog_options(global: &Global, paths: &Paths, facts: &Facts) -> CatalogOptions {
    CatalogOptions {
        explicit: global.catalog.clone(),
        overlay: (!global.no_overlay).then(|| paths.overlay_dir()),
        remote: (!global.no_remote).then(|| {
            let url = std::env::var("ENVIRA_CATALOG_URL")
                .ok()
                .filter(|u| !u.is_empty())
                .unwrap_or_else(|| catalog::remote::URL.into());
            catalog::remote::Remote {
                cache: catalog::remote::cache_file(&paths.cache_dir, &url),
                url,
                owner: facts.drop_to.as_ref().map(|_| (facts.uid, facts.gid)),
            }
        }),
    }
}

fn load_valid(global: &Global, paths: &Paths, facts: &Facts) -> Result<Loaded> {
    let loaded = catalog::load_catalog(&catalog_options(global, paths, facts))?;
    let report = validate::validate(&loaded);
    if !report.is_ok() {
        bail!(
            "the catalog has errors (run `envira catalog check`):\n  {}",
            report.errors.join("\n  ")
        );
    }
    Ok(loaded)
}

/// Whether steps can use sudo now without its password (see
/// `facts::sudo_runs_without_password`).
pub fn sudo_without_password() -> bool {
    crate::facts::sudo_runs_without_password()
}

/// Refreshes the sudo ticket every minute for as long as it runs (a run). Harmless where sudo
/// needs no password.
pub fn sudo_keepalive() -> tokio::task::JoinHandle<()> {
    tokio::spawn(async {
        loop {
            tokio::time::sleep(Duration::from_secs(60)).await;
            let _ = tokio::process::Command::new("sudo")
                .args(["-n", "-v"])
                .stdin(std::process::Stdio::null())
                .stdout(std::process::Stdio::null())
                .stderr(std::process::Stdio::null())
                .status()
                .await;
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;

    #[test]
    fn scope_user_plans_without_sudo_and_system_assumes_it() {
        let f = Facts::sample(Path::new("/home/user"));
        let user = scoped(f.clone(), Some(ScopeArg::User), true);
        assert_eq!(user.privilege, Privilege::None);
        assert!(user.user_mode && user.summary().contains("user mode"));
        assert_eq!(scoped(f.clone(), None, true).privilege, f.privilege);
        // Sudo envira could not see (a sudoers rule for this user): the plan asks for it.
        let mut none = f.clone();
        none.privilege = Privilege::None;
        let system = scoped(none.clone(), Some(ScopeArg::System), true);
        assert!(system.sudo_asks_password());
        // No sudo command at all: nothing to ask.
        assert_eq!(
            scoped(none, Some(ScopeArg::System), false).privilege,
            Privilege::None
        );
    }
}
