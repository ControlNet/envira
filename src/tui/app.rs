//! TUI state and key handling. Pure logic: side effects are returned as `Cmd`s for the runtime.

use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;
use std::time::{Duration, Instant};

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use tokio::sync::watch;

use crate::catalog::{Catalog, Loaded};
use crate::exec::{Control, Event, Outcome};
use crate::facts::Facts;
use crate::latest::{Fetch, Latest};
use crate::plan::{Plan, Planner, UpdateTarget};
use crate::state::{Paths, RunRecord, State};
use crate::status::{ItemStatus, Label, Snapshot};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Screen {
    Loading,
    Picker,
    Dashboard,
    Plan,
    Run,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Filter {
    All,
    Attention,
    Outdated,
    Missing,
    New,
    Failed,
    Installed,
}

impl Filter {
    pub const ALL: [Filter; 7] = [
        Filter::All,
        Filter::Attention,
        Filter::Outdated,
        Filter::Missing,
        Filter::New,
        Filter::Failed,
        Filter::Installed,
    ];

    pub fn name(self) -> &'static str {
        match self {
            Filter::All => "All",
            Filter::Attention => "Attention",
            Filter::Outdated => "Outdated",
            Filter::Missing => "Missing",
            Filter::New => "New",
            Filter::Failed => "Failed",
            Filter::Installed => "Installed",
        }
    }

    fn keeps(self, i: &ItemStatus) -> bool {
        match self {
            Filter::All => true,
            Filter::Attention => matches!(
                i.label,
                Label::Missing | Label::New | Label::Failed | Label::Drifted | Label::Outdated
            ),
            Filter::Outdated => i.label == Label::Outdated,
            Filter::Missing => i.label == Label::Missing,
            Filter::New => i.label == Label::New,
            Filter::Failed => matches!(i.label, Label::Failed | Label::Drifted),
            Filter::Installed => i.present,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DetailTab {
    Info,
    Commands,
    Log,
}

/// Side effects requested by the app.
#[derive(Debug)]
pub enum Cmd {
    Quit,
    /// Re-detect everything; latest versions per the fetch mode.
    Refresh(Fetch),
    /// Re-read catalog layers, then refresh.
    Reload,
    SaveState,
    StartRun {
        plan: Plan,
        command: String,
    },
    /// Write the plan as a shell script next to the state.
    ExportScript(Plan),
}

#[derive(Debug, Clone)]
pub enum UnitState {
    Queued,
    Running(Instant),
    Done(Outcome, Duration),
}

pub struct RunView {
    pub plan: Plan,
    pub run_id: String,
    pub units: Vec<UnitState>,
    pub logs: Vec<Vec<String>>,
    pub sel: usize,
    /// Cursor follows the running unit and the log sticks to the bottom.
    pub follow: bool,
    /// Lines scrolled up from the bottom of the log.
    pub scroll: usize,
    pub started: Instant,
    pub finished: Option<(RunRecord, Duration)>,
    pub shell_env: Option<Result<Vec<String>, String>>,
    pub ctl: Option<watch::Sender<Control>>,
    pub stopping: bool,
}

impl RunView {
    pub fn new(plan: Plan, run_id: String, ctl: Option<watch::Sender<Control>>) -> RunView {
        let n = plan.units.len();
        RunView {
            plan,
            run_id,
            units: vec![UnitState::Queued; n],
            logs: vec![Vec::new(); n],
            sel: 0,
            follow: true,
            scroll: 0,
            started: Instant::now(),
            finished: None,
            shell_env: None,
            ctl,
            stopping: false,
        }
    }

    pub fn count(&self, f: impl Fn(&UnitState) -> bool) -> usize {
        self.units.iter().filter(|u| f(u)).count()
    }

    /// Units to retry: failed, blocked, cancelled, or skipped after a stop.
    pub fn failed_ids(&self) -> Vec<String> {
        self.plan
            .units
            .iter()
            .zip(&self.units)
            .filter(|(_, s)| {
                matches!(
                    s,
                    UnitState::Done(
                        Outcome::Failed(_)
                            | Outcome::Blocked(_)
                            | Outcome::Cancelled
                            | Outcome::Skipped,
                        _
                    )
                )
            })
            .map(|(u, _)| u.id.clone())
            .collect()
    }
}

pub struct App {
    pub catalog: Arc<Catalog>,
    pub loaded_layers: Vec<String>,
    pub facts: Arc<Facts>,
    pub paths: Paths,
    pub prefer_system: bool,
    pub state: Option<State>,
    pub snap: Option<Snapshot>,
    pub detecting: bool,
    /// A latest-version check is running in the background.
    pub checking_latest: bool,
    pub screen: Screen,
    pub focus: usize,
    pub cursor: usize,
    pub selected: BTreeSet<String>,
    pub filter: Filter,
    pub search: String,
    pub searching: bool,
    pub bundle_cursor: usize,
    pub detail: DetailTab,
    pub detail_scroll: usize,
    pub picker: BTreeSet<String>,
    pub picker_cursor: usize,
    pub plan: Option<(Plan, String)>,
    pub plan_cursor: usize,
    pub run: Option<RunView>,
    pub toast: Option<(String, Instant)>,
    pub help: bool,
    pub tick: u64,
}

impl App {
    pub fn new(
        loaded: &Loaded,
        catalog: Arc<Catalog>,
        facts: Arc<Facts>,
        paths: Paths,
        prefer_system: bool,
    ) -> App {
        App {
            catalog,
            loaded_layers: loaded.layers.clone(),
            facts,
            paths,
            prefer_system,
            state: None,
            snap: None,
            detecting: true,
            checking_latest: false,
            screen: Screen::Loading,
            focus: 4,
            cursor: 0,
            selected: BTreeSet::new(),
            filter: Filter::All,
            search: String::new(),
            searching: false,
            bundle_cursor: 0,
            detail: DetailTab::Info,
            detail_scroll: 0,
            picker: BTreeSet::new(),
            picker_cursor: 0,
            plan: None,
            plan_cursor: 0,
            run: None,
            toast: None,
            help: false,
            tick: 0,
        }
    }

    pub fn notify(&mut self, msg: impl Into<String>) {
        self.toast = Some((msg.into(), Instant::now()));
    }

    pub fn toast_text(&self) -> Option<&str> {
        self.toast
            .as_ref()
            .filter(|(_, t)| t.elapsed() < Duration::from_secs(6))
            .map(|(m, _)| m.as_str())
    }

    /// New detection results.
    pub fn on_snapshot(&mut self, snap: Snapshot, state: Option<State>) {
        let first = self.screen == Screen::Loading;
        self.state = state;
        self.detecting = false;
        self.selected.retain(|id| snap.get(id).is_some());
        if first {
            if snap.first_run {
                // Preselect bundles whose items are mostly present already.
                self.picker = self
                    .catalog
                    .bundles
                    .keys()
                    .filter(|id| {
                        let items = self.catalog.bundle_items(id);
                        let have = items
                            .iter()
                            .filter(|i| snap.get(i).is_some_and(|s| s.present))
                            .count();
                        !items.is_empty() && have * 2 >= items.len()
                    })
                    .cloned()
                    .collect();
                self.screen = Screen::Picker;
            } else {
                self.screen = Screen::Dashboard;
            }
        }
        self.snap = Some(snap);
        self.clamp();
    }

    pub fn rows(&self) -> Vec<&ItemStatus> {
        let Some(snap) = &self.snap else {
            return vec![];
        };
        let q = self.search.to_lowercase();
        snap.items
            .iter()
            .filter(|i| self.filter.keeps(i))
            .filter(|i| {
                q.is_empty()
                    || i.id.to_lowercase().contains(&q)
                    || i.category.to_lowercase().contains(&q)
                    || self.catalog.desc(&i.id).to_lowercase().contains(&q)
            })
            .collect()
    }

    pub fn current(&self) -> Option<&ItemStatus> {
        self.rows().get(self.cursor).copied()
    }

    fn clamp(&mut self) {
        let n = self.rows().len();
        self.cursor = self.cursor.min(n.saturating_sub(1));
        self.bundle_cursor = self
            .bundle_cursor
            .min(self.catalog.bundles.len().saturating_sub(1));
    }

    pub fn present(&self) -> BTreeSet<String> {
        self.snap.as_ref().map(|s| s.present()).unwrap_or_default()
    }

    pub fn make_plan(&self, targets: &[String]) -> Plan {
        let present = self.present();
        let mut p = Planner::new(&self.catalog, &self.facts, &present);
        p.prefer_system = self.prefer_system;
        p.plan(targets, false)
    }

    pub fn make_update_plan(&self, targets: &[UpdateTarget]) -> Plan {
        let present = self.present();
        let mut p = Planner::new(&self.catalog, &self.facts, &present);
        p.prefer_system = self.prefer_system;
        p.update(targets)
    }

    /// Update targets: the selected tools (explicit), else every outdated tool; plus why
    /// selected ones were left out, and a plan title.
    fn update_targets(&self) -> (Vec<UpdateTarget>, Vec<(String, String)>, String) {
        let Some(snap) = &self.snap else {
            return (vec![], vec![], String::new());
        };
        let (mut targets, mut skipped) = (Vec::new(), Vec::new());
        if self.selected.is_empty() {
            for id in snap.outdated() {
                if let Some(Ok(t)) = snap.get(&id).map(|i| i.update_target(false)) {
                    targets.push(t);
                }
            }
            let n = targets.len();
            return (targets, skipped, format!("update · {n} outdated"));
        }
        for id in self.targets() {
            match snap.get(&id).map(|i| i.update_target(true)) {
                Some(Ok(t)) => targets.push(t),
                Some(Err(why)) => skipped.push((id, why)),
                None => {}
            }
        }
        let n = self.selected.len();
        (targets, skipped, format!("update · from {n} selected"))
    }

    /// Latest-version answers from the background check.
    pub fn on_latest(&mut self, answers: BTreeMap<String, Latest>) {
        self.checking_latest = false;
        if let Some(snap) = &mut self.snap {
            snap.apply_latest(&answers);
            let n = snap.outdated().len();
            if n > 0 {
                self.notify(format!(
                    "{n} {} · u updates",
                    super::ui::plural(n, "update", "updates")
                ));
            }
        }
        self.clamp();
    }

    fn make_revert(&self, targets: &[String]) -> Plan {
        let present = self.present();
        let mut p = Planner::new(&self.catalog, &self.facts, &present);
        p.prefer_system = self.prefer_system;
        p.revert(targets)
    }

    pub fn bundle_ids(&self) -> Vec<String> {
        self.catalog.bundles.keys().cloned().collect()
    }

    pub fn subscribed(&self, bundle: &str) -> bool {
        self.state
            .as_ref()
            .is_some_and(|s| s.bundles.iter().any(|b| b == bundle))
    }

    /// Items to act on: the selection, else the item under the cursor.
    fn targets(&self) -> Vec<String> {
        if !self.selected.is_empty() {
            let order = self.catalog.item_ids();
            return order
                .into_iter()
                .filter(|id| self.selected.contains(id))
                .collect();
        }
        self.current()
            .map(|i| vec![i.id.clone()])
            .unwrap_or_default()
    }

    fn open_plan(&mut self, plan: Plan, title: String) {
        if plan.is_empty() {
            let why = plan
                .skipped
                .first()
                .map(|(id, w)| format!(" ({id}: {w})"))
                .unwrap_or_default();
            self.notify(format!("Nothing to do{why}"));
            return;
        }
        self.plan = Some((plan, title));
        self.plan_cursor = 0;
        self.screen = Screen::Plan;
    }

    // --------------------------------------------------------------------------------------------
    // Keys
    // --------------------------------------------------------------------------------------------

    pub fn on_key(&mut self, k: KeyEvent) -> Vec<Cmd> {
        if k.modifiers.contains(KeyModifiers::CONTROL) && k.code == KeyCode::Char('c') {
            if self.screen == Screen::Run {
                return self.run_key(k);
            }
            return vec![Cmd::Quit];
        }
        if self.help {
            self.help = false;
            return vec![];
        }
        match self.screen {
            Screen::Loading => match k.code {
                KeyCode::Char('q') | KeyCode::Esc => vec![Cmd::Quit],
                _ => vec![],
            },
            Screen::Picker => self.picker_key(k),
            Screen::Dashboard => self.dashboard_key(k),
            Screen::Plan => self.plan_key(k),
            Screen::Run => self.run_key(k),
        }
    }

    fn picker_key(&mut self, k: KeyEvent) -> Vec<Cmd> {
        let ids = self.bundle_ids();
        match k.code {
            KeyCode::Char('q') => return vec![Cmd::Quit],
            KeyCode::Up | KeyCode::Char('k') => {
                self.picker_cursor = self.picker_cursor.saturating_sub(1)
            }
            KeyCode::Down | KeyCode::Char('j') => {
                self.picker_cursor = (self.picker_cursor + 1).min(ids.len().saturating_sub(1))
            }
            KeyCode::Char(' ') => {
                if let Some(id) = ids.get(self.picker_cursor)
                    && !self.picker.remove(id)
                {
                    self.picker.insert(id.clone());
                }
            }
            KeyCode::Char('a') => self.picker = ids.into_iter().collect(),
            KeyCode::Char('n') => self.picker.clear(),
            KeyCode::Char('?') => self.help = true,
            KeyCode::Enter | KeyCode::Esc => {
                let mut st = self.state.clone().unwrap_or_default();
                st.bundles = self
                    .bundle_ids()
                    .into_iter()
                    .filter(|b| self.picker.contains(b))
                    .collect();
                st.mark_all_seen(&self.catalog);
                self.state = Some(st);
                self.screen = Screen::Dashboard;
                let cmds = vec![Cmd::SaveState, Cmd::Refresh(Fetch::CacheOnly)];
                if k.code == KeyCode::Enter {
                    let targets = self.picker_targets();
                    let plan = self.make_plan(&targets);
                    if !plan.is_empty() {
                        self.open_plan(plan, format!("first sync · {} bundles", self.picker.len()));
                    }
                }
                return cmds;
            }
            _ => {}
        }
        vec![]
    }

    /// Absent, installable items of the bundles chosen in the picker.
    pub fn picker_targets(&self) -> Vec<String> {
        let Some(snap) = &self.snap else {
            return vec![];
        };
        let mut out: Vec<String> = Vec::new();
        for b in self
            .bundle_ids()
            .iter()
            .filter(|b| self.picker.contains(*b))
        {
            for i in &self.catalog.bundle_items(b) {
                if let Some(s) = snap.get(i)
                    && !s.present
                    && s.label != Label::NotApplicable
                    && !out.contains(i)
                {
                    out.push(i.clone());
                }
            }
        }
        out
    }

    fn dashboard_key(&mut self, k: KeyEvent) -> Vec<Cmd> {
        if self.searching {
            match k.code {
                KeyCode::Esc => {
                    self.searching = false;
                    self.search.clear();
                }
                KeyCode::Enter => self.searching = false,
                KeyCode::Backspace => {
                    self.search.pop();
                }
                KeyCode::Char(c) => self.search.push(c),
                _ => {}
            }
            self.cursor = 0;
            self.clamp();
            return vec![];
        }
        match k.code {
            KeyCode::Char('q') => return vec![Cmd::Quit],
            KeyCode::Esc => {
                if !self.search.is_empty() {
                    self.search.clear();
                } else {
                    self.selected.clear();
                }
                self.clamp();
            }
            KeyCode::Char(c @ '1'..='5') => self.focus = c as usize - '0' as usize,
            KeyCode::Tab => self.focus = if self.focus >= 5 { 1 } else { self.focus + 1 },
            KeyCode::BackTab => self.focus = if self.focus <= 1 { 5 } else { self.focus - 1 },
            KeyCode::Char('?') => self.help = true,
            KeyCode::Char('/') => {
                self.focus = 4;
                self.searching = true;
                self.search.clear();
            }
            KeyCode::Char('f') => {
                let i = Filter::ALL
                    .iter()
                    .position(|f| *f == self.filter)
                    .unwrap_or(0);
                self.filter = Filter::ALL[(i + 1) % Filter::ALL.len()];
                self.cursor = 0;
                self.clamp();
            }
            KeyCode::Char('R') => {
                self.detecting = true;
                self.checking_latest = true;
                return vec![Cmd::Refresh(Fetch::Force)];
            }
            KeyCode::Char('u') => {
                let (targets, skipped, what) = self.update_targets();
                if targets.is_empty() {
                    let why = skipped
                        .first()
                        .map(|(id, w)| format!("{id}: {w}"))
                        .unwrap_or_else(|| {
                            if self.checking_latest {
                                "Checking for updates…".into()
                            } else {
                                "Nothing is outdated (R checks again)".into()
                            }
                        });
                    self.notify(why);
                } else {
                    let mut plan = self.make_update_plan(&targets);
                    plan.skipped.extend(skipped);
                    self.open_plan(plan, what);
                }
            }
            KeyCode::Char('r') => {
                self.detecting = true;
                return vec![Cmd::Reload];
            }
            KeyCode::Char('s') => {
                let targets = self.snap.as_ref().map(|s| s.to_sync()).unwrap_or_default();
                if targets.is_empty() {
                    self.notify(
                        if self.state.as_ref().is_none_or(|s| s.bundles.is_empty()) {
                            "No bundles selected: focus [2] Bundles and press space"
                        } else {
                            "Everything in your bundles is present"
                        },
                    );
                } else {
                    let n = targets.len();
                    self.open_plan(
                        self.make_plan(&targets),
                        format!("sync · {n} missing in your bundles"),
                    );
                }
            }
            KeyCode::Char('i') => {
                let t = self.targets();
                let n = t.len();
                self.open_plan(self.make_plan(&t), format!("install · from {n} selected"));
            }
            KeyCode::Char('x') => {
                let t: Vec<String> = self
                    .targets()
                    .into_iter()
                    .filter(|id| self.catalog.configs.contains_key(id))
                    .collect();
                if t.is_empty() {
                    self.notify("Only configs can be reverted");
                } else {
                    let n = t.len();
                    self.open_plan(self.make_revert(&t), format!("revert · {n} configs"));
                }
            }
            KeyCode::Char('l') => {
                self.focus = 5;
                self.detail = DetailTab::Log;
                self.detail_scroll = 0;
            }
            _ => match self.focus {
                2 => return self.bundles_key(k),
                5 => self.detail_key(k),
                _ => self.table_key(k),
            },
        }
        vec![]
    }

    fn table_key(&mut self, k: KeyEvent) {
        let n = self.rows().len();
        match k.code {
            KeyCode::Up | KeyCode::Char('k') => self.cursor = self.cursor.saturating_sub(1),
            KeyCode::Down | KeyCode::Char('j') => {
                self.cursor = (self.cursor + 1).min(n.saturating_sub(1))
            }
            KeyCode::PageUp => self.cursor = self.cursor.saturating_sub(10),
            KeyCode::PageDown => self.cursor = (self.cursor + 10).min(n.saturating_sub(1)),
            KeyCode::Home | KeyCode::Char('g') => self.cursor = 0,
            KeyCode::End | KeyCode::Char('G') => self.cursor = n.saturating_sub(1),
            KeyCode::Char(' ') => {
                if let Some(id) = self.current().map(|i| i.id.clone()) {
                    if !self.selected.remove(&id) {
                        self.selected.insert(id);
                    }
                    self.cursor = (self.cursor + 1).min(n.saturating_sub(1));
                }
            }
            KeyCode::Char('a') => {
                let ids: Vec<String> = self
                    .rows()
                    .iter()
                    .filter(|i| !i.present && i.label != Label::NotApplicable)
                    .map(|i| i.id.clone())
                    .collect();
                self.selected.extend(ids);
            }
            KeyCode::Enter => {
                self.focus = 5;
                self.detail = DetailTab::Info;
            }
            _ => {}
        }
        self.detail_scroll = 0;
    }

    fn bundles_key(&mut self, k: KeyEvent) -> Vec<Cmd> {
        let ids = self.bundle_ids();
        match k.code {
            KeyCode::Up | KeyCode::Char('k') => {
                self.bundle_cursor = self.bundle_cursor.saturating_sub(1)
            }
            KeyCode::Down | KeyCode::Char('j') => {
                self.bundle_cursor = (self.bundle_cursor + 1).min(ids.len().saturating_sub(1))
            }
            KeyCode::Char(' ') => {
                if let Some(b) = ids.get(self.bundle_cursor) {
                    let st = self.state.get_or_insert_with(State::default);
                    if let Some(pos) = st.bundles.iter().position(|x| x == b) {
                        st.bundles.remove(pos);
                        self.notify(format!(
                            "{} is no longer kept in sync",
                            self.catalog.bundle_name(b)
                        ));
                    } else {
                        st.bundles.push(b.clone());
                        self.notify(format!(
                            "{} added: press s to sync",
                            self.catalog.bundle_name(b)
                        ));
                    }
                    return vec![Cmd::SaveState, Cmd::Refresh(Fetch::CacheOnly)];
                }
            }
            KeyCode::Enter => {
                if let Some(b) = ids.get(self.bundle_cursor) {
                    self.selected = self.catalog.bundle_items(b).into_iter().collect();
                    self.focus = 4;
                    self.notify(format!(
                        "Selected the items of {}: i installs them",
                        self.catalog.bundle_name(b)
                    ));
                }
            }
            _ => {}
        }
        vec![]
    }

    fn detail_key(&mut self, k: KeyEvent) {
        match k.code {
            KeyCode::Left | KeyCode::Char('h') => {
                self.detail = match self.detail {
                    DetailTab::Info => DetailTab::Log,
                    DetailTab::Commands => DetailTab::Info,
                    DetailTab::Log => DetailTab::Commands,
                };
                self.detail_scroll = 0;
            }
            KeyCode::Right | KeyCode::Char('l') => {
                self.detail = match self.detail {
                    DetailTab::Info => DetailTab::Commands,
                    DetailTab::Commands => DetailTab::Log,
                    DetailTab::Log => DetailTab::Info,
                };
                self.detail_scroll = 0;
            }
            KeyCode::Up | KeyCode::Char('k') => {
                self.detail_scroll = self.detail_scroll.saturating_sub(1)
            }
            KeyCode::Down | KeyCode::Char('j') => self.detail_scroll += 1,
            _ => {}
        }
    }

    fn plan_key(&mut self, k: KeyEvent) -> Vec<Cmd> {
        let Some((plan, title)) = &self.plan else {
            self.screen = Screen::Dashboard;
            return vec![];
        };
        let n = plan.units.len();
        match k.code {
            KeyCode::Esc | KeyCode::Char('q') => {
                self.plan = None;
                self.screen = Screen::Dashboard;
            }
            KeyCode::Up | KeyCode::Char('k') => {
                self.plan_cursor = self.plan_cursor.saturating_sub(1)
            }
            KeyCode::Down | KeyCode::Char('j') => {
                self.plan_cursor = (self.plan_cursor + 1).min(n.saturating_sub(1))
            }
            KeyCode::Char('c') => return vec![Cmd::ExportScript(plan.clone())],
            KeyCode::Char('?') => self.help = true,
            KeyCode::Enter => {
                let cmd = Cmd::StartRun {
                    plan: plan.clone(),
                    command: title.split(" · ").next().unwrap_or("run").to_string(),
                };
                self.plan = None;
                self.selected.clear();
                return vec![cmd];
            }
            _ => {}
        }
        vec![]
    }

    fn run_key(&mut self, k: KeyEvent) -> Vec<Cmd> {
        let Some(run) = &mut self.run else {
            self.screen = Screen::Dashboard;
            return vec![];
        };
        let n = run.units.len();
        let finished = run.finished.is_some();
        let ctrl_c = k.modifiers.contains(KeyModifiers::CONTROL) && k.code == KeyCode::Char('c');
        if ctrl_c && !finished {
            if let Some(ctl) = &run.ctl {
                ctl.send_modify(|c| c.stop = true);
            }
            run.stopping = true;
            self.notify("Stopping after the current step");
            return vec![];
        }
        match k.code {
            KeyCode::Up | KeyCode::Char('k') => {
                run.follow = false;
                run.sel = run.sel.saturating_sub(1);
                run.scroll = 0;
            }
            KeyCode::Down | KeyCode::Char('j') => {
                run.follow = false;
                run.sel = (run.sel + 1).min(n.saturating_sub(1));
                run.scroll = 0;
            }
            KeyCode::PageUp => run.scroll += 10,
            KeyCode::PageDown => run.scroll = run.scroll.saturating_sub(10),
            KeyCode::Char('f') => {
                run.follow = !run.follow;
                run.scroll = 0;
            }
            KeyCode::Char('c') if !finished => {
                if let Some(ctl) = &run.ctl {
                    ctl.send_modify(|c| c.cancel_seq += 1);
                }
                self.notify("Cancelling the current step");
            }
            KeyCode::Char('r') if finished => {
                let failed = run.failed_ids();
                if failed.is_empty() {
                    self.notify("Nothing to retry");
                } else {
                    let plan = self.make_plan(&failed);
                    let n = failed.len();
                    self.run = None;
                    self.screen = Screen::Dashboard;
                    self.open_plan(plan, format!("retry · {n} unfinished"));
                }
            }
            KeyCode::Enter | KeyCode::Esc if finished => {
                self.run = None;
                self.screen = Screen::Dashboard;
                self.detecting = true;
                return vec![Cmd::Refresh(Fetch::IfStale)];
            }
            KeyCode::Char('q') if finished => return vec![Cmd::Quit],
            _ => {}
        }
        vec![]
    }

    // --------------------------------------------------------------------------------------------
    // Executor events
    // --------------------------------------------------------------------------------------------

    pub fn start_run(&mut self, plan: Plan, run_id: String, ctl: Option<watch::Sender<Control>>) {
        self.run = Some(RunView::new(plan, run_id, ctl));
        self.screen = Screen::Run;
    }

    pub fn on_exec(&mut self, ev: Event) {
        let Some(run) = &mut self.run else { return };
        match ev {
            Event::UnitStarted(i) => {
                run.units[i] = UnitState::Running(Instant::now());
                if run.follow {
                    run.sel = i;
                    run.scroll = 0;
                }
            }
            Event::Line(i, l) => run.logs[i].push(l),
            Event::UnitFinished(i, o, d) => run.units[i] = UnitState::Done(o, d),
            Event::ShellEnv(r) => run.shell_env = Some(r),
            Event::Finished(rec) => {
                let elapsed = run.started.elapsed();
                // Jump to the first failure so its log is visible.
                if let Some(i) = run.units.iter().position(|u| {
                    matches!(
                        u,
                        UnitState::Done(Outcome::Failed(_) | Outcome::Cancelled, _)
                    )
                }) {
                    run.sel = i;
                    run.scroll = 0;
                }
                run.finished = Some((rec, elapsed));
                run.ctl = None;
            }
        }
    }
}

#[cfg(test)]
pub mod tests {
    use super::*;
    use crate::catalog::load::{embedded_layer, load};
    use crate::plan::Kind;
    use crate::status::Ownership;
    use std::path::Path;

    pub fn item(id: &str, kind: Kind, label: Label, desired: bool) -> ItemStatus {
        let present = matches!(label, Label::Installed | Label::Applied | Label::Outdated);
        ItemStatus {
            id: id.into(),
            kind,
            category: "terminal".into(),
            label,
            present,
            version: present.then(|| "1.0.0".into()),
            method_version: None,
            ownership: present.then_some(Ownership::Adoptable),
            path: None,
            desired,
            seen: label != Label::New,
            via: Some("script".into()),
            scope: None,
            route: (present && kind == Kind::Tool).then(|| crate::plan::Route {
                entry: 0,
                via: "script".into(),
                scope: crate::state::Scope::User,
            }),
            latest: (label == Label::Outdated).then(|| "2.0.0".into()),
            latest_error: None,
            reason: (label == Label::NotApplicable).then(|| "when gpu is not met".into()),
            last_error: (label == Label::Failed).then(|| "exit 1".into()),
        }
    }

    pub fn app() -> App {
        let loaded = load(&[embedded_layer()]).unwrap();
        let cat = Arc::new(loaded.catalog.clone());
        let facts = Arc::new(Facts::sample(Path::new("/home/user")));
        let paths = Paths {
            state_dir: "/home/user/.local/state/envira".into(),
            config_dir: "/home/user/.config/envira".into(),
        };
        let mut a = App::new(&loaded, cat.clone(), facts, paths, true);
        let items = cat
            .item_ids()
            .into_iter()
            .map(|id| {
                let kind = if cat.configs.contains_key(&id) {
                    Kind::Config
                } else {
                    Kind::Tool
                };
                let label = match id.as_str() {
                    "bat" | "yazi" | "rust-toolchain" | "unzip" | "libatomic" | "curl" => {
                        Label::Installed
                    }
                    "herdr" => Label::New,
                    "codex" => Label::Missing,
                    "nvitop" => Label::NotApplicable,
                    "fnm" => Label::Failed,
                    _ => Label::Available,
                };
                let mut it = item(
                    &id,
                    kind,
                    label,
                    matches!(id.as_str(), "codex" | "bat" | "yazi" | "fnm"),
                );
                it.category = cat.category(&id).to_string();
                it
            })
            .collect();
        let state = State {
            bundles: vec!["terminal".into()],
            ..Default::default()
        };
        a.on_snapshot(
            Snapshot {
                items,
                first_run: false,
            },
            Some(state),
        );
        a
    }

    fn key(c: KeyCode) -> KeyEvent {
        KeyEvent::new(c, KeyModifiers::NONE)
    }

    fn chars(a: &mut App, s: &str) {
        for c in s.chars() {
            a.on_key(key(KeyCode::Char(c)));
        }
    }

    #[test]
    fn filter_and_search_narrow_rows() {
        let mut a = app();
        assert_eq!(a.screen, Screen::Dashboard);
        let all = a.rows().len();
        a.on_key(key(KeyCode::Char('f')));
        assert_eq!(a.filter, Filter::Attention);
        let attention: Vec<&str> = a.rows().iter().map(|i| i.id.as_str()).collect();
        assert_eq!(attention, vec!["codex", "herdr", "fnm"]);
        a.on_key(key(KeyCode::Char('/')));
        chars(&mut a, "her");
        assert_eq!(a.rows().len(), 1);
        a.on_key(key(KeyCode::Esc));
        assert!(a.search.is_empty());
        a.filter = Filter::All;
        assert_eq!(a.rows().len(), all);
    }

    #[test]
    fn select_and_install_opens_plan_then_run() {
        let mut a = app();
        a.on_key(key(KeyCode::Char('/')));
        chars(&mut a, "codex");
        a.on_key(key(KeyCode::Enter));
        // codex and codex-sandbox-network match; select the tool.
        a.on_key(key(KeyCode::Char(' ')));
        assert!(a.selected.contains("codex"));
        a.on_key(key(KeyCode::Char('i')));
        assert_eq!(a.screen, Screen::Plan);
        let (plan, _) = a.plan.as_ref().unwrap();
        let ids: Vec<&str> = plan.units.iter().map(|u| u.id.as_str()).collect();
        assert_eq!(ids, vec!["fnm", "nodejs", "npm-prefix", "codex"]);
        let cmds = a.on_key(key(KeyCode::Enter));
        assert!(matches!(&cmds[..], [Cmd::StartRun { command, .. }] if command == "install"));
        assert!(a.selected.is_empty());
    }

    #[test]
    fn latest_answers_mark_outdated_and_u_updates_them() {
        let mut a = app();
        a.checking_latest = true;
        a.on_key(key(KeyCode::Char('u')));
        assert_eq!(a.toast_text(), Some("Checking for updates…"));
        a.on_latest(BTreeMap::from([
            (
                "rust-toolchain".to_string(),
                Latest::Version("2.0.0".into()),
            ),
            ("yazi".to_string(), Latest::Version("1.0.0".into())),
        ]));
        assert!(!a.checking_latest);
        assert_eq!(a.toast_text(), Some("1 update · u updates"));
        a.filter = Filter::Outdated;
        let rows: Vec<&str> = a.rows().iter().map(|i| i.id.as_str()).collect();
        assert_eq!(rows, vec!["rust-toolchain"]);
        a.on_key(key(KeyCode::Char('u')));
        assert_eq!(a.screen, Screen::Plan);
        let (plan, title) = a.plan.as_ref().unwrap();
        assert_eq!(title, "update · 1 outdated");
        assert_eq!(plan.units[0].action, crate::plan::Action::Update);
        assert_eq!(plan.units[0].steps[0].cmd.as_deref(), Some("rustup update"));
        assert_eq!(plan.units[0].reason, "1.0.0 → 2.0.0");
        let cmds = a.on_key(key(KeyCode::Enter));
        assert!(matches!(&cmds[..], [Cmd::StartRun { command, .. }] if command == "update"));
    }

    #[test]
    fn u_on_a_selection_explains_skips() {
        let mut a = app();
        a.selected.insert("codex".into());
        a.on_key(key(KeyCode::Char('u')));
        assert_eq!(a.screen, Screen::Dashboard);
        assert_eq!(
            a.toast_text(),
            Some("codex: not installed (use `envira install`)")
        );
    }

    #[test]
    fn run_events_and_retry() {
        let mut a = app();
        let plan = a.make_plan(&["codex".into()]);
        a.start_run(plan, "r1".into(), None);
        a.on_exec(Event::UnitStarted(0));
        a.on_exec(Event::Line(0, "curl: (22) 403".into()));
        a.on_exec(Event::UnitFinished(
            0,
            Outcome::Failed("exit 22".into()),
            Duration::from_secs(1),
        ));
        a.on_exec(Event::UnitFinished(
            1,
            Outcome::Blocked("fnm".into()),
            Duration::ZERO,
        ));
        a.on_exec(Event::UnitFinished(
            2,
            Outcome::Blocked("nodejs".into()),
            Duration::ZERO,
        ));
        a.on_exec(Event::UnitFinished(
            3,
            Outcome::Blocked("npm-prefix".into()),
            Duration::ZERO,
        ));
        a.on_exec(Event::Finished(RunRecord::default()));
        let run = a.run.as_ref().unwrap();
        assert_eq!(run.sel, 0);
        assert_eq!(
            run.failed_ids(),
            vec!["fnm", "nodejs", "npm-prefix", "codex"]
        );
        a.on_key(key(KeyCode::Char('r')));
        assert_eq!(a.screen, Screen::Plan);
        assert!(a.plan.as_ref().unwrap().1.starts_with("retry"));
    }

    #[test]
    fn bundle_toggle_saves_state() {
        let mut a = app();
        a.on_key(key(KeyCode::Char('2')));
        let first = a.bundle_ids()[0].clone();
        let cmds = a.on_key(key(KeyCode::Char(' ')));
        assert!(matches!(cmds[..], [Cmd::SaveState, Cmd::Refresh(_)]));
        assert!(a.subscribed(&first));
    }

    #[test]
    fn picker_on_first_run() {
        let mut a = app();
        let mut snap = a.snap.clone().unwrap();
        snap.first_run = true;
        a.screen = Screen::Loading;
        a.on_snapshot(snap, None);
        assert_eq!(a.screen, Screen::Picker);
        a.on_key(key(KeyCode::Char('n')));
        a.picker.insert("ai-agents".into());
        let targets = a.picker_targets();
        assert_eq!(&targets[..2], ["codex", "codex-sandbox-network"]);
        assert!(
            targets
                .iter()
                .all(|t| a.catalog.bundle_items("ai-agents").contains(t))
        );
        let cmds = a.on_key(key(KeyCode::Enter));
        assert!(matches!(cmds[..], [Cmd::SaveState, Cmd::Refresh(_)]));
        assert_eq!(a.state.as_ref().unwrap().bundles, vec!["ai-agents"]);
        assert_eq!(a.screen, Screen::Plan);
    }
}
