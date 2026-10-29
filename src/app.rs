//! Shared session: catalog, facts, paths and runner used by the CLI and the TUI.

use std::collections::BTreeSet;
use std::sync::Arc;
use std::time::Duration;

use anyhow::{Result, bail};

use crate::catalog::{self, Catalog, CatalogOptions, Loaded, validate};
use crate::cli::{Global, ScopeArg};
use crate::facts::{Facts, Privilege};
use crate::plan::{Plan, Planner};
use crate::runner::{CmdEnv, RealRunner, Runner};
use crate::state::{Paths, State};
use crate::status::{self, Snapshot};

pub struct Session {
    pub global: Global,
    pub loaded: Loaded,
    pub catalog: Arc<Catalog>,
    pub facts: Arc<Facts>,
    pub paths: Paths,
    pub runner: Arc<dyn Runner>,
}

impl Session {
    pub fn open(global: &Global) -> Result<Session> {
        let facts = Facts::probe();
        let paths = Paths::new(&facts);
        let loaded = load_valid(global, &paths)?;
        let path = status::effective_path(&loaded.catalog, &facts);
        let runner: Arc<dyn Runner> = Arc::new(RealRunner {
            env: CmdEnv {
                path,
                home: facts.home.clone(),
                user: facts.user.clone(),
                drop_to: facts.drop_to.clone(),
                state_dir: paths.state_dir.clone(),
            },
        });
        Ok(Session {
            global: global.clone(),
            catalog: Arc::new(loaded.catalog.clone()),
            loaded,
            facts: Arc::new(facts),
            paths,
            runner,
        })
    }

    /// Re-reads every catalog layer (TUI `r`).
    pub fn reload(&mut self) -> Result<()> {
        let loaded = load_valid(&self.global, &self.paths)?;
        self.catalog = Arc::new(loaded.catalog.clone());
        self.loaded = loaded;
        Ok(())
    }

    pub fn state(&self) -> Result<Option<State>> {
        State::load(&self.paths)
    }

    pub async fn snapshot(&self, state: Option<&State>) -> Snapshot {
        status::snapshot(&self.catalog, &self.facts, state, self.runner.as_ref()).await
    }

    pub fn prefer_system(&self) -> bool {
        match self.global.scope {
            Some(ScopeArg::User) => false,
            Some(ScopeArg::System) | None => self.facts.privilege.can_elevate(),
        }
    }

    pub fn plan(&self, present: &BTreeSet<String>, targets: &[String], force: bool) -> Plan {
        let mut p = Planner::new(&self.catalog, &self.facts, present);
        p.prefer_system = self.prefer_system();
        p.plan(targets, force)
    }

    pub fn revert_plan(&self, present: &BTreeSet<String>, targets: &[String]) -> Plan {
        let mut p = Planner::new(&self.catalog, &self.facts, present);
        p.prefer_system = self.prefer_system();
        p.revert(targets)
    }

    /// Expands bundle names into their items; other names are kept.
    pub fn expand_targets(&self, names: &[String]) -> Result<Vec<String>> {
        let mut out = Vec::new();
        for n in names {
            if let Some(b) = self.catalog.bundles.get(n) {
                out.extend(b.items.iter().cloned());
            } else if self.catalog.tools.contains_key(n) || self.catalog.configs.contains_key(n) {
                out.push(n.clone());
            } else {
                bail!("unknown item or bundle {n:?}");
            }
        }
        Ok(out)
    }
}

pub fn catalog_options(global: &Global, paths: &Paths) -> CatalogOptions {
    CatalogOptions {
        explicit: global.catalog.clone(),
        overlay: (!global.no_overlay).then(|| paths.overlay_dir()),
    }
}

fn load_valid(global: &Global, paths: &Paths) -> Result<Loaded> {
    let loaded = catalog::load_catalog(&catalog_options(global, paths))?;
    let report = validate::validate(&loaded);
    if !report.is_ok() {
        bail!(
            "the catalog has errors (run `envira catalog check`):\n  {}",
            report.errors.join("\n  ")
        );
    }
    Ok(loaded)
}

/// Asks for the sudo password once (when needed) and keeps the credential cache warm.
/// Must run while the terminal is in normal mode.
pub fn ensure_sudo(facts: &Facts) -> Result<Option<tokio::task::JoinHandle<()>>> {
    match facts.privilege {
        Privilege::Root => return Ok(None),
        Privilege::None => bail!("this plan needs sudo, which is not available"),
        Privilege::Sudo { .. } => {}
    }
    let ok = std::process::Command::new("sudo")
        .arg("-v")
        .status()
        .is_ok_and(|s| s.success());
    if !ok {
        bail!("sudo authentication failed");
    }
    Ok(Some(tokio::spawn(async {
        loop {
            tokio::time::sleep(Duration::from_secs(60)).await;
            let _ = tokio::process::Command::new("sudo")
                .args(["-n", "-v"])
                .stdin(std::process::Stdio::null())
                .status()
                .await;
        }
    })))
}
