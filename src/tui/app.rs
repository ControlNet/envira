//! TUI state and key handling. Pure logic: side effects are returned as `Cmd`s for the runtime.

use std::cell::{Cell, RefCell};
use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;
use std::time::{Duration, Instant};

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers, MouseButton, MouseEvent, MouseEventKind};
use ratatui::layout::{Position, Rect};
use tokio::sync::watch;

use crate::catalog::{Catalog, Loaded};
use crate::exec::{Control, Event, Outcome};
use crate::facts::Facts;
use crate::latest::{Fetch, Latest};
use crate::plan::{Plan, Planner, UpdateTarget};
use crate::state::{Paths, RunRecord, State};
use crate::status::{ItemStatus, Label, Ownership, Snapshot};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Screen {
    Loading,
    Dashboard,
    Plan,
    Run,
}

/// The dashboard's two panels: Bundles on the left, Items on the right (← →).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Focus {
    Bundles,
    Items,
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
            Filter::Attention => "To do",
            Filter::Outdated => "Outdated",
            Filter::Missing => "Wanted",
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

/// The first visible row of a list. It follows the cursor when the cursor or the list changes;
/// the mouse wheel moves it alone, so scrolling never moves the selection.
#[derive(Debug, Default)]
pub struct View {
    top: Cell<usize>,
    /// `(cursor, rows)` at the last draw.
    seen: Cell<Option<(usize, usize)>>,
}

impl View {
    pub fn scroll(&self, up: bool) {
        let t = self.top.get();
        self.top.set(if up { t.saturating_sub(3) } else { t + 3 });
    }

    /// The top row for `height` visible rows out of `len`, called while drawing.
    pub fn top(&self, cursor: usize, len: usize, height: usize) -> usize {
        let mut top = self.top.get();
        if self.seen.get() != Some((cursor, len)) {
            self.seen.set(Some((cursor, len)));
            if cursor < top {
                top = cursor;
            } else if height > 0 && cursor >= top + height {
                top = cursor + 1 - height;
            }
        }
        top = top.min(len.saturating_sub(height));
        self.top.set(top);
        top
    }
}

/// A panel the mouse wheel scrolls.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Panel {
    Items,
    Bundles,
    Detail,
    Plan,
    Steps,
    Log,
}

/// What lies under the mouse, as recorded by the last draw. Every click maps to what a key
/// already does, so the mouse never reaches anything the keyboard cannot.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Hit {
    Panel(Panel),
    ItemRow(usize),
    /// The `[ ]` of an item row.
    ItemMark(usize),
    BundleRow(usize),
    /// The `● ○` of a bundle row.
    BundleMark(usize),
    Filter(Filter),
    Detail(DetailTab),
    /// An overview counter: shows that set in Items.
    Counter(Filter),
    /// A name in To install: jumps to it in Items.
    Unit(String),
    Key(KeyEvent),
    PlanRow(usize),
    StepRow(usize),
    /// A control of the sudo password dialog.
    Sudo(crate::sudo_prompt::Focus),
}

/// What a plan on screen was made from, so it can be made again in user mode.
#[derive(Debug, Clone)]
pub enum PlanReq {
    Install(Vec<String>),
    /// `u`: every outdated tool.
    Update(Vec<UpdateTarget>),
    Revert(Vec<String>),
    Retry(crate::plan::RetryTargets),
}

/// The key a key-bar entry stands for, when clicking it should press it. Navigation entries
/// have no single key, and stopping a run stays keyboard only, so a stray click cannot kill an
/// install.
pub fn bar_key(name: &str, key: &str) -> Option<KeyEvent> {
    if matches!(name, "Cancel running" | "Stop after running") {
        return None;
    }
    let code = match key {
        "space" => KeyCode::Char(' '),
        "enter" => KeyCode::Enter,
        "esc" => KeyCode::Esc,
        "tab" => KeyCode::Tab,
        _ => {
            let mut cs = key.chars();
            match (cs.next(), cs.next()) {
                (Some(c), None) => KeyCode::Char(c),
                _ => return None,
            }
        }
    };
    Some(KeyEvent::new(code, KeyModifiers::NONE))
}

/// Side effects requested by the app.
#[derive(Debug)]
pub enum Cmd {
    Quit,
    /// Re-detect everything; latest versions per the fetch mode.
    Refresh(Fetch),
    /// Re-read catalog layers, then refresh.
    Reload,
    /// Keep these bundles on this machine: saved as a change to the state on disk, not as the
    /// whole state this screen read (a run may have finished since).
    SaveBundles {
        bundles: Vec<String>,
        /// Ids shown so far (a first run counts the whole catalog).
        seen: BTreeSet<String>,
    },
    /// Record these tools as managed, likewise.
    Adopt(Vec<(String, crate::state::ToolRecord)>),
    StartRun {
        plan: Plan,
        command: String,
    },
    /// Write the plan as a shell script next to the state.
    ExportScript(Plan),
    /// Leave the screen and run `envira self-update`.
    SelfUpdate,
    /// Check the password typed in the sudo dialog.
    Sudo(crate::sudo_prompt::Secret),
}

#[derive(Debug, Clone)]
pub enum UnitState {
    Queued,
    Running(Instant),
    Done(Outcome, Duration),
}

/// The toast while `u` waits for the update check.
const CHECKING: &str = "Checking for updates…";

/// Lines of a unit's log the run view keeps (the log file has all of them).
pub const LOG_LINES: usize = 20_000;
/// Bytes of a unit's log the run view keeps (lines can be up to 64 KiB each).
pub const LOG_BYTES: usize = 2 << 20;

pub struct RunView {
    pub plan: Plan,
    pub run_id: String,
    pub units: Vec<UnitState>,
    /// The latest `LOG_LINES` lines or `LOG_BYTES` bytes (up to a quarter more) of each unit.
    pub logs: Vec<Vec<String>>,
    /// Bytes in each of `logs`.
    pub log_bytes: Vec<usize>,
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
    /// The log takes the whole width.
    pub zoom: bool,
}

impl RunView {
    pub fn new(plan: Plan, run_id: String, ctl: Option<watch::Sender<Control>>) -> RunView {
        let n = plan.units.len();
        RunView {
            plan,
            run_id,
            units: vec![UnitState::Queued; n],
            logs: vec![Vec::new(); n],
            log_bytes: vec![0; n],
            sel: 0,
            follow: true,
            scroll: 0,
            started: Instant::now(),
            finished: None,
            shell_env: None,
            ctl,
            stopping: false,
            zoom: false,
        }
    }

    pub fn count(&self, f: impl Fn(&UnitState) -> bool) -> usize {
        self.units.iter().filter(|u| f(u)).count()
    }

    /// Units to retry: failed, blocked, cancelled, or skipped after a stop.
    fn failed_units(&self) -> impl Iterator<Item = &crate::plan::Unit> {
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
            .map(|(u, _)| u)
    }
}

/// One toast line for catalog load warnings; the rest are in `envira doctor`.
pub fn catalog_warning(warnings: &[String]) -> Option<String> {
    let first = warnings.first()?;
    Some(match warnings.len() {
        1 => format!("Catalog: {first}"),
        n => format!("Catalog: {first} (+{} more: envira doctor)", n - 1),
    })
}

pub struct App {
    pub catalog: Arc<Catalog>,
    /// Catalog load warnings (remote unreachable, cache unusable), shown once the dashboard opens.
    pub catalog_warnings: Vec<String>,
    pub facts: Arc<Facts>,
    pub paths: Paths,
    pub prefer_system: bool,
    pub state: Option<State>,
    pub snap: Option<Snapshot>,
    pub detecting: bool,
    /// A latest-version check is running in the background.
    pub checking_latest: bool,
    /// A newer envira release, when there is one.
    pub self_latest: Option<String>,
    /// envira was updated during this session; a restart runs the new version.
    pub self_updated: bool,
    pub screen: Screen,
    pub focus: Focus,
    pub cursor: usize,
    /// What Enter installs: the missing items of the kept bundles plus the user's changes.
    pub selected: BTreeSet<String>,
    /// Fill `selected` from the kept bundles on the next snapshot (on entry and after a run).
    reseed: bool,
    pub filter: Filter,
    pub search: String,
    pub searching: bool,
    pub bundle_cursor: usize,
    pub detail: DetailTab,
    pub detail_scroll: usize,
    pub plan: Option<(Plan, String)>,
    /// What `plan` was made from.
    plan_req: Option<PlanReq>,
    /// `plan` is made in user mode (chosen in the sudo dialog).
    pub user_mode: bool,
    /// `facts` without sudo, for user mode.
    user_facts: Arc<Facts>,
    /// The sudo password dialog over the plan.
    pub sudo: Option<crate::sudo_prompt::Prompt>,
    /// `--scope` was given: the sudo dialog does not offer user mode.
    pub scope_fixed: bool,
    pub plan_cursor: usize,
    pub run: Option<RunView>,
    pub toast: Option<(String, Instant)>,
    /// The toast is a warning that routine notices (the update count) must not cover.
    pub toast_warns: bool,
    pub help: bool,
    pub tick: u64,
    /// SIGTERM/SIGHUP arrived during a run: quit once it has wound down.
    pub quitting: bool,
    /// The last run left these units unfinished: `(run id, ids, whether it finished)`.
    /// The last run, when it left something unfinished: its id, what to retry and how (as
    /// `envira retry` would), and whether it finished.
    pub last_run: Option<(String, crate::plan::RetryTargets, bool)>,
    /// A retry waiting for detection to catch up with the run that just ended (`r`).
    pending_retry: Option<crate::plan::RetryTargets>,
    /// Mouse targets of the last frame; drawing fills it, later entries lie on top.
    pub hits: RefCell<Vec<(Rect, Hit)>>,
    pub items_view: View,
    pub bundles_view: View,
    pub plan_view: View,
    pub steps_view: View,
}

impl App {
    pub fn new(
        loaded: &Loaded,
        catalog: Arc<Catalog>,
        facts: Arc<Facts>,
        paths: Paths,
        prefer_system: bool,
    ) -> App {
        let user_facts = Arc::new(facts.without_sudo());
        App {
            catalog,
            catalog_warnings: loaded.warnings.clone(),
            facts,
            paths,
            prefer_system,
            state: None,
            snap: None,
            detecting: true,
            checking_latest: false,
            self_latest: None,
            self_updated: false,
            screen: Screen::Loading,
            focus: Focus::Items,
            cursor: 0,
            selected: BTreeSet::new(),
            reseed: true,
            filter: Filter::All,
            search: String::new(),
            searching: false,
            bundle_cursor: 0,
            detail: DetailTab::Info,
            detail_scroll: 0,
            plan: None,
            plan_req: None,
            user_mode: false,
            user_facts,
            sudo: None,
            scope_fixed: false,
            plan_cursor: 0,
            run: None,
            toast: None,
            toast_warns: false,
            help: false,
            tick: 0,
            quitting: false,
            last_run: None,
            pending_retry: None,
            hits: RefCell::new(Vec::new()),
            items_view: View::default(),
            bundles_view: View::default(),
            plan_view: View::default(),
            steps_view: View::default(),
        }
    }

    pub fn notify(&mut self, msg: impl Into<String>) {
        self.toast = Some((msg.into(), Instant::now()));
        self.toast_warns = false;
    }

    pub fn warn(&mut self, msg: impl Into<String>) {
        self.notify(msg);
        self.toast_warns = true;
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
        // Before anything is saved, keep the in-memory state of a first run.
        self.state = state.or(self.state.take());
        self.detecting = false;
        self.selected.retain(|id| snap.get(id).is_some());
        if first {
            self.screen = Screen::Dashboard;
            if snap.first_run {
                // Nothing kept yet: start on Bundles. The whole catalog counts as seen, so
                // nothing shows as new after the first bundle is kept or the first run.
                let mut st = State::default();
                st.mark_all_seen(&self.catalog);
                self.state = Some(st);
                self.focus = Focus::Bundles;
            } else {
                self.load_last_run();
            }
            // After the last-run toast: the overview already shows what that run left.
            if let Some(w) = catalog_warning(&self.catalog_warnings) {
                self.warn(w);
            }
        }
        self.snap = Some(snap);
        if let Some(t) = self.pending_retry.take() {
            let title = format!("retry · {} unfinished", t.len());
            self.open_req(PlanReq::Retry(t), title);
        }
        if self.reseed {
            self.reseed = false;
            self.selected = self
                .snap
                .as_ref()
                .map(|s| s.to_install().into_iter().collect())
                .unwrap_or_default();
        }
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
        self.make_retry_install(targets, &BTreeMap::new())
    }

    /// `make_plan` after a failed run; `done`: the steps each install got through (see
    /// `Planner::finishing`).
    fn make_retry_install(&self, targets: &[String], done: &crate::plan::InstallsDone) -> Plan {
        let present = self.present();
        let mut p = self.planner(&present);
        p.finishing = done.clone();
        p.plan(targets, false)
    }

    pub fn make_update_plan(&self, targets: &[UpdateTarget]) -> Plan {
        let present = self.present();
        self.planner(&present).update(targets)
    }

    /// A planner for this machine, without sudo in user mode.
    fn planner<'a>(&'a self, present: &'a BTreeSet<String>) -> Planner<'a> {
        let facts = if self.user_mode {
            &self.user_facts
        } else {
            &self.facts
        };
        let mut p = Planner::new(&self.catalog, facts, present);
        p.prefer_system = self.prefer_system && !self.user_mode;
        p
    }

    fn build(&self, req: &PlanReq) -> Plan {
        match req {
            PlanReq::Install(t) => self.make_selection_plan(t),
            PlanReq::Update(t) => self.make_update_plan(t),
            PlanReq::Revert(t) => self.make_revert(t, None),
            PlanReq::Retry(t) => self.retry_plan(t),
        }
    }

    /// Makes the plan `req` asks for (not in user mode) and opens it.
    fn open_req(&mut self, req: PlanReq, title: String) {
        self.user_mode = false;
        let plan = self.build(&req);
        self.plan_req = Some(req);
        self.open_plan(plan, title);
    }

    /// The selected tools that are installed with a newer version out.
    fn selected_outdated(&self, targets: &[String]) -> Vec<UpdateTarget> {
        let Some(snap) = &self.snap else {
            return vec![];
        };
        targets
            .iter()
            .filter_map(|id| snap.get(id))
            .filter(|i| i.label == crate::status::Label::Outdated)
            .filter_map(|i| i.update_target(false).ok())
            .collect()
    }

    /// Enter on the dashboard: outdated tools in `targets` are updated, the rest planned as
    /// an install (missing ones installed, present ones skipped).
    pub fn make_selection_plan(&self, targets: &[String]) -> Plan {
        let updates = self.selected_outdated(targets);
        let rest: Vec<String> = targets
            .iter()
            .filter(|id| !updates.iter().any(|u| &u.id == *id))
            .cloned()
            .collect();
        // Updates first: an install may depend on a tool being updated.
        let mut plan = self.make_update_plan(&updates);
        plan.extend(self.make_plan(&rest));
        plan
    }

    /// `u`: every outdated tool, whatever is selected.
    fn update_targets(&self) -> (Vec<UpdateTarget>, String) {
        let Some(snap) = &self.snap else {
            return (vec![], String::new());
        };
        let targets: Vec<UpdateTarget> = snap
            .outdated()
            .iter()
            .filter_map(|id| snap.get(id).and_then(|i| i.update_target(false).ok()))
            .collect();
        let n = targets.len();
        (targets, format!("update · {n} outdated"))
    }

    /// Latest-version answers from the background check.
    pub fn on_latest(&mut self, answers: BTreeMap<String, Latest>) {
        self.checking_latest = false;
        let waiting = self.toast_text() == Some(CHECKING);
        if let Some(snap) = &mut self.snap {
            snap.apply_latest(&answers);
            // The overview shows the count; a toast only answers a `u` pressed while checking.
            if waiting {
                let n = snap.outdated().len();
                self.notify(if n > 0 {
                    format!("{n} outdated")
                } else {
                    "Nothing is outdated".into()
                });
            }
        }
        self.clamp();
    }

    /// A newer envira is out: the header shows it; say so once unless a warning is up.
    pub fn on_self_latest(&mut self, newer: Option<String>) {
        if self.self_updated {
            return;
        }
        if let Some(v) = &newer
            && self.self_latest.as_ref() != Some(v)
            && !(self.toast_warns && self.toast_text().is_some())
        {
            self.notify(format!("envira {v} is out · U updates envira"));
        }
        self.self_latest = newer;
    }

    /// Retries each unit the way it ran: install/apply, update or revert.
    fn retry_plan(&self, t: &crate::plan::RetryTargets) -> Plan {
        let mut plan = Plan::default();
        if !t.install.is_empty() {
            plan.extend(self.make_retry_install(&t.install, &t.install_done));
        }
        if !t.update.is_empty() {
            let (targets, skipped) = self
                .snap
                .as_ref()
                .map(|s| s.update_targets(&t.update))
                .unwrap_or_default();
            let mut p = self.make_update_plan(&targets);
            p.skipped.extend(skipped);
            plan.extend(p);
        }
        if !t.revert.is_empty() {
            plan.extend(self.make_revert(&t.revert, Some(&t.revert_done)));
        }
        plan
    }

    /// Reverts `targets`; `retrying` reverts that stopped halfway, with the steps each got
    /// through (see `Planner::retrying`).
    fn make_revert(
        &self,
        targets: &[String],
        retrying: Option<&BTreeMap<String, Vec<String>>>,
    ) -> Plan {
        let present = self.present();
        let mut p = self.planner(&present);
        p.retrying = retrying.cloned();
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
        self.plan_view = View::default();
        self.screen = Screen::Plan;
    }

    // --------------------------------------------------------------------------------------------
    // Keys
    // --------------------------------------------------------------------------------------------

    pub fn on_key(&mut self, k: KeyEvent) -> Vec<Cmd> {
        if self.sudo.is_some() {
            return self.sudo_key(k);
        }
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
            Screen::Dashboard => self.dashboard_key(k),
            Screen::Plan => self.plan_key(k),
            Screen::Run => self.run_key(k),
        }
    }

    /// Records adoptable tools as managed without reinstalling them (`envira adopt`): the
    /// selected ones, else the one under the cursor.
    fn adopt(&mut self) -> Vec<Cmd> {
        let Some(snap) = &self.snap else {
            return vec![];
        };
        let mut ids: Vec<String> = self
            .selected
            .iter()
            .filter(|id| snap.get(id).is_some_and(|i| i.adoption().is_some()))
            .cloned()
            .collect();
        if ids.is_empty() {
            let Some(it) = self.current() else {
                return vec![];
            };
            if it.adoption().is_none() {
                let why = match it.ownership {
                    Some(Ownership::Managed) => "is already managed",
                    Some(Ownership::External) => {
                        "is external: envira cannot tell how it was installed"
                    }
                    _ if !it.present => "is not installed",
                    _ => "cannot be adopted",
                };
                self.notify(format!("{} {why}", it.id));
                return vec![];
            }
            ids.push(it.id.clone());
        }
        let records: Vec<_> = ids
            .iter()
            .filter_map(|id| Some((id.clone(), snap.get(id)?.adoption()?)))
            .collect();
        let state = self.state.get_or_insert_with(State::default);
        for (id, rec) in &records {
            state.tools.insert(id.clone(), rec.clone());
        }
        self.notify(match &ids[..] {
            [one] => format!("{one} is now managed"),
            many => format!("{} tools are now managed", many.len()),
        });
        self.detecting = true;
        vec![Cmd::Adopt(records), Cmd::Refresh(Fetch::CacheOnly)]
    }

    /// Records a mouse target while drawing.
    pub fn hit(&self, area: Rect, h: Hit) {
        if area.width > 0 && area.height > 0 {
            self.hits.borrow_mut().push((area, h));
        }
    }

    pub fn on_mouse(&mut self, m: MouseEvent) -> Vec<Cmd> {
        let at = Position::new(m.column, m.row);
        let find = |panel: bool| {
            self.hits
                .borrow()
                .iter()
                .rev()
                .find(|(r, h)| r.contains(at) && matches!(h, Hit::Panel(_)) == panel)
                .map(|(_, h)| h.clone())
        };
        match m.kind {
            MouseEventKind::Down(MouseButton::Left) if self.sudo.is_some() => match find(false) {
                Some(Hit::Sudo(f)) => self.sudo_click(f),
                _ => vec![],
            },
            _ if self.sudo.is_some() => vec![],
            MouseEventKind::Down(MouseButton::Left) => {
                if self.help {
                    self.help = false;
                    return vec![];
                }
                self.searching = false;
                match find(false) {
                    Some(h) => self.click(h),
                    None => vec![],
                }
            }
            MouseEventKind::ScrollUp | MouseEventKind::ScrollDown if !self.help => {
                if let Some(Hit::Panel(p)) = find(true) {
                    self.wheel(p, m.kind == MouseEventKind::ScrollUp);
                }
                vec![]
            }
            _ => vec![],
        }
    }

    fn click(&mut self, h: Hit) -> Vec<Cmd> {
        let space = KeyEvent::new(KeyCode::Char(' '), KeyModifiers::NONE);
        match h {
            Hit::ItemRow(i) | Hit::ItemMark(i) => {
                self.focus = Focus::Items;
                self.cursor = i;
                self.detail_scroll = 0;
                self.clamp();
                if matches!(h, Hit::ItemMark(_)) {
                    let cmds = self.on_key(space);
                    // Space moves on to the next row for the keyboard; a click stays put.
                    self.cursor = i;
                    self.clamp();
                    return cmds;
                }
            }
            Hit::BundleRow(i) | Hit::BundleMark(i) => {
                self.focus = Focus::Bundles;
                self.bundle_cursor = i.min(self.bundle_ids().len().saturating_sub(1));
                if matches!(h, Hit::BundleMark(_)) {
                    return self.on_key(space);
                }
            }
            Hit::Filter(f) | Hit::Counter(f) => {
                self.filter = f;
                self.focus = Focus::Items;
                self.cursor = 0;
                self.clamp();
            }
            Hit::Detail(d) => {
                self.detail = d;
                self.detail_scroll = 0;
            }
            Hit::Unit(id) => {
                if !self.rows().iter().any(|r| r.id == id) {
                    self.filter = Filter::All;
                    self.search.clear();
                }
                if let Some(i) = self.rows().iter().position(|r| r.id == id) {
                    self.focus = Focus::Items;
                    self.cursor = i;
                    self.detail_scroll = 0;
                }
            }
            Hit::Key(k) => return self.on_key(k),
            Hit::Sudo(f) => return self.sudo_click(f),
            Hit::PlanRow(i) => self.plan_cursor = i,
            Hit::StepRow(i) => {
                if let Some(run) = &mut self.run {
                    run.follow = false;
                    run.sel = i.min(run.units.len().saturating_sub(1));
                    run.scroll = 0;
                }
            }
            Hit::Panel(_) => {}
        }
        vec![]
    }

    /// Scrolls the view of the panel under the pointer by three rows, focused or not; the
    /// cursor and the selection stay where they are.
    fn wheel(&mut self, p: Panel, up: bool) {
        match p {
            Panel::Items => self.items_view.scroll(up),
            Panel::Bundles => self.bundles_view.scroll(up),
            Panel::Plan => self.plan_view.scroll(up),
            Panel::Steps => self.steps_view.scroll(up),
            Panel::Detail => {
                self.detail_scroll = if up {
                    self.detail_scroll.saturating_sub(3)
                } else {
                    self.detail_scroll + 3
                }
            }
            // Like PgUp / PgDn: lines up from the bottom of the log.
            Panel::Log => {
                if let Some(run) = &mut self.run {
                    run.scroll = if up {
                        run.scroll + 3
                    } else {
                        run.scroll.saturating_sub(3)
                    };
                }
            }
        }
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
            KeyCode::Char('?') => self.help = true,
            // Esc only clears the search: the selection is the install set.
            KeyCode::Esc => {
                self.search.clear();
                self.clamp();
            }
            KeyCode::Left | KeyCode::Char('h') => self.focus = Focus::Bundles,
            KeyCode::Right | KeyCode::Char('l') => self.focus = Focus::Items,
            KeyCode::Enter => self.install_selection(),
            KeyCode::Char('n') => self.selected.clear(),
            KeyCode::Char('U') if self.self_updated => {
                self.notify("envira is already updated: restart it to use the new version")
            }
            KeyCode::Char('U') => return vec![Cmd::SelfUpdate],
            KeyCode::Tab | KeyCode::BackTab => {
                let tabs = [DetailTab::Info, DetailTab::Commands, DetailTab::Log];
                let i = tabs.iter().position(|d| *d == self.detail).unwrap_or(0);
                let step = if k.code == KeyCode::Tab {
                    1
                } else {
                    tabs.len() - 1
                };
                self.detail = tabs[(i + step) % tabs.len()];
                self.detail_scroll = 0;
            }
            KeyCode::Char('J') => self.detail_scroll += 1,
            KeyCode::Char('K') => self.detail_scroll = self.detail_scroll.saturating_sub(1),
            KeyCode::Char('/') => {
                self.focus = Focus::Items;
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
            KeyCode::Char('r') => {
                self.detecting = true;
                return vec![Cmd::Reload];
            }
            KeyCode::Char('A') => return self.adopt(),
            KeyCode::Char('S') => {
                if !self.facts.privilege.can_elevate() {
                    self.notify("No sudo here: new installs go to user scope");
                    return vec![];
                }
                self.prefer_system = !self.prefer_system;
                self.notify(format!(
                    "New installs go to {} scope",
                    if self.prefer_system { "system" } else { "user" }
                ));
                // Missing items show the route they would take in the new scope.
                self.detecting = true;
                return vec![Cmd::Refresh(Fetch::CacheOnly)];
            }
            KeyCode::Char('u') => {
                let (targets, what) = self.update_targets();
                if targets.is_empty() {
                    self.notify(if self.checking_latest {
                        CHECKING
                    } else {
                        "Nothing is outdated (R checks again)"
                    });
                } else {
                    self.open_req(PlanReq::Update(targets), what);
                }
            }
            KeyCode::Char('x') => {
                let present = self.present();
                let mut t: Vec<String> = self
                    .selected_ids()
                    .into_iter()
                    .filter(|id| present.contains(id))
                    .collect();
                if t.is_empty() {
                    t = self
                        .current()
                        .map(|i| vec![i.id.clone()])
                        .unwrap_or_default();
                }
                t.retain(|id| self.catalog.configs.contains_key(id));
                if t.is_empty() {
                    self.notify("Only configs can be reverted");
                } else {
                    let n = t.len();
                    self.open_req(PlanReq::Revert(t), format!("revert · {n} configs"));
                }
            }
            KeyCode::Char('t') => match self.last_run.clone() {
                Some((id, t, _)) => {
                    let n = t.len();
                    self.open_req(
                        PlanReq::Retry(t),
                        format!("retry · {n} unfinished from run {id}"),
                    );
                }
                None => self.notify("The last run left nothing to retry"),
            },
            _ => match self.focus {
                Focus::Bundles => return self.bundles_key(k),
                Focus::Items => self.table_key(k),
            },
        }
        vec![]
    }

    /// The selection in catalog order.
    fn selected_ids(&self) -> Vec<String> {
        self.catalog
            .item_ids()
            .into_iter()
            .filter(|id| self.selected.contains(id))
            .collect()
    }

    /// Enter on the dashboard: install what is selected and update what of it is outdated.
    fn install_selection(&mut self) {
        let t = self.selected_ids();
        if t.is_empty() {
            self.notify("Nothing selected: space selects bundles (left) and items (right)");
            return;
        }
        let items = |n: usize| format!("{n} {}", super::ui::plural(n, "item", "items"));
        let upd = self.selected_outdated(&t).len();
        let rest = t.len() - upd;
        let title = match (rest, upd) {
            (_, 0) => format!("install · {}", items(rest)),
            (0, _) => format!("update · {}", items(upd)),
            _ => format!("install · {} · update · {upd}", items(rest)),
        };
        self.open_req(PlanReq::Install(t), title);
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
            _ => {}
        }
        self.detail_scroll = 0;
    }

    /// Items of a bundle that are not installed and can be.
    fn missing_items(&self, bundle: &str) -> Vec<String> {
        let Some(snap) = &self.snap else {
            return vec![];
        };
        self.catalog
            .bundle_items(bundle)
            .into_iter()
            .filter(|i| {
                snap.get(i)
                    .is_some_and(|s| !s.present && s.label != Label::NotApplicable)
            })
            .collect()
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
            // Keeps the bundle on this machine (like `envira install <bundle>`) and selects
            // what it misses; dropping it unselects its items unless another kept bundle has them.
            KeyCode::Char(' ') => {
                let Some(b) = ids.get(self.bundle_cursor).cloned() else {
                    return vec![];
                };
                let name = self.catalog.bundle_name(&b).to_string();
                let mut kept = self
                    .state
                    .as_ref()
                    .map(|s| s.bundles.clone())
                    .unwrap_or_default();
                if let Some(pos) = kept.iter().position(|x| *x == b) {
                    kept.remove(pos);
                    let others: BTreeSet<String> = kept
                        .iter()
                        .flat_map(|o| self.catalog.bundle_items(o))
                        .collect();
                    for i in self.catalog.bundle_items(&b) {
                        if !others.contains(&i) {
                            self.selected.remove(&i);
                        }
                    }
                    self.notify(format!("No longer following {name}"));
                } else {
                    kept.push(b.clone());
                    let missing = self.missing_items(&b);
                    let n = missing.len();
                    self.selected.extend(missing);
                    self.notify(format!(
                        "Following {name}: {n} {} to install",
                        super::ui::plural(n, "item", "items")
                    ));
                }
                let state = self.state.get_or_insert_with(State::default);
                state.bundles = kept.clone();
                let seen = state.seen.clone();
                return vec![
                    Cmd::SaveBundles {
                        bundles: kept,
                        seen,
                    },
                    Cmd::Refresh(Fetch::CacheOnly),
                ];
            }
            _ => {}
        }
        vec![]
    }

    fn sudo_key(&mut self, k: KeyEvent) -> Vec<Cmd> {
        use crate::sudo_prompt::Action;
        let Some(prompt) = &mut self.sudo else {
            return vec![];
        };
        match prompt.key(k) {
            Some(Action::Submit(password)) => vec![Cmd::Sudo(password)],
            Some(Action::UserMode) => {
                self.switch_to_user_mode();
                vec![]
            }
            Some(Action::Back) => {
                self.sudo = None;
                vec![]
            }
            None => vec![],
        }
    }

    fn sudo_click(&mut self, f: crate::sudo_prompt::Focus) -> Vec<Cmd> {
        use crate::sudo_prompt::Focus;
        match f {
            Focus::Password => {
                if let Some(p) = &mut self.sudo {
                    p.focus = Focus::Password;
                }
                vec![]
            }
            Focus::UserMode | Focus::Back => {
                if let Some(p) = &mut self.sudo {
                    p.focus = f;
                }
                self.sudo_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE))
            }
        }
    }

    /// What sudo said to the password typed in the dialog: run the plan, or ask again.
    pub fn on_sudo(&mut self, check: crate::sudo_prompt::Check) -> Vec<Cmd> {
        if check != crate::sudo_prompt::Check::Ok {
            if let Some(p) = &mut self.sudo {
                p.failed(check);
            }
            return vec![];
        }
        self.sudo = None;
        let Some((plan, title)) = self.plan.take() else {
            return vec![];
        };
        self.selected.clear();
        vec![Cmd::StartRun {
            plan,
            command: title.split(" · ").next().unwrap_or("run").to_string(),
        }]
    }

    /// The plan on screen made again without sudo, to review before it runs.
    fn switch_to_user_mode(&mut self) {
        self.sudo = None;
        let Some(req) = self.plan_req.clone() else {
            return;
        };
        self.user_mode = true;
        let plan = self.build(&req);
        let title = self
            .plan
            .as_ref()
            .map(|(_, t)| t.clone())
            .unwrap_or_default();
        if plan.is_empty() {
            self.plan = None;
            self.screen = Screen::Dashboard;
            let why = plan
                .skipped
                .first()
                .map(|(id, w)| format!(" ({id}: {w})"))
                .unwrap_or_default();
            self.notify(format!("Nothing to do in user mode{why}"));
            return;
        }
        let title = if title.ends_with(" · user mode") {
            title
        } else {
            format!("{title} · user mode")
        };
        self.plan = Some((plan, title));
        self.plan_cursor = 0;
        self.plan_view = View::default();
    }

    /// sudo wanted a password after all (its ticket ran out since the plan was made): ask in
    /// the dialog over this plan.
    pub fn ask_sudo(&mut self, plan: Plan, command: String) {
        let n = plan.sudo_units(&self.facts);
        self.plan = Some((plan, command));
        self.screen = Screen::Plan;
        self.sudo = Some(crate::sudo_prompt::Prompt::new(
            n,
            "Back",
            !self.scope_fixed,
        ));
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
                if self.facts.sudo_asks_password() && plan.needs_sudo(&self.facts) {
                    self.sudo = Some(crate::sudo_prompt::Prompt::new(
                        plan.sudo_units(&self.facts),
                        "Back",
                        !self.scope_fixed,
                    ));
                    return vec![];
                }
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
        if k.code == KeyCode::Char('?') {
            self.help = true;
            return vec![];
        }
        let Some(run) = &mut self.run else {
            self.screen = Screen::Dashboard;
            return vec![];
        };
        let n = run.units.len();
        let finished = run.finished.is_some();
        let ctrl_c = k.modifiers.contains(KeyModifiers::CONTROL) && k.code == KeyCode::Char('c');
        if ctrl_c && !finished {
            // As on the command line: the first stops after the running steps, the next
            // cancels them.
            let again = run.stopping;
            if let Some(ctl) = &run.ctl {
                ctl.send_modify(|c| {
                    c.stop = true;
                    if again {
                        c.cancel_seq += 1;
                    }
                });
            }
            run.stopping = true;
            self.notify(if again {
                "Cancelling the running steps"
            } else {
                "Stopping after the running steps (Ctrl-C again cancels them)"
            });
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
            KeyCode::Char('z') => run.zoom = !run.zoom,
            KeyCode::Char('c') if !finished => {
                if let Some(ctl) = &run.ctl {
                    ctl.send_modify(|c| c.cancel_seq += 1);
                }
                self.notify("Cancelling the running steps");
            }
            KeyCode::Char('r') if finished => {
                let t = crate::plan::RetryTargets::new(
                    run.failed_units()
                        .map(|u| (u.id.as_str(), u.action.as_str())),
                )
                .progress(run.finished.iter().flat_map(|(rec, _)| &rec.units), None);
                if t.is_empty() {
                    self.notify("Nothing to retry");
                } else {
                    // Planned once detection has seen what the run changed: a build that
                    // went through is not run again for a copy that failed after it.
                    self.run = None;
                    self.screen = Screen::Dashboard;
                    self.pending_retry = Some(t);
                    self.detecting = true;
                    self.notify("Checking what the run changed before the retry");
                    return vec![Cmd::Refresh(Fetch::IfStale)];
                }
            }
            KeyCode::Enter | KeyCode::Esc if finished => {
                self.run = None;
                self.screen = Screen::Dashboard;
                self.reseed = true;
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

    /// Offers to retry what the last run left unfinished (failed, or interrupted by a crash,
    /// a closed terminal or a stop).
    pub fn load_last_run(&mut self) {
        self.last_run = None;
        let Some(id) = self.state.as_ref().and_then(|s| s.last_run.clone()) else {
            return;
        };
        let Ok(rec) = RunRecord::load(&self.paths, &id) else {
            return;
        };
        let t = crate::plan::RetryTargets::new(
            rec.unfinished_units()
                .map(|u| (u.id.as_str(), u.action.as_str())),
        )
        .progress(&rec.units, Some(&self.paths.run_dir(&id)));
        if t.is_empty() {
            return;
        }
        if rec.finished.is_none()
            && crate::state::RunLock::acquire(&self.paths, &self.facts).is_err()
        {
            self.notify(format!("Run {id} is still in progress in another envira"));
            return;
        }
        let n = t.len();
        self.notify(if rec.finished.is_none() {
            format!("Run {id} was interrupted and left {n} unfinished · t retries")
        } else {
            format!("Run {id} left {n} unfinished · t retries")
        });
        self.last_run = Some((id, t, rec.finished.is_some()));
    }

    /// SIGTERM or SIGHUP: true when the TUI can exit now; during a run it stops the run and
    /// cancels the running step first, so no installer is left running without envira.
    pub fn terminate(&mut self) -> bool {
        match &mut self.run {
            Some(run) if run.finished.is_none() => {
                if let Some(ctl) = &run.ctl {
                    ctl.send_modify(|c| {
                        c.stop = true;
                        c.cancel_seq += 1;
                    });
                }
                run.stopping = true;
                self.quitting = true;
                self.notify("Terminating: cancelling the running steps");
                false
            }
            _ => true,
        }
    }

    pub fn start_run(&mut self, plan: Plan, run_id: String, ctl: Option<watch::Sender<Control>>) {
        self.last_run = None;
        self.run = Some(RunView::new(plan, run_id, ctl));
        self.steps_view = View::default();
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
            Event::Line(i, l) => {
                let (log, bytes) = (&mut run.logs[i], &mut run.log_bytes[i]);
                *bytes += l.len();
                log.push(l);
                if log.len() > LOG_LINES + LOG_LINES / 4 || *bytes > LOG_BYTES + LOG_BYTES / 4 {
                    // Drop the oldest lines a chunk at a time (draining moves the rest).
                    let mut n = log.len().saturating_sub(LOG_LINES);
                    let mut freed: usize = log[..n].iter().map(String::len).sum();
                    while *bytes - freed > LOG_BYTES && n + 1 < log.len() {
                        freed += log[n].len();
                        n += 1;
                    }
                    log.drain(..n);
                    *bytes -= freed;
                }
            }
            Event::UnitFinished(i, o, d) => run.units[i] = UnitState::Done(o, d),
            Event::ShellEnv(r) => run.shell_env = Some(r),
            Event::Notice(m) => self.notify(m),
            Event::Finished(rec) => {
                let unrecorded = rec.unrecorded.clone();
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
                if let Some(e) = unrecorded {
                    self.notify(format!(
                        "the run could not be recorded ({e}): retry may not know what it did"
                    ));
                }
            }
        }
    }
}

#[cfg(test)]
pub mod tests {
    use super::*;
    use crate::catalog::load::{embedded_layer, load};
    use crate::plan::Kind;
    use crate::state::Scope;
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
            reason: (label == Label::NotApplicable).then(|| "needs an NVIDIA GPU".into()),
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
            cache_dir: "/home/user/.cache/envira".into(),
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

    /// The sample app on a machine where sudo asks for a password, with `ids` selected.
    fn asking_sudo(ids: &[&str]) -> App {
        let mut a = app();
        let mut f = (*a.facts).clone();
        f.privilege = crate::facts::Privilege::Sudo {
            passwordless: false,
        };
        a.facts = Arc::new(f);
        a.selected = ids.iter().map(|s| s.to_string()).collect();
        a.on_key(key(KeyCode::Enter));
        assert_eq!(a.screen, Screen::Plan);
        a
    }

    #[test]
    fn a_plan_needing_a_sudo_password_asks_for_it_or_user_mode() {
        use crate::sudo_prompt::Check;
        let mut a = asking_sudo(&["nmap", "lsd"]);
        assert!(a.plan.as_ref().unwrap().0.needs_sudo(&a.facts));
        // Enter asks for the password instead of running.
        assert!(a.on_key(key(KeyCode::Enter)).is_empty());
        assert_eq!(a.sudo.as_ref().unwrap().items, 2);
        chars(&mut a, "pw");
        let cmds = a.on_key(key(KeyCode::Enter));
        assert!(matches!(&cmds[..], [Cmd::Sudo(p)] if p.as_str() == "pw"));
        // A wrong one: asked again, nothing runs.
        assert!(a.on_sudo(Check::Wrong).is_empty());
        assert!(a.sudo.as_ref().unwrap().error.is_some());
        chars(&mut a, "pw2");
        a.on_key(key(KeyCode::Enter));
        let cmds = a.on_sudo(Check::Ok);
        assert!(a.sudo.is_none());
        let [Cmd::StartRun { plan, command }] = &cmds[..] else {
            panic!("{cmds:?}");
        };
        assert_eq!(command, "install");
        assert!(plan.needs_sudo(&a.facts));
    }

    #[test]
    fn user_mode_makes_the_plan_again_without_sudo() {
        let mut a = asking_sudo(&["nmap", "lsd"]);
        a.on_key(key(KeyCode::Enter));
        // Esc goes back to the plan.
        a.on_key(key(KeyCode::Esc));
        assert!(a.sudo.is_none() && a.screen == Screen::Plan);
        a.on_key(key(KeyCode::Enter));
        a.on_key(key(KeyCode::Right));
        assert!(a.on_key(key(KeyCode::Enter)).is_empty());
        assert!(a.sudo.is_none());
        let (plan, title) = a.plan.as_ref().unwrap();
        assert!(title.ends_with(" · user mode"), "{title}");
        assert!(!plan.needs_sudo(&a.facts));
        let ids: Vec<&str> = plan.units.iter().map(|u| u.id.as_str()).collect();
        assert!(ids.contains(&"lsd") && !ids.contains(&"nmap"), "{ids:?}");
        assert!(plan.skipped.iter().any(|(id, _)| id == "nmap"));
        // It runs without asking.
        let cmds = a.on_key(key(KeyCode::Enter));
        assert!(matches!(&cmds[..], [Cmd::StartRun { command, .. }] if command == "install"));
    }

    #[test]
    fn user_mode_with_nothing_left_goes_back_to_the_dashboard() {
        let mut a = asking_sudo(&["nmap"]);
        a.on_key(key(KeyCode::Enter));
        a.on_key(key(KeyCode::Tab));
        a.on_key(key(KeyCode::Enter));
        assert_eq!(a.screen, Screen::Dashboard);
        assert!(
            a.toast_text()
                .unwrap()
                .starts_with("Nothing to do in user mode")
        );
    }

    #[test]
    fn select_and_install_opens_plan_then_run() {
        let mut a = app();
        a.on_key(key(KeyCode::Char('n')));
        a.on_key(key(KeyCode::Char('/')));
        chars(&mut a, "codex");
        a.on_key(key(KeyCode::Enter));
        // codex and codex-sandbox-network match; select the tool.
        a.on_key(key(KeyCode::Char(' ')));
        assert!(a.selected.contains("codex"));
        a.on_key(key(KeyCode::Enter));
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
        assert_eq!(a.toast_text(), Some("1 outdated"));
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

        // The update fails; `r` updates again (an install plan would skip the present tool).
        let Some(Cmd::StartRun { plan, .. }) = cmds.into_iter().next() else {
            unreachable!()
        };
        a.start_run(plan, "r1".into(), None);
        a.on_exec(Event::UnitFinished(
            0,
            Outcome::Failed("exit 1".into()),
            Duration::from_secs(1),
        ));
        a.on_exec(Event::Finished(RunRecord::default()));
        // Planned once detection has caught up with the run.
        let cmds = a.on_key(key(KeyCode::Char('r')));
        assert!(matches!(cmds[..], [Cmd::Refresh(_)]));
        assert_eq!(a.screen, Screen::Dashboard);
        let snap = a.snap.clone().unwrap();
        a.on_snapshot(snap, None);
        assert_eq!(a.screen, Screen::Plan);
        let (plan, title) = a.plan.as_ref().unwrap();
        assert_eq!(title, "retry · 1 unfinished");
        assert_eq!(plan.units.len(), 1);
        assert_eq!(plan.units[0].action, crate::plan::Action::Update);
        assert_eq!(plan.units[0].steps[0].cmd.as_deref(), Some("rustup update"));
    }

    #[test]
    fn u_updates_every_outdated_tool_whatever_is_selected() {
        let mut a = app();
        a.selected.insert("bat".into());
        a.on_key(key(KeyCode::Char('u')));
        assert_eq!(a.screen, Screen::Dashboard);
        assert_eq!(a.toast_text(), Some("Nothing is outdated (R checks again)"));
        a.on_latest(BTreeMap::from([(
            "rust-toolchain".to_string(),
            Latest::Version("2.0.0".into()),
        )]));
        a.on_key(key(KeyCode::Char('u')));
        let (plan, title) = a.plan.as_ref().unwrap();
        assert_eq!(title, "update · 1 outdated");
        let ids: Vec<&str> = plan.units.iter().map(|u| u.id.as_str()).collect();
        assert_eq!(ids, vec!["rust-toolchain"]);
    }

    #[test]
    fn enter_installs_missing_and_updates_outdated_selected_items() {
        let mut a = app();
        a.on_latest(BTreeMap::from([(
            "rust-toolchain".to_string(),
            Latest::Version("2.0.0".into()),
        )]));
        a.selected.clear();
        // Outdated, up to date, and missing.
        for id in ["rust-toolchain", "bat", "codex"] {
            a.selected.insert(id.into());
        }
        a.on_key(key(KeyCode::Enter));
        assert_eq!(a.screen, Screen::Plan);
        let (plan, title) = a.plan.as_ref().unwrap();
        assert_eq!(title, "install · 2 items · update · 1");
        let upd = plan
            .units
            .iter()
            .find(|u| u.id == "rust-toolchain")
            .expect("the outdated tool is updated");
        assert_eq!(upd.action, crate::plan::Action::Update);
        assert_eq!(upd.reason, "1.0.0 → 2.0.0");
        assert!(
            plan.units
                .iter()
                .any(|u| u.id == "codex" && u.action == crate::plan::Action::Install)
        );
        assert!(plan.units.iter().all(|u| u.id != "bat"));
        assert!(plan.skipped.iter().any(|(id, _)| id == "bat"));
        let cmds = a.on_key(key(KeyCode::Enter));
        assert!(matches!(&cmds[..], [Cmd::StartRun { .. }]));

        // Only outdated tools selected: an update plan.
        let mut a = app();
        a.on_latest(BTreeMap::from([(
            "rust-toolchain".to_string(),
            Latest::Version("2.0.0".into()),
        )]));
        a.selected = BTreeSet::from(["rust-toolchain".to_string()]);
        a.on_key(key(KeyCode::Enter));
        let (plan, title) = a.plan.as_ref().unwrap();
        assert_eq!(title, "update · 1 item");
        assert_eq!(plan.units[0].action, crate::plan::Action::Update);
    }

    #[test]
    fn run_view_keeps_the_latest_lines() {
        let mut a = app();
        let plan = a.make_plan(&["codex".into()]);
        a.start_run(plan, "r1".into(), None);
        for n in 0..LOG_LINES * 2 {
            a.on_exec(Event::Line(0, n.to_string()));
        }
        let log = &a.run.as_ref().unwrap().logs[0];
        assert!((LOG_LINES..=LOG_LINES + LOG_LINES / 4).contains(&log.len()));
        assert_eq!(log.last().unwrap(), &(LOG_LINES * 2 - 1).to_string());
        // Long lines are kept by bytes.
        let long = "x".repeat(60 << 10);
        for n in 0..200 {
            a.on_exec(Event::Line(0, format!("{n:03}{long}")));
        }
        let run = a.run.as_ref().unwrap();
        let bytes: usize = run.logs[0].iter().map(String::len).sum();
        assert_eq!(bytes, run.log_bytes[0]);
        assert!((LOG_BYTES - (64 << 10)..=LOG_BYTES + LOG_BYTES / 4).contains(&bytes));
        assert!(run.logs[0].last().unwrap().starts_with("199"));
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
            run.failed_units()
                .map(|u| u.id.as_str())
                .collect::<Vec<_>>(),
            vec!["fnm", "nodejs", "npm-prefix", "codex"]
        );
        // Planned once detection has caught up with the run.
        let cmds = a.on_key(key(KeyCode::Char('r')));
        assert!(matches!(cmds[..], [Cmd::Refresh(_)]));
        assert_eq!(a.screen, Screen::Dashboard);
        let snap = a.snap.clone().unwrap();
        a.on_snapshot(snap, None);
        assert_eq!(a.screen, Screen::Plan);
        assert!(a.plan.as_ref().unwrap().1.starts_with("retry"));
    }

    #[test]
    fn ctrl_c_twice_cancels_the_running_steps() {
        let mut a = app();
        let plan = a.make_plan(&["herdr".into()]);
        let (ctl_tx, ctl_rx) = watch::channel(Control::default());
        a.start_run(plan, "r1".into(), Some(ctl_tx));
        a.on_exec(Event::UnitStarted(0));
        let ctrl_c = KeyEvent::new(KeyCode::Char('c'), KeyModifiers::CONTROL);
        a.on_key(ctrl_c);
        let c = *ctl_rx.borrow();
        assert!(c.stop && c.cancel_seq == 0);
        a.on_key(ctrl_c);
        assert_eq!(ctl_rx.borrow().cancel_seq, 1);
    }

    #[test]
    fn the_last_run_is_retried_the_way_it_ran() {
        let dir = tempfile::tempdir().unwrap();
        let mut a = app();
        a.paths = Paths {
            state_dir: dir.path().join("state"),
            config_dir: dir.path().join("config"),
            cache_dir: dir.path().join("cache"),
        };
        // An interrupted revert of a config: `t` reverts it again, not apply it.
        let id = "npm-prefix".to_string();
        let plan = a.make_revert(std::slice::from_ref(&id), Some(&BTreeMap::new()));
        assert_eq!(plan.units.len(), 1, "{:?}", plan.skipped);
        let rec = RunRecord::planned(&plan, "20261029-140500", "config revert");
        rec.save(&a.paths, &a.facts).unwrap();
        a.state.as_mut().unwrap().last_run = Some("20261029-140500".into());
        a.load_last_run();
        a.on_key(key(KeyCode::Char('t')));
        assert_eq!(a.screen, Screen::Plan);
        let (plan, _) = a.plan.as_ref().unwrap();
        assert_eq!(plan.units[0].id, id);
        assert_eq!(plan.units[0].action, crate::plan::Action::Revert);
    }

    #[test]
    fn terminate_cancels_a_running_run_first() {
        let mut a = app();
        assert!(a.terminate(), "nothing running: exit at once");
        let plan = a.make_plan(&["herdr".into()]);
        let (ctl_tx, ctl_rx) = watch::channel(Control::default());
        a.start_run(plan, "r1".into(), Some(ctl_tx));
        a.on_exec(Event::UnitStarted(0));
        assert!(!a.terminate());
        assert!(a.quitting);
        let c = *ctl_rx.borrow();
        assert!(c.stop);
        assert_eq!(c.cancel_seq, 1);
        a.on_exec(Event::Finished(RunRecord::default()));
        assert!(a.terminate());
    }

    #[test]
    fn unfinished_last_run_is_offered_for_retry() {
        let dir = tempfile::tempdir().unwrap();
        let mut a = app();
        a.paths = Paths {
            state_dir: dir.path().join("state"),
            config_dir: dir.path().join("config"),
            cache_dir: dir.path().join("cache"),
        };
        let plan = a.make_plan(&["codex".into()]);
        let mut rec = RunRecord::planned(&plan, "20261029-140500", "install");
        rec.units[0].outcome = "failed".into();
        rec.save(&a.paths, &a.facts).unwrap();
        a.state.as_mut().unwrap().last_run = Some("20261029-140500".into());
        a.load_last_run();
        let toast = a.toast_text().unwrap().to_string();
        assert!(
            toast.contains("interrupted") && toast.contains("t retries"),
            "{toast}"
        );
        a.on_key(key(KeyCode::Char('t')));
        assert_eq!(a.screen, Screen::Plan);
        let (plan, title) = a.plan.as_ref().unwrap();
        assert!(title.starts_with("retry"), "{title}");
        let ids: Vec<&str> = plan.units.iter().map(|u| u.id.as_str()).collect();
        assert_eq!(ids, vec!["fnm", "nodejs", "npm-prefix", "codex"]);
    }

    #[test]
    fn bundle_toggle_saves_state() {
        let mut a = app();
        a.on_key(key(KeyCode::Left));
        let first = a.bundle_ids()[0].clone();
        let cmds = a.on_key(key(KeyCode::Char(' ')));
        assert!(matches!(
            cmds[..],
            [Cmd::SaveBundles { .. }, Cmd::Refresh(_)]
        ));
        assert!(a.subscribed(&first));
    }

    /// First run (no state yet): straight to the dashboard, nothing kept or selected, the
    /// cursor on Bundles. The state exists in memory with the catalog seen, and is saved by the
    /// first bundle kept.
    #[test]
    fn first_run_opens_the_dashboard_on_bundles() {
        let mut a = app();
        let mut snap = a.snap.clone().unwrap();
        snap.first_run = true;
        for i in &mut snap.items {
            i.desired = false;
        }
        a.screen = Screen::Loading;
        a.state = None;
        a.reseed = true;
        a.on_snapshot(snap.clone(), None);
        assert_eq!(a.screen, Screen::Dashboard);
        assert_eq!(a.focus, Focus::Bundles);
        assert!(a.selected.is_empty(), "{:?}", a.selected);
        let st = a.state.as_ref().unwrap();
        assert!(st.bundles.is_empty());
        assert!(st.seen.contains("codex"));
        // A refresh before anything was saved keeps it.
        a.on_snapshot(snap, None);
        assert!(a.state.as_ref().unwrap().seen.contains("codex"));
        // Keeping a bundle saves it.
        a.bundle_cursor = a
            .bundle_ids()
            .iter()
            .position(|b| b == "ai-agents")
            .unwrap();
        let cmds = a.on_key(key(KeyCode::Char(' ')));
        assert!(matches!(
            cmds[..],
            [Cmd::SaveBundles { .. }, Cmd::Refresh(_)]
        ));
        assert!(a.selected.contains("codex"));
    }

    // Dashboard: Bundles on the left, Items on the right; the selection is what Enter installs.

    fn ids(set: &BTreeSet<String>) -> Vec<&str> {
        set.iter().map(String::as_str).collect()
    }

    #[test]
    fn dashboard_starts_with_what_the_kept_bundles_miss() {
        let a = app();
        assert_eq!(ids(&a.selected), vec!["codex", "fnm"]);
        assert_eq!(a.focus, Focus::Items);
    }

    #[test]
    fn arrows_switch_between_bundles_and_items() {
        let mut a = app();
        a.on_key(key(KeyCode::Left));
        assert_eq!(a.focus, Focus::Bundles);
        a.on_key(key(KeyCode::Right));
        assert_eq!(a.focus, Focus::Items);
        a.on_key(key(KeyCode::Char('h')));
        assert_eq!(a.focus, Focus::Bundles);
        a.on_key(key(KeyCode::Char('l')));
        assert_eq!(a.focus, Focus::Items);
        // Panel numbers no longer jump.
        a.on_key(key(KeyCode::Char('2')));
        assert_eq!(a.focus, Focus::Items);
    }

    #[test]
    fn a_bundle_selects_its_missing_items_and_is_kept() {
        let mut a = app();
        a.selected.clear();
        a.on_key(key(KeyCode::Left));
        a.bundle_cursor = a
            .bundle_ids()
            .iter()
            .position(|b| b == "ai-agents")
            .unwrap();
        let cmds = a.on_key(key(KeyCode::Char(' ')));
        assert!(matches!(
            cmds[..],
            [Cmd::SaveBundles { .. }, Cmd::Refresh(_)]
        ));
        assert!(
            a.state
                .as_ref()
                .unwrap()
                .bundles
                .contains(&"ai-agents".to_string())
        );
        let missing: Vec<String> = a
            .catalog
            .bundle_items("ai-agents")
            .into_iter()
            .filter(|i| {
                a.snap
                    .as_ref()
                    .unwrap()
                    .get(i)
                    .is_some_and(|s| !s.present && s.label != Label::NotApplicable)
            })
            .collect();
        assert!(!missing.is_empty());
        assert!(
            missing.iter().all(|i| a.selected.contains(i)),
            "{:?}",
            a.selected
        );

        // The refresh that follows keeps manual changes.
        a.selected.remove("codex");
        let snap = a.snap.clone().unwrap();
        let state = a.state.clone();
        a.on_snapshot(snap, state);
        assert!(!a.selected.contains("codex"));

        // Dropping the bundle drops its items, except those another kept bundle has.
        a.on_key(key(KeyCode::Char(' ')));
        assert!(
            !a.state
                .as_ref()
                .unwrap()
                .bundles
                .contains(&"ai-agents".to_string())
        );
        let terminal = a.catalog.bundle_items("terminal");
        assert!(
            a.selected.iter().all(|i| terminal.contains(i)),
            "{:?}",
            a.selected
        );
    }

    #[test]
    fn space_in_items_adds_and_removes() {
        let mut a = app();
        a.on_key(key(KeyCode::Char('/')));
        chars(&mut a, "codex");
        a.on_key(key(KeyCode::Enter));
        assert_eq!(a.current().unwrap().id, "codex");
        a.on_key(key(KeyCode::Char(' ')));
        assert!(!a.selected.contains("codex"));
        a.cursor = 0;
        a.on_key(key(KeyCode::Char(' ')));
        assert!(a.selected.contains("codex"));
    }

    #[test]
    fn enter_installs_the_selection() {
        let mut a = app();
        a.on_key(key(KeyCode::Enter));
        assert_eq!(a.screen, Screen::Plan);
        let (plan, title) = a.plan.as_ref().unwrap();
        assert!(title.starts_with("install · "), "{title}");
        let units: Vec<&str> = plan.units.iter().map(|u| u.id.as_str()).collect();
        assert!(
            units.contains(&"codex") && units.contains(&"fnm"),
            "{units:?}"
        );
        let cmds = a.on_key(key(KeyCode::Enter));
        assert!(matches!(&cmds[..], [Cmd::StartRun { command, .. }] if command == "install"));
        // From Bundles too.
        let mut a = app();
        a.on_key(key(KeyCode::Left));
        a.on_key(key(KeyCode::Enter));
        assert_eq!(a.screen, Screen::Plan);
    }

    #[test]
    fn shift_a_adopts_the_selection_else_the_cursor_tool() {
        let mut a = app();
        a.selected.clear();
        let adoptable: Vec<String> = a
            .rows()
            .iter()
            .filter(|i| i.adoption().is_some())
            .map(|i| i.id.clone())
            .collect();
        assert!(adoptable.len() >= 3, "{adoptable:?}");
        a.cursor = a.rows().iter().position(|i| i.id == adoptable[0]).unwrap();
        let cmds = a.on_key(key(KeyCode::Char('A')));
        assert!(matches!(cmds[..], [Cmd::Adopt(_), Cmd::Refresh(_)]));
        let rec = &a.state.as_ref().unwrap().tools[&adoptable[0]];
        assert_eq!(rec.method.as_deref(), Some("script"));
        assert!(
            rec.installed_at.is_some(),
            "a record with installed_at is managed"
        );
        assert!(a.toast_text().unwrap().contains("is now managed"));

        // The selection wins over the cursor.
        a.selected = [adoptable[1].clone(), adoptable[2].clone()].into();
        a.on_key(key(KeyCode::Char('A')));
        let st = a.state.as_ref().unwrap();
        assert!(st.is_managed(&adoptable[1]) && st.is_managed(&adoptable[2]));
        assert!(a.toast_text().unwrap().contains("2 tools are now managed"));

        // Nothing adoptable under the cursor: it says why and changes nothing.
        a.selected.clear();
        a.cursor = a.rows().iter().position(|i| !i.present).unwrap();
        assert!(a.on_key(key(KeyCode::Char('A'))).is_empty());
        assert!(a.toast_text().unwrap().contains("is not installed"));
    }

    #[test]
    fn shift_s_switches_where_new_installs_go() {
        let mut a = app();
        assert!(a.prefer_system);
        let cmds = a.on_key(key(KeyCode::Char('S')));
        assert!(!a.prefer_system);
        assert!(matches!(cmds[..], [Cmd::Refresh(Fetch::CacheOnly)]));
        assert!(a.toast_text().unwrap().contains("user scope"));
        // Plans follow the choice.
        a.on_key(key(KeyCode::Enter));
        let (plan, _) = a.plan.as_ref().unwrap();
        assert!(
            plan.units.iter().all(|u| u.scope == Scope::User),
            "{plan:?}"
        );
        a.on_key(key(KeyCode::Esc));
        a.on_key(key(KeyCode::Char('S')));
        assert!(a.prefer_system);

        let mut a = app();
        let mut f = (*a.facts).clone();
        f.privilege = crate::facts::Privilege::None;
        a.facts = Arc::new(f);
        a.prefer_system = false;
        assert!(a.on_key(key(KeyCode::Char('S'))).is_empty());
        assert!(!a.prefer_system);
        assert!(a.toast_text().unwrap().contains("No sudo"));
    }

    #[test]
    fn enter_with_nothing_selected_explains() {
        let mut a = app();
        a.on_key(key(KeyCode::Char('n')));
        assert!(a.selected.is_empty());
        a.on_key(key(KeyCode::Enter));
        assert_eq!(a.screen, Screen::Dashboard);
        assert!(
            a.toast_text().unwrap().contains("space"),
            "{:?}",
            a.toast_text()
        );
    }

    #[test]
    fn esc_clears_the_search_but_keeps_the_selection() {
        let mut a = app();
        a.on_key(key(KeyCode::Char('/')));
        chars(&mut a, "co");
        a.on_key(key(KeyCode::Enter));
        a.on_key(key(KeyCode::Esc));
        assert!(a.search.is_empty());
        a.on_key(key(KeyCode::Esc));
        assert_eq!(ids(&a.selected), vec!["codex", "fnm"]);
    }

    #[test]
    fn the_selection_is_seeded_again_after_a_run() {
        let mut a = app();
        a.on_key(key(KeyCode::Enter));
        let plan = a.plan.clone().unwrap().0;
        a.on_key(key(KeyCode::Enter));
        assert!(a.selected.is_empty());
        a.start_run(plan, "20261031-070000".into(), None);
        a.on_exec(Event::Finished(RunRecord::default()));
        a.on_key(key(KeyCode::Enter));
        assert_eq!(a.screen, Screen::Dashboard);
        let snap = a.snap.clone().unwrap();
        let state = a.state.clone();
        a.on_snapshot(snap, state);
        assert_eq!(ids(&a.selected), vec!["codex", "fnm"]);
    }

    #[test]
    fn tab_cycles_the_detail_tabs() {
        let mut a = app();
        assert_eq!(a.detail, DetailTab::Info);
        a.on_key(key(KeyCode::Tab));
        assert_eq!(a.detail, DetailTab::Commands);
        a.on_key(key(KeyCode::Tab));
        assert_eq!(a.detail, DetailTab::Log);
        a.on_key(key(KeyCode::BackTab));
        assert_eq!(a.detail, DetailTab::Commands);
    }
}
