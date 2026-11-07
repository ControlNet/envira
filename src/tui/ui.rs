//! Rendering (BC1: lazygit-style numbered panels with btop-style gauges).

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::Frame;
use ratatui::layout::{Alignment, Constraint, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, BorderType, Cell, Clear, Paragraph, Row, Table, Wrap};

use super::app::{App, DetailTab, Filter, Focus, Hit, Panel, Screen, UnitState, bar_key};
use super::theme::Theme;
use crate::commands::fmt_duration;
use crate::exec::Outcome;
use crate::plan::{Action, Kind, Planner};
use crate::state::Scope;
use crate::status::{ItemStatus, Label, Ownership};

const SPINNER: [&str; 10] = ["⠋", "⠙", "⠹", "⠸", "⠼", "⠴", "⠦", "⠧", "⠇", "⠏"];

pub fn draw(f: &mut Frame, app: &App, t: &Theme) {
    app.hits.borrow_mut().clear();
    let area = f.area();
    match app.screen {
        Screen::Loading => loading(f, area, app, t),
        Screen::Dashboard => dashboard(f, area, app, t),
        Screen::Plan => plan_view(f, area, app, t),
        Screen::Run => run_view(f, area, app, t),
    }
    if let Some(p) = &app.sudo {
        sudo_dialog(f, area, app, p, t);
    }
    if app.help {
        help(f, area, app, t);
    }
}

/// The password sudo wants for the plan on screen, or user mode instead.
fn sudo_dialog(f: &mut Frame, area: Rect, app: &App, p: &crate::sudo_prompt::Prompt, t: &Theme) {
    use crate::sudo_prompt::Focus;
    let title = p.title();
    let w = ((title.chars().count() + 4).max(44) as u16).min(area.width);
    let h = 7.min(area.height);
    let r = Rect {
        x: area.x + (area.width - w) / 2,
        y: area.y + (area.height - h) / 2,
        width: w,
        height: h,
    };
    f.render_widget(Clear, r);
    let block = Block::bordered()
        .border_type(BorderType::Rounded)
        .border_style(t.s(t.orange))
        .title(Span::styled(
            " sudo ",
            Style::new().fg(t.orange).add_modifier(Modifier::BOLD),
        ));
    let inner = block.inner(r);
    f.render_widget(block, r);
    let typing = p.focus == Focus::Password && !p.checking;
    let mut field = vec![Span::styled(format!(" {}", p.masked()), t.s(t.fg))];
    if typing {
        field.push(Span::styled("▏", t.s(t.focus)));
    }
    let status = if p.checking {
        Span::styled(" checking…", t.s(t.dim))
    } else if let Some(e) = &p.error {
        Span::styled(format!(" {e}"), t.s(t.red))
    } else {
        Span::raw("")
    };
    let button = |f: Focus, label: &str| {
        let style = if p.focus == f {
            Style::new()
                .fg(t.fg)
                .add_modifier(Modifier::REVERSED | Modifier::BOLD)
        } else {
            t.s(t.dim)
        };
        Span::styled(format!(" {label} "), style)
    };
    let mut buttons = vec![];
    if p.user_mode {
        buttons.extend([button(Focus::UserMode, "Use user mode"), Span::raw("   ")]);
    }
    buttons.push(button(Focus::Back, p.back_label));
    let bw: u16 = buttons.iter().map(|s| s.width() as u16).sum();
    let lines = vec![
        Line::styled(format!(" {title}"), t.s(t.fg)),
        Line::from(field),
        Line::from(status),
        Line::raw(""),
    ];
    f.render_widget(Paragraph::new(lines), inner);
    if inner.height > 1 {
        app.hit(
            Rect::new(inner.x, inner.y + 1, inner.width, 1),
            Hit::Sudo(Focus::Password),
        );
    }
    if inner.height >= 5 {
        let row = Rect::new(
            inner.x + inner.width.saturating_sub(bw) / 2,
            inner.y + 4,
            bw.min(inner.width),
            1,
        );
        hit_spans(
            app,
            row,
            &buttons,
            if p.user_mode {
                vec![(0, Hit::Sudo(Focus::UserMode)), (2, Hit::Sudo(Focus::Back))]
            } else {
                vec![(0, Hit::Sudo(Focus::Back))]
            },
        );
        f.render_widget(Paragraph::new(Line::from(buttons)), row);
    }
}

// ------------------------------------------------------------------------------------------------
// Building blocks
// ------------------------------------------------------------------------------------------------

fn panel<'a>(t: &Theme, n: usize, title: &str, focused: bool, extra: Vec<Span<'a>>) -> Block<'a> {
    pane(t, &format!("[{n}] {title}"), focused, extra)
}

/// A rounded panel with a plain title (the dashboard's panels are not numbered).
fn pane<'a>(t: &Theme, title: &str, focused: bool, extra: Vec<Span<'a>>) -> Block<'a> {
    let (border, title_style) = if focused {
        (
            t.s(t.focus),
            Style::new().fg(t.focus).add_modifier(Modifier::BOLD),
        )
    } else {
        (t.s(t.border), t.s(t.dim))
    };
    let mut spans = vec![Span::styled(title.to_string(), title_style)];
    if !extra.is_empty() {
        spans.push(Span::raw(" "));
        spans.extend(extra);
    }
    Block::bordered()
        .border_type(BorderType::Rounded)
        .border_style(border)
        .title(Line::from(spans))
}

/// Line gauge of `━` with colored segments over `total`.
fn bar<'a>(t: &Theme, width: u16, segments: &[(usize, Color)], total: usize) -> Line<'a> {
    let w = width as usize;
    let mut spans = Vec::new();
    let mut used = 0;
    for (v, c) in segments {
        let n = if total == 0 {
            0
        } else {
            ((*v as f64 / total as f64) * w as f64).round() as usize
        };
        let n = n.min(w - used.min(w));
        if n > 0 {
            spans.push(Span::styled("━".repeat(n), t.s(*c)));
            used += n;
        }
    }
    if used < w {
        spans.push(Span::styled("━".repeat(w - used), t.s(t.border)));
    }
    Line::from(spans)
}

fn label_color(t: &Theme, l: Label) -> Color {
    match l {
        Label::Installed | Label::Applied => t.green,
        Label::Failed => t.red,
        Label::Missing => t.yellow,
        Label::New => t.cyan,
        Label::Drifted => t.yellow,
        Label::Outdated => t.blue,
        Label::Available | Label::NotApplicable => t.dim,
    }
}

/// Key hints; clicking one presses its key (see `bar_key`), and `right_key` is what clicking the
/// right-hand text presses.
fn keybar(
    f: &mut Frame,
    area: Rect,
    app: &App,
    t: &Theme,
    keys: &[(&str, &str)],
    right: Option<Span>,
    right_key: Option<KeyEvent>,
) {
    // A right side wider than the bar would not be drawn at all: cut it to fit instead.
    let right = right
        .map(|s| {
            let max = area.width as usize;
            match s.width() > max {
                true => Span::styled(truncate(&s.content, max), s.style),
                false => s,
            }
        })
        .map(Line::from)
        .unwrap_or_default();
    let rw = right.width() as u16;
    // Whole key hints only, with a gap before the right side.
    let room = (area.width as usize).saturating_sub(if rw > 0 { rw as usize + 2 } else { 0 });
    // When they do not all fit, the least needed hints go first; the main action, Help, Quit and
    // Back stay.
    let width = |n: &str, k: &str| Span::raw(format!("{n}: {k}")).width();
    let mut keep: Vec<bool> = vec![true; keys.len()];
    let total = |keep: &[bool]| -> usize {
        let shown: Vec<usize> = keys
            .iter()
            .zip(keep)
            .filter(|(_, k)| **k)
            .map(|((n, k), _)| width(n, k))
            .collect();
        shown.iter().sum::<usize>() + 3 * shown.len().saturating_sub(1)
    };
    const DROP_FIRST: [&str; 10] = [
        "Search",
        "Filter",
        "Update all",
        "Scroll",
        "Zoom log",
        "Follow",
        "Panels",
        "Select",
        "Step",
        "Stop after running",
    ];
    let order: Vec<usize> = DROP_FIRST
        .iter()
        .filter_map(|n| keys.iter().position(|(k, _)| k == n))
        .chain((0..keys.len()).rev())
        .filter(|&i| {
            !matches!(
                keys[i].0,
                "Help" | "Quit" | "Back" | "Install/Update" | "Cancel running" | "Dashboard"
            )
        })
        .collect();
    for i in order {
        if total(&keep) <= room {
            break;
        }
        keep[i] = false;
    }
    let mut spans = Vec::new();
    let mut marks = Vec::new();
    let mut used = 0;
    for (name, key) in keys.iter().zip(&keep).filter(|(_, k)| **k).map(|(x, _)| x) {
        let hint = format!("{name}: {key}");
        let w = Span::raw(hint.as_str()).width() + if used > 0 { 3 } else { 0 };
        if used + w > room {
            break;
        }
        if used > 0 {
            spans.push(Span::styled(" | ", t.s(t.dim)));
        }
        used += w;
        if let Some(k) = bar_key(name, key) {
            marks.push((spans.len(), Hit::Key(k)));
        }
        spans.push(Span::styled(hint, t.s(t.cyan)));
    }
    let [l, r] = Layout::horizontal([Constraint::Min(0), Constraint::Length(rw)]).areas(area);
    hit_spans(app, l, &spans, marks);
    if let Some(k) = right_key {
        app.hit(r, Hit::Key(k));
    }
    f.render_widget(Paragraph::new(Line::from(spans)), l);
    f.render_widget(Paragraph::new(right).alignment(Alignment::Right), r);
}

/// Returns the column where `what` starts.
fn header(f: &mut Frame, area: Rect, app: &App, t: &Theme, what: Vec<Span>) -> u16 {
    let mut left = vec![
        Span::styled(
            "envira",
            Style::new().fg(t.blue).add_modifier(Modifier::BOLD),
        ),
        Span::styled(format!(" {}", env!("CARGO_PKG_VERSION")), t.s(t.dim)),
    ];
    if let Some(v) = &app.self_latest {
        left.push(Span::styled(
            format!(" → {v}"),
            Style::new().fg(t.yellow).add_modifier(Modifier::BOLD),
        ));
    }
    left.push(Span::styled(
        format!(" · {} · ", app.facts.hostname),
        t.s(t.dim),
    ));
    let what_x = area.x + Line::from(left.clone()).width() as u16;
    left.extend(what);
    // Where the catalog came from is in `envira doctor`; load warnings show as a toast.
    let right = if app.detecting {
        format!("{} detecting…", SPINNER[(app.tick % 10) as usize])
    } else {
        String::new()
    };
    let [l, r] = Layout::horizontal([
        Constraint::Min(0),
        Constraint::Length(right.chars().count() as u16 + 1),
    ])
    .areas(area);
    f.render_widget(Paragraph::new(Line::from(left)), l);
    f.render_widget(
        Paragraph::new(Span::styled(right, t.s(t.dim))).alignment(Alignment::Right),
        r,
    );
    what_x
}

/// Records `marks` (span index, hit) for spans laid out in one line from the left of `area`.
fn hit_spans(app: &App, area: Rect, spans: &[Span], marks: Vec<(usize, Hit)>) {
    let mut x = area.x;
    let mut starts = Vec::with_capacity(spans.len());
    for s in spans {
        starts.push(x);
        x = x.saturating_add(s.width() as u16);
    }
    for (i, h) in marks {
        let (Some(&sx), Some(s)) = (starts.get(i), spans.get(i)) else {
            continue;
        };
        if sx < area.right() {
            let w = (s.width() as u16).min(area.right() - sx);
            app.hit(Rect::new(sx, area.y, w, 1), h);
        }
    }
}

/// Records hits for spans of a panel title (`pane` puts `title` and a space before `extra`).
fn hit_title(app: &App, area: Rect, title: &str, extra: &[Span], marks: Vec<(usize, Hit)>) {
    let x = area.x + 1 + Span::raw(format!("{title} ")).width() as u16;
    let row = Rect::new(x, area.y, area.right().saturating_sub(x + 1), 1);
    hit_spans(app, row, extra, marks);
}

fn toast_span<'a>(app: &'a App, t: &Theme) -> Option<Span<'a>> {
    app.toast_text()
        .map(|m| Span::styled(m, Style::new().fg(t.yellow)))
}

fn columns(area: Rect, n: u16) -> Vec<Rect> {
    Layout::horizontal((0..n).map(|_| Constraint::Ratio(1, n as u32)))
        .spacing(2)
        .split(area)
        .to_vec()
}

fn stat<'a>(t: &Theme, value: usize, color: Color, label: String) -> Line<'a> {
    Line::from(vec![
        Span::styled(
            value.to_string(),
            Style::new().fg(color).add_modifier(Modifier::BOLD),
        ),
        Span::styled(format!(" {label}"), t.s(t.dim)),
    ])
}

fn loading(f: &mut Frame, area: Rect, app: &App, t: &Theme) {
    let [h, body] = Layout::vertical([Constraint::Length(1), Constraint::Min(0)]).areas(area);
    header(f, h, app, t, vec![Span::raw(app.facts.summary())]);
    let msg = format!(
        "{} detecting installed tools…",
        SPINNER[(app.tick % 10) as usize]
    );
    let [_, mid, _] = Layout::vertical([
        Constraint::Min(0),
        Constraint::Length(1),
        Constraint::Min(0),
    ])
    .areas(body);
    f.render_widget(
        Paragraph::new(Span::styled(msg, t.s(t.dim))).alignment(Alignment::Center),
        mid,
    );
}

// ------------------------------------------------------------------------------------------------
// Dashboard
// ------------------------------------------------------------------------------------------------

fn dashboard(f: &mut Frame, area: Rect, app: &App, t: &Theme) {
    let [h, ov, main, keys] = Layout::vertical([
        Constraint::Length(1),
        Constraint::Length(4),
        Constraint::Min(8),
        Constraint::Length(1),
    ])
    .areas(area);
    // On a narrow terminal the machine facts go first: the scope and its key matter more.
    let lead = format!(
        "envira {} · {} · ",
        env!("CARGO_PKG_VERSION"),
        app.facts.hostname
    );
    let scope_w = " · new installs → system (S → user)".len() + 14;
    let facts = app.facts.summary();
    let roomy = lead.chars().count() + facts.chars().count() + scope_w <= h.width as usize;
    let mut what = if roomy {
        vec![
            Span::raw(facts),
            Span::styled(" · new installs → ", t.s(t.dim)),
        ]
    } else {
        vec![Span::styled("new installs → ", t.s(t.dim))]
    };
    if app.prefer_system {
        what.push(Span::styled("system", t.s(t.orange)));
    } else {
        what.push(Span::raw("user"));
    }
    let hint = app.facts.privilege.can_elevate().then_some(what.len());
    if hint.is_some() {
        let other = if app.prefer_system { "user" } else { "system" };
        what.push(Span::styled(format!(" (S → {other})"), t.s(t.dim)));
    }
    let x = header(f, h, app, t, what.clone());
    if let Some(i) = hint {
        let row = Rect::new(x, h.y, h.right().saturating_sub(x), 1);
        let s = KeyEvent::new(KeyCode::Char('S'), KeyModifiers::NONE);
        hit_spans(app, row, &what, vec![(i, Hit::Key(s))]);
    }
    overview(f, ov, app, t);
    let left_w = if area.width >= 110 { 36 } else { 30 };
    let [left, right] =
        Layout::horizontal([Constraint::Length(left_w), Constraint::Min(30)]).areas(main);
    let nb = app.catalog.bundles.len() as u16;
    let [bl, sel] =
        Layout::vertical([Constraint::Min(nb.min(4) + 2), Constraint::Length(7)]).areas(left);
    bundles(f, bl, app, t);
    selection(f, sel, app, t);
    let [tbl, det] = Layout::vertical([Constraint::Min(5), Constraint::Length(8)]).areas(right);
    table(f, tbl, app, t);
    detail(f, det, app, t);
    let right_span = toast_span(app, t).or_else(|| {
        (!app.selected.is_empty()).then(|| {
            Span::styled(
                format!("{} selected", app.selected.len()),
                Style::new().fg(t.magenta).add_modifier(Modifier::BOLD),
            )
        })
    });
    let keyset: Vec<(&str, &str)> = if app.searching {
        vec![("Filter", "type"), ("Keep", "enter"), ("Clear", "esc")]
    } else {
        let mut k = vec![
            ("Panels", "← →"),
            ("Select", "space"),
            ("Install/Update", "enter"),
            ("Update all", "u"),
        ];
        if app.last_run.is_some() {
            k.push(("Retry", "t"));
        }
        k.extend([
            ("Search", "/"),
            ("Filter", "f"),
            ("Help", "?"),
            ("Quit", "q"),
        ]);
        k
    };
    keybar(f, keys, app, t, &keyset, right_span, None);
}

fn overview(f: &mut Frame, area: Rect, app: &App, t: &Theme) {
    let block = pane(t, "Overview", false, vec![]);
    let inner = block.inner(area);
    f.render_widget(block, area);
    let Some(snap) = &app.snap else { return };
    let applicable = snap.count(|i| i.present || i.label != Label::NotApplicable);
    let present = snap.count(|i| i.present);
    let to_install = snap.to_install().len();
    let new = snap.count(|i| i.label == Label::New);
    let failed: Vec<&str> = snap
        .items
        .iter()
        .filter(|i| matches!(i.label, Label::Failed | Label::Drifted))
        .map(|i| i.id.as_str())
        .collect();
    let outdated = snap.count(|i| i.label == Label::Outdated);
    let updatable = snap.count(|i| i.route.is_some());
    let cols = columns(inner, 4);
    let system = snap.count(|i| i.present && i.scope == Some(Scope::System));
    let user = snap.count(|i| i.present && i.scope == Some(Scope::User));
    let fit = |s: &String| s.chars().count() + present.to_string().len() < cols[0].width as usize;
    let installed = [
        format!("/ {applicable} installed · {system} system {user} user"),
        format!("/ {applicable} installed · {system} system"),
        format!("/ {applicable} installed"),
    ]
    .into_iter()
    .find(fit)
    .unwrap_or_else(|| format!("/ {applicable}"));
    let cells: [(Line, Line); 4] = [
        (
            stat(t, present, t.green, installed),
            bar(t, cols[0].width, &[(present, t.green)], applicable),
        ),
        (
            stat(
                t,
                to_install,
                t.cyan,
                if new > 0 {
                    format!("to install · {new} new in catalog")
                } else {
                    "to install".into()
                },
            ),
            bar(t, cols[1].width, &[(to_install, t.cyan)], applicable),
        ),
        (
            stat(
                t,
                failed.len(),
                t.red,
                match &app.last_run {
                    Some((_, t, _)) => format!("failed · last run left {} · t", t.len()),
                    None if failed.is_empty() => "failed".into(),
                    None => format!("failed · {}", failed.join(" ")),
                },
            ),
            bar(
                t,
                cols[2].width,
                &[(failed.len(), t.red)],
                applicable.max(1),
            ),
        ),
        (
            stat(
                t,
                outdated,
                t.blue,
                if app.checking_latest {
                    format!("outdated · checking{}", ".".repeat(app.tick as usize % 4))
                } else {
                    "outdated".into()
                },
            ),
            bar(t, cols[3].width, &[(outdated, t.blue)], updatable.max(1)),
        ),
    ];
    let counters = [
        Filter::Installed,
        Filter::Attention,
        Filter::Failed,
        Filter::Outdated,
    ];
    for ((c, (a, b)), fl) in cols.iter().zip(cells).zip(counters) {
        app.hit(*c, Hit::Counter(fl));
        f.render_widget(Paragraph::new(vec![a, b]), *c);
    }
}

fn bundle_counts(app: &App, b: &str) -> (usize, usize) {
    let Some(snap) = &app.snap else { return (0, 0) };
    let items = app.catalog.bundle_items(b);
    let relevant: Vec<&ItemStatus> = items
        .iter()
        .filter_map(|i| snap.get(i))
        .filter(|s| s.present || s.label != Label::NotApplicable)
        .collect();
    (
        relevant.iter().filter(|s| s.present).count(),
        relevant.len(),
    )
}

fn bundles(f: &mut Frame, area: Rect, app: &App, t: &Theme) {
    let block = pane(
        t,
        "Bundles",
        app.focus == Focus::Bundles,
        vec![Span::styled("space: follow", t.s(t.dim))],
    );
    let inner = block.inner(area);
    f.render_widget(block, area);
    let name_w = 13usize;
    let count_w = 8u16;
    let bar_w = inner
        .width
        .saturating_sub(2 + name_w as u16 + count_w + 1)
        .min(16);
    let lines: Vec<Line> = app
        .bundle_ids()
        .iter()
        .enumerate()
        .map(|(i, b)| {
            let on = app.subscribed(b);
            let (have, total) = bundle_counts(app, b);
            let color = if total > 0 && have == total {
                t.green
            } else if on {
                t.blue
            } else {
                t.dim
            };
            let mut spans = vec![
                Span::styled(
                    if on { "● " } else { "○ " },
                    t.s(if on { t.green } else { t.dim }),
                ),
                Span::styled(
                    format!(
                        "{:<name_w$}",
                        truncate(app.catalog.bundle_name(b), name_w - 1)
                    ),
                    t.s(if on { t.fg } else { t.dim }),
                ),
            ];
            spans.extend(bar(t, bar_w, &[(have, color)], total).spans);
            spans.push(Span::styled(format!(" {have:>3}/{total:<3}"), t.s(t.dim)));
            let line = Line::from(spans);
            if app.focus == Focus::Bundles && i == app.bundle_cursor {
                line.style(t.cursor())
            } else {
                line
            }
        })
        .collect();
    let skip = app
        .bundles_view
        .top(app.bundle_cursor, lines.len(), inner.height as usize);
    app.hit(inner, Hit::Panel(Panel::Bundles));
    for (row, i) in (skip..lines.len()).take(inner.height as usize).enumerate() {
        let y = inner.y + row as u16;
        app.hit(Rect::new(inner.x, y, inner.width, 1), Hit::BundleRow(i));
        app.hit(
            Rect::new(inner.x, y, 2.min(inner.width), 1),
            Hit::BundleMark(i),
        );
    }
    f.render_widget(Paragraph::new(lines).scroll((skip as u16, 0)), inner);
}

fn selection(f: &mut Frame, area: Rect, app: &App, t: &Theme) {
    let targets: Vec<String> = app
        .catalog
        .item_ids()
        .into_iter()
        .filter(|id| app.selected.contains(id))
        .collect();
    let plan = (!targets.is_empty()).then(|| app.make_selection_plan(&targets));
    let extra = match &plan {
        Some(p) => {
            let deps = p
                .units
                .iter()
                .filter(|u| !app.selected.contains(&u.id))
                .count();
            vec![
                Span::styled(app.selected.len().to_string(), t.s(t.magenta)),
                Span::styled(
                    format!(
                        " · {} {}{}",
                        p.units.len(),
                        plural(p.units.len(), "step", "steps"),
                        if deps > 0 {
                            format!(" (+{deps} deps)")
                        } else {
                            String::new()
                        }
                    ),
                    t.s(t.dim),
                ),
            ]
        }
        None => vec![],
    };
    let block = pane(t, "Selected", false, extra);
    let inner = block.inner(area);
    f.render_widget(block, area);
    let Some(plan) = plan else {
        let lines = vec![
            Line::styled("Nothing selected.", t.s(t.dim)),
            Line::styled("space: select bundles, items", t.s(t.dim)),
            Line::styled("enter: install / update them", t.s(t.dim)),
        ];
        f.render_widget(Paragraph::new(lines), inner);
        return;
    };
    // (glyph, its color, id, id color) per entry; wrapped by hand so each name has a known place.
    let mut entries: Vec<(&str, Color, &str, Color)> = Vec::new();
    for u in &plan.units {
        let (g, c) = match u.action {
            Action::Install => ("+", t.cyan),
            Action::Update => ("↑", t.blue),
            Action::Apply => ("◇", t.magenta),
            Action::Revert => ("↺", t.yellow),
        };
        let ic = if u.scope == Scope::System {
            t.orange
        } else if app.selected.contains(&u.id) {
            t.fg
        } else {
            t.dim
        };
        entries.push((g, c, &u.id, ic));
    }
    for (id, _) in plan.skipped.iter().filter(|(_, w)| w != "already present") {
        entries.push(("–", t.dim, id, t.dim));
    }
    let mut lines: Vec<Line> = vec![Line::default()];
    let (mut x, mut y) = (0u16, 0u16);
    for (g, c, id, ic) in entries {
        let w = Span::raw(format!("{g} {id}")).width() as u16;
        if x > 0 && x + w > inner.width {
            lines.push(Line::default());
            (x, y) = (0, y + 1);
        }
        if y < inner.height {
            let gw = Span::raw(format!("{g} ")).width() as u16;
            let iw = w.saturating_sub(gw).min(inner.width.saturating_sub(x + gw));
            app.hit(
                Rect::new(inner.x + x + gw, inner.y + y, iw, 1),
                Hit::Unit(id.to_string()),
            );
        }
        let line = lines.last_mut().expect("one line");
        line.push_span(Span::styled(format!("{g} "), t.s(c)));
        line.push_span(Span::styled(id.to_string(), t.s(ic)));
        line.push_span(Span::raw("  "));
        x += w + 2;
    }
    f.render_widget(Paragraph::new(lines), inner);
}

fn table(f: &mut Frame, area: Rect, app: &App, t: &Theme) {
    let mut tabs: Vec<Span> = Vec::new();
    let mut marks = Vec::new();
    for (i, fl) in Filter::ALL.iter().enumerate() {
        if i > 0 {
            tabs.push(Span::styled(" - ", t.s(t.dim)));
        }
        let st = if *fl == app.filter {
            Style::new().fg(t.focus).add_modifier(Modifier::BOLD)
        } else {
            t.s(t.dim)
        };
        marks.push((tabs.len(), Hit::Filter(*fl)));
        tabs.push(Span::styled(fl.name(), st));
    }
    hit_title(app, area, "Items", &tabs, marks);
    if !app.search.is_empty() || app.searching {
        tabs.push(Span::styled(format!("  /{}", app.search), t.s(t.yellow)));
        if app.searching {
            tabs.push(Span::styled("▏", t.s(t.yellow)));
        }
    }
    let block = pane(t, "Items", app.focus == Focus::Items, tabs);
    let inner = block.inner(area);
    f.render_widget(block, area);
    let rows = app.rows();
    let height = inner.height.saturating_sub(1) as usize;
    let offset = app.items_view.top(app.cursor, rows.len(), height);
    app.hit(inner, Hit::Panel(Panel::Items));
    for (row, i) in (offset..rows.len()).take(height).enumerate() {
        let y = inner.y + 1 + row as u16;
        app.hit(Rect::new(inner.x, y, inner.width, 1), Hit::ItemRow(i));
        app.hit(
            Rect::new(inner.x, y, 3.min(inner.width), 1),
            Hit::ItemMark(i),
        );
    }
    let name_w = rows
        .iter()
        .map(|i| i.id.chars().count())
        .max()
        .unwrap_or(0)
        .clamp(12, 22) as u16;
    let cols = item_columns(inner.width, name_w);
    // What the last column gets: the rest, after the others and a space between each pair.
    let note_w = cols
        .iter()
        .filter_map(|(_, c)| match c {
            Constraint::Length(w) => Some(*w as usize + 1),
            _ => None,
        })
        .fold(inner.width as usize, |rest, w| rest.saturating_sub(w))
        .max(1);
    let pick = |cells: Vec<Cell<'static>>| -> Vec<Cell<'static>> {
        let mut cells: Vec<Option<Cell>> = cells.into_iter().map(Some).collect();
        cols.iter().filter_map(|(i, _)| cells[*i].take()).collect()
    };
    let head = Row::new(pick(
        [
            "",
            "",
            "name",
            "version",
            "latest",
            "status",
            "via",
            "scope",
            "managed by / note",
        ]
        .map(Cell::from)
        .to_vec(),
    ))
    .style(t.s(t.dim));
    let body: Vec<Row> = rows
        .iter()
        .enumerate()
        .skip(offset)
        .take(height)
        .map(|(i, it)| {
            let sel = app.selected.contains(&it.id);
            let note = match it.label {
                Label::NotApplicable => it.reason.clone().unwrap_or_default(),
                Label::Failed => it.last_error.clone().unwrap_or_default(),
                Label::Drifted => "check fails after envira applied it".into(),
                _ if update_needs_sudo(app, it) => "envira · update needs sudo".into(),
                _ => match (it.ownership, &it.path) {
                    (Some(Ownership::External), Some(p)) => {
                        format!("other · {}", short_path(app, p))
                    }
                    (Some(o), _) => managed_by(o).to_string(),
                    (None, _) => String::new(),
                },
            };
            let note_color = match (it.label, it.ownership) {
                (Label::Failed, _) => t.red,
                _ if update_needs_sudo(app, it) => t.yellow,
                _ => t.dim,
            };
            // Missing items show the route an install would take, behind an arrow.
            let planned = !it.present && it.label != Label::NotApplicable;
            let via = match (&it.via, planned) {
                (Some(v), true) => format!("→ {}", via_name(v)),
                (Some(v), false) => via_name(v).to_string(),
                // Unknown: the note says where it came from.
                (None, _) => String::new(),
            };
            let scope = match it.scope {
                Some(s) if it.label != Label::NotApplicable => Span::styled(
                    s.to_string(),
                    t.s(if s == Scope::System && !planned {
                        t.orange
                    } else {
                        t.dim
                    }),
                ),
                _ => Span::raw(""),
            };
            let version = match (it.kind, it.method_version.as_ref().or(it.version.as_ref())) {
                (Kind::Config, _) => String::new(),
                (_, Some(v)) => truncate(v, 9),
                _ => "—".into(),
            };
            let latest = match (&it.latest, it.present) {
                (Some(l), true) => Span::styled(
                    truncate(l, 9),
                    t.s(if it.label == Label::Outdated {
                        t.blue
                    } else {
                        t.dim
                    }),
                ),
                _ => Span::raw(""),
            };
            let row = Row::new(pick(vec![
                Cell::from(Span::styled(
                    if sel { "[x]" } else { "[ ]" },
                    t.s(if sel { t.magenta } else { t.dim }),
                )),
                Cell::from(Span::styled(
                    it.label.glyph(),
                    t.s(label_color(t, it.label)),
                )),
                Cell::from(Span::styled(
                    truncate(&it.id, name_w as usize),
                    t.s(if it.label == Label::NotApplicable {
                        t.dim
                    } else {
                        t.fg
                    }),
                )),
                Cell::from(version),
                Cell::from(latest),
                Cell::from(Span::styled(it.label.text(), t.s(label_color(t, it.label)))),
                Cell::from(Span::styled(via, t.s(t.dim))),
                Cell::from(scope),
                Cell::from(Span::styled(truncate(&note, note_w), t.s(note_color))),
            ]));
            if i == app.cursor && app.focus == Focus::Items {
                row.style(t.cursor())
            } else {
                row
            }
        })
        .collect();
    let widths: Vec<Constraint> = cols.iter().map(|(_, c)| *c).collect();
    if rows.is_empty() {
        let msg = if app.snap.is_none() {
            "detecting…"
        } else {
            "no items match"
        };
        let msg = if app.snap.is_some() && app.filter != Filter::All {
            format!("{msg} (filter: {})", app.filter.name())
        } else {
            msg.to_string()
        };
        f.render_widget(Paragraph::new(Span::styled(msg, t.s(t.dim))), inner);
        return;
    }
    f.render_widget(
        Table::new(body, widths).header(head).column_spacing(1),
        inner,
    );
}

fn detail(f: &mut Frame, area: Rect, app: &App, t: &Theme) {
    let mut tabs: Vec<Span> = Vec::new();
    let mut marks = Vec::new();
    for (i, (tab, name)) in [
        (DetailTab::Info, "Info"),
        (DetailTab::Commands, "Commands"),
        (DetailTab::Log, "Log"),
    ]
    .iter()
    .enumerate()
    {
        if i > 0 {
            tabs.push(Span::styled(" - ", t.s(t.dim)));
        }
        let st = if *tab == app.detail {
            Style::new().fg(t.focus)
        } else {
            t.s(t.dim)
        };
        marks.push((tabs.len(), Hit::Detail(*tab)));
        tabs.push(Span::styled(*name, st));
    }
    tabs.push(Span::styled("  tab", t.s(t.dim)));
    let row = Rect::new(area.x + 1, area.y, area.width.saturating_sub(2), 1);
    hit_spans(app, row, &tabs, marks);
    let block = Block::bordered()
        .border_type(BorderType::Rounded)
        .border_style(t.s(t.border))
        .title(Line::from([vec![], tabs].concat()));
    let inner = block.inner(area);
    f.render_widget(block, area);
    app.hit(inner, Hit::Panel(Panel::Detail));
    let Some(it) = app.current() else { return };
    let lines = match app.detail {
        DetailTab::Info => info_lines(app, it, t),
        DetailTab::Commands => command_lines(app, it, t),
        DetailTab::Log => log_lines(app, it, t, inner.height as usize),
    };
    f.render_widget(
        Paragraph::new(lines)
            .scroll((app.detail_scroll as u16, 0))
            .wrap(Wrap { trim: false }),
        inner,
    );
}

fn info_lines<'a>(app: &'a App, it: &'a ItemStatus, t: &Theme) -> Vec<Line<'a>> {
    let cat = &app.catalog;
    let kind = if it.kind == Kind::Tool {
        "tool"
    } else {
        "config"
    };
    let mut first = vec![
        Span::styled(
            it.id.clone(),
            Style::new().fg(t.blue).add_modifier(Modifier::BOLD),
        ),
        Span::raw("  "),
        Span::styled(
            format!("{} {}", it.label.glyph(), it.label.text()),
            t.s(label_color(t, it.label)),
        ),
        Span::styled(format!("  {} · {kind}", it.category), t.s(t.dim)),
    ];
    if it.present {
        if let Some(s) = it.scope {
            let c = if s == Scope::System { t.orange } else { t.dim };
            first.push(Span::styled(" · ", t.s(t.dim)));
            first.push(Span::styled(s.to_string(), t.s(c)));
        }
        if let Some(v) = &it.via {
            first.push(Span::styled(format!(" · via {}", via_name(v)), t.s(t.dim)));
        }
    }
    let mut lines = vec![
        Line::from(first),
        Line::styled(cat.desc(&it.id).to_string(), t.s(t.fg)),
    ];
    let deps = cat.depends(&it.id);
    if !deps.is_empty() {
        let mut spans = vec![Span::styled("needs ", t.s(t.dim))];
        for d in deps {
            let ok = app
                .snap
                .as_ref()
                .and_then(|s| s.get(d))
                .is_some_and(|s| s.present);
            spans.push(Span::raw(format!("{d} ")));
            spans.push(Span::styled(
                if ok { "✓  " } else { "✗  " },
                t.s(if ok { t.green } else { t.red }),
            ));
        }
        lines.push(Line::from(spans));
    }
    if let Some(p) = &it.path {
        lines.push(Line::from(vec![
            Span::styled("path ", t.s(t.dim)),
            Span::raw(p.clone()),
            Span::styled(
                format!("  {}", it.version.as_deref().unwrap_or("")),
                t.s(t.dim),
            ),
        ]));
    }
    if it.present && it.kind == Kind::Tool {
        let mut spans = vec![Span::styled("latest ", t.s(t.dim))];
        match (&it.latest, &it.latest_error, &it.route) {
            (Some(l), _, _) => spans.push(Span::styled(
                l.clone(),
                t.s(if it.label == Label::Outdated {
                    t.blue
                } else {
                    t.fg
                }),
            )),
            (None, Some(e), _) => spans.push(Span::styled(format!("unknown ({e})"), t.s(t.yellow))),
            (None, None, Some(_)) => spans.push(Span::styled("not checked", t.s(t.dim))),
            (None, None, None) => {}
        }
        match &it.route {
            Some(r) if update_needs_sudo(app, it) => spans.push(Span::styled(
                format!(
                    "  · updates via {} (system) need sudo, which this user lacks",
                    via_name(&r.via)
                ),
                t.s(t.yellow),
            )),
            Some(r) => spans.push(Span::styled(
                format!("  · updates via {} ({})", via_name(&r.via), r.scope),
                t.s(t.dim),
            )),
            None => spans.push(Span::styled(
                "  · not updated by envira (installed elsewhere)",
                t.s(t.dim),
            )),
        }
        lines.push(Line::from(spans));
        if it.ownership == Some(Ownership::Adoptable) {
            lines.push(Line::styled(
                "installed outside envira · A: let envira manage it (no reinstall)",
                t.s(t.dim),
            ));
        }
    }
    if !it.present
        && it.reason.is_none()
        && let (Some(v), Some(s)) = (&it.via, it.scope)
    {
        let sudo = if s == Scope::System { ", sudo" } else { "" };
        lines.push(Line::styled(
            format!("would install via {v} ({s}{sudo})"),
            t.s(t.dim),
        ));
    }
    if let Some(r) = &it.reason {
        lines.push(Line::styled(
            format!("not for this machine: {r}"),
            t.s(t.yellow),
        ));
    }
    if let Some(e) = &it.last_error {
        lines.push(Line::styled(
            format!("last attempt failed: {e} · l shows the log"),
            t.s(t.red),
        ));
    }
    if it.label == Label::New {
        lines.push(Line::styled(
            "new in the catalog since your last install",
            t.s(t.cyan),
        ));
    }
    lines
}

fn command_lines<'a>(app: &'a App, it: &'a ItemStatus, t: &Theme) -> Vec<Line<'a>> {
    let present = app.present();
    let mut p = Planner::new(&app.catalog, &app.facts, &present);
    p.prefer_system = app.prefer_system;
    match p.choose(&it.id) {
        Err(e) => vec![Line::styled(
            format!("cannot install here: {e}"),
            t.s(t.yellow),
        )],
        Ok(c) => {
            let mut lines = vec![Line::styled(
                format!("via {} · scope {}", via_name(&c.via), c.scope),
                t.s(t.dim),
            )];
            for s in c.steps {
                if let Some(chk) = s.check {
                    lines.push(Line::from(vec![
                        Span::styled("check ", t.s(t.dim)),
                        Span::raw(chk),
                    ]));
                }
                if let Some(cmd) = s.cmd {
                    lines.push(Line::from(vec![
                        Span::styled("$ ", t.s(t.magenta)),
                        Span::raw(cmd),
                    ]));
                }
            }
            for v in c.verify.iter().take(1) {
                lines.push(Line::from(vec![
                    Span::styled("verify ", t.s(t.dim)),
                    Span::raw(v.clone()),
                ]));
            }
            lines
        }
    }
}

/// The end of the item's last log, enough for `height` lines (read on every frame, so never
/// the whole file).
fn last_log(app: &App, id: &str, height: usize) -> Option<(String, Vec<String>)> {
    let st = app.state.as_ref()?;
    let run = st
        .tools
        .get(id)
        .and_then(|r| r.last_run.clone())
        .or_else(|| st.configs.get(id).and_then(|r| r.last_run.clone()))?;
    let path = app.paths.run_dir(&run).join(format!("{id}.log"));
    let lines = crate::state::tail_lines(&path, height, 256 << 10).ok()?;
    Some((run, lines))
}

fn log_lines<'a>(app: &'a App, it: &'a ItemStatus, t: &Theme, height: usize) -> Vec<Line<'a>> {
    match last_log(app, &it.id, height) {
        None => vec![Line::styled(
            "no envira run has touched this item yet",
            t.s(t.dim),
        )],
        Some((run, lines)) => {
            let start = lines.len().saturating_sub(height.saturating_sub(1));
            let mut out = vec![Line::styled(format!("run {run}"), t.s(t.dim))];
            out.extend(lines[start..].iter().map(|l| log_line(t, l)));
            out
        }
    }
}

/// `n/m` from the latest "retrying ... (attempt n/m)" line of a running step.
fn retry_attempt(log: &[String]) -> Option<&str> {
    log.iter()
        .rev()
        .find(|l| l.starts_with("[envira] ") && l.contains("; retrying in "))?
        .rsplit_once("(attempt ")?
        .1
        .strip_suffix(')')
}

/// A log line from the live run or a log file; files add `# ` header/trailer lines and an
/// `HH:MM:SS ` time before every line.
fn log_line<'a>(t: &Theme, l: &str) -> Line<'a> {
    if l.starts_with("# ") {
        return Line::styled(l.to_string(), t.s(t.dim));
    }
    let b = l.as_bytes();
    let timed = b.len() >= 9
        && b[8] == b' '
        && b[2] == b':'
        && b[5] == b':'
        && [0, 1, 3, 4, 6, 7].iter().all(|&i| b[i].is_ascii_digit());
    let (time, body) = if timed { l.split_at(9) } else { ("", l) };
    let body = if body.starts_with("[envira]") {
        Span::styled(body.to_string(), t.s(t.dim))
    } else if body.starts_with("$ ") {
        Span::styled(body.to_string(), t.s(t.magenta))
    } else {
        Span::raw(body.to_string())
    };
    Line::from(vec![Span::styled(time.to_string(), t.s(t.dim)), body])
}

pub fn plural<'a>(n: usize, one: &'a str, many: &'a str) -> &'a str {
    if n == 1 { one } else { many }
}

/// Items table columns that fit `width`, as (index, width): on a narrow terminal via, scope,
/// latest, the status word and the version go, in that order (the detail panel has them all).
fn item_columns(width: u16, name: u16) -> Vec<(usize, Constraint)> {
    const NOTE: u16 = 17;
    let mut cols: Vec<(usize, u16)> = vec![
        (0, 3),
        (1, 1),
        (2, name),
        (3, 9),
        (4, 9),
        (5, 9),
        (6, 15),
        (7, 6),
    ];
    for drop in [6, 7, 4, 5, 3] {
        // The note column and one space between each pair of columns.
        let used = cols.iter().map(|c| c.1).sum::<u16>() + cols.len() as u16 + NOTE;
        if used <= width {
            break;
        }
        cols.retain(|c| c.0 != drop);
    }
    let mut out: Vec<(usize, Constraint)> = cols
        .into_iter()
        .map(|(i, w)| (i, Constraint::Length(w)))
        .collect();
    out.push((8, Constraint::Min(NOTE)));
    out
}

/// Who looks after an installed tool, as the items table says it.
fn managed_by(o: Ownership) -> &'static str {
    match o {
        Ownership::Managed => "envira",
        Ownership::Adoptable => "not envira · A",
        Ownership::External => "other",
    }
}

/// A unit's reason on screen: what the user picked reads as "selected".
fn why(reason: &str) -> String {
    if reason == "requested" {
        "selected".into()
    } else {
        reason.to_string()
    }
}

/// A route's name on screen: a catalog's raw `run` commands read as "command".
fn via_name(v: &str) -> &str {
    if v == "run" { "command" } else { v }
}

fn truncate(s: &str, n: usize) -> String {
    if s.chars().count() <= n {
        s.to_string()
    } else {
        format!(
            "{}…",
            s.chars().take(n.saturating_sub(1)).collect::<String>()
        )
    }
}

// ------------------------------------------------------------------------------------------------
// Plan review
// ------------------------------------------------------------------------------------------------

fn plan_view(f: &mut Frame, area: Rect, app: &App, t: &Theme) {
    let Some((plan, title)) = &app.plan else {
        return;
    };
    let [h, top, steps, bottom, keys] = Layout::vertical([
        Constraint::Length(1),
        Constraint::Length(4),
        Constraint::Min(5),
        Constraint::Length(9),
        Constraint::Length(1),
    ])
    .areas(area);
    header(
        f,
        h,
        app,
        t,
        vec![
            Span::raw("review plan · "),
            Span::styled(title.clone(), t.s(t.fg)),
        ],
    );

    let n = plan.units.len();
    let count = |a: Action| plan.units.iter().filter(|u| u.action == a).count();
    let block = panel(t, 1, "Plan", false, vec![]);
    let inner = block.inner(top);
    f.render_widget(block, top);
    let mut first = vec![Span::styled(
        format!("{n} {}  ", plural(n, "step", "steps")),
        Style::new().fg(t.fg).add_modifier(Modifier::BOLD),
    )];
    for (a, one, many, c) in [
        (Action::Install, "install", "installs", t.cyan),
        (Action::Update, "update", "updates", t.blue),
        (Action::Apply, "config", "configs", t.magenta),
        (Action::Revert, "revert", "reverts", t.yellow),
    ] {
        let k = count(a);
        if k > 0 {
            first.push(Span::styled(
                format!("{k} {}  ", plural(k, one, many)),
                t.s(c),
            ));
        }
    }
    first.push(Span::styled("+ shell setup", t.s(t.dim)));
    let sudo = if plan.needs_sudo(&app.facts) {
        Span::styled(
            if app.facts.sudo_asks_password() {
                "! needs sudo · enter: password or user mode"
            } else {
                "! needs sudo (no password needed)"
            },
            t.s(t.yellow),
        )
    } else {
        Span::styled("✓ sudo not needed", t.s(t.green))
    };
    let low = plan.low_space(&app.facts);
    let rest = if low.is_empty() {
        Span::raw("")
    } else {
        let what: Vec<String> = low
            .iter()
            .map(|(d, free)| format!("{} on {}", crate::plan::fmt_bytes(*free), d.display()))
            .collect();
        Span::styled(
            format!(" · ! low disk space: {} free", what.join(", ")),
            t.s(t.red),
        )
    };
    f.render_widget(
        Paragraph::new(vec![Line::from(first), Line::from(vec![sudo, rest])]),
        inner,
    );

    let block = panel(t, 2, "Steps", true, vec![]);
    let inner = block.inner(steps);
    f.render_widget(block, steps);
    let item_w = plan
        .units
        .iter()
        .map(|u| u.id.chars().count())
        .max()
        .unwrap_or(0)
        .clamp(10, 24) as u16;
    // (index, width); via, then scope, go when `why` would get under 16 columns.
    let mut scols: Vec<(usize, u16)> = vec![(0, 3), (1, 8), (2, item_w), (3, 15), (4, 7)];
    let why_w = |c: &[(usize, u16)]| {
        inner
            .width
            .saturating_sub(c.iter().map(|x| x.1 + 1).sum::<u16>())
    };
    for drop in [3, 4] {
        if why_w(&scols) >= 16 {
            break;
        }
        scols.retain(|c| c.0 != drop);
    }
    let why_room = why_w(&scols).max(1) as usize;
    let spick = |cells: Vec<Cell<'static>>| -> Vec<Cell<'static>> {
        let mut cells: Vec<Option<Cell>> = cells.into_iter().map(Some).collect();
        let mut out: Vec<Cell> = scols.iter().filter_map(|(i, _)| cells[*i].take()).collect();
        out.extend(cells[5].take());
        out
    };
    let head = Row::new(spick(
        ["#", "action", "item", "via", "scope", "why"]
            .map(Cell::from)
            .to_vec(),
    ))
    .style(t.s(t.dim));
    let height = inner.height.saturating_sub(1) as usize;
    let offset = app.plan_view.top(app.plan_cursor, plan.units.len(), height);
    app.hit(inner, Hit::Panel(Panel::Plan));
    for (row, i) in (offset..plan.units.len()).take(height).enumerate() {
        let y = inner.y + 1 + row as u16;
        app.hit(Rect::new(inner.x, y, inner.width, 1), Hit::PlanRow(i));
    }
    let rows: Vec<Row> = plan
        .units
        .iter()
        .enumerate()
        .skip(offset)
        .take(height)
        .map(|(i, u)| {
            let c = match u.action {
                Action::Install => t.cyan,
                Action::Update => t.blue,
                Action::Apply => t.magenta,
                Action::Revert => t.yellow,
            };
            let row = Row::new(spick(vec![
                Cell::from(Span::styled((i + 1).to_string(), t.s(t.dim))),
                Cell::from(Span::styled(u.action.as_str(), t.s(c))),
                Cell::from(truncate(&u.id, item_w as usize)),
                Cell::from(Span::styled(via_name(&u.via).to_string(), t.s(t.dim))),
                Cell::from(Span::styled(u.scope.to_string(), t.s(t.dim))),
                Cell::from(Span::styled(
                    truncate(&why(&u.reason), why_room),
                    t.s(t.dim),
                )),
            ]));
            if i == app.plan_cursor {
                row.style(t.cursor())
            } else {
                row
            }
        })
        .collect();
    let mut widths: Vec<Constraint> = scols.iter().map(|c| Constraint::Length(c.1)).collect();
    widths.push(Constraint::Min(1));
    f.render_widget(
        Table::new(rows, widths).header(head).column_spacing(1),
        inner,
    );

    let [cmds, skipped] = Layout::horizontal([
        Constraint::Min(30),
        Constraint::Length((bottom.width / 3).min(44)),
    ])
    .areas(bottom);
    let u = plan.units.get(app.plan_cursor);
    let block = panel(
        t,
        3,
        "Commands",
        false,
        u.map(|u| {
            vec![Span::styled(
                format!("· step {} {}", app.plan_cursor + 1, u.id),
                t.s(t.dim),
            )]
        })
        .unwrap_or_default(),
    );
    let inner = block.inner(cmds);
    f.render_widget(block, cmds);
    if let Some(u) = u {
        let mut lines = Vec::new();
        for s in &u.steps {
            if let Some(c) = &s.check {
                lines.push(Line::from(vec![
                    Span::styled("skip if ", t.s(t.dim)),
                    Span::raw(c.clone()),
                ]));
            }
            match &s.cmd {
                Some(c) => lines.push(Line::from(vec![
                    Span::styled("$ ", t.s(t.magenta)),
                    Span::raw(c.clone()),
                ])),
                None => lines.push(Line::styled(
                    format!("· {} (manual)", s.label),
                    t.s(t.yellow),
                )),
            }
        }
        if let Some(v) = u.verify.first() {
            lines.push(Line::from(vec![
                Span::styled("verify ", t.s(t.dim)),
                Span::raw(v.clone()),
            ]));
        }
        lines.push(Line::from(vec![
            Span::styled("log ", t.s(t.dim)),
            Span::raw(format!("logs/<run>/{}.log", u.id)),
        ]));
        f.render_widget(Paragraph::new(lines).wrap(Wrap { trim: false }), inner);
    }
    let block = panel(t, 4, "Left out", false, vec![]);
    let inner = block.inner(skipped);
    f.render_widget(block, skipped);
    let lines: Vec<Line> = if plan.skipped.is_empty() {
        vec![Line::styled("nothing", t.s(t.dim))]
    } else {
        plan.skipped
            .iter()
            .map(|(id, why)| {
                Line::from(vec![
                    Span::raw(format!("{id} ")),
                    Span::styled(why.clone(), t.s(t.dim)),
                ])
            })
            .collect()
    };
    f.render_widget(Paragraph::new(lines).wrap(Wrap { trim: true }), inner);

    // The run label is clickable; a toast in its place is not.
    let toast = toast_span(app, t);
    let run_key = toast
        .is_none()
        .then(|| KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
    keybar(
        f,
        keys,
        app,
        t,
        if app.sudo.is_some() {
            &[
                ("Password", "type"),
                ("Choose", "←→"),
                ("Confirm", "enter"),
                ("Back", "esc"),
            ]
        } else {
            &[("Step", "↑↓"), ("Save as script", "c"), ("Back", "esc")]
        },
        toast.or_else(|| {
            Some(Span::styled(
                format!("Run {n} {}: enter", plural(n, "step", "steps")),
                Style::new().fg(t.magenta).add_modifier(Modifier::BOLD),
            ))
        }),
        run_key,
    );
}

// ------------------------------------------------------------------------------------------------
// Run and summary
// ------------------------------------------------------------------------------------------------

fn run_view(f: &mut Frame, area: Rect, app: &App, t: &Theme) {
    let Some(run) = &app.run else { return };
    let [h, top, main, keys] = Layout::vertical([
        Constraint::Length(1),
        Constraint::Length(4),
        Constraint::Min(8),
        Constraint::Length(1),
    ])
    .areas(area);
    let n = run.units.len();
    let ok = run.count(|u| matches!(u, UnitState::Done(Outcome::Ok { .. }, _)));
    let failed = run.count(|u| {
        matches!(
            u,
            UnitState::Done(Outcome::Failed(_) | Outcome::Cancelled, _)
        )
    });
    let blocked = run.count(|u| matches!(u, UnitState::Done(Outcome::Blocked(_), _)));
    let skipped = run.count(|u| matches!(u, UnitState::Done(Outcome::Skipped, _)));
    let running = run.count(|u| matches!(u, UnitState::Running(_)));
    let queued = run.count(|u| matches!(u, UnitState::Queued));
    let what = match &run.finished {
        None => vec![
            Span::styled(
                if run.stopping { "stopping" } else { "running" },
                t.s(t.yellow),
            ),
            Span::styled(format!(" · run {} · elapsed ", run.run_id), t.s(t.dim)),
            Span::raw(fmt_duration(run.started.elapsed())),
        ],
        Some((_, took)) => vec![
            Span::styled(
                "run finished",
                t.s(if failed + blocked + skipped == 0 {
                    t.green
                } else {
                    t.red
                }),
            ),
            Span::styled(format!(" · run {} · took ", run.run_id), t.s(t.dim)),
            Span::raw(fmt_duration(*took)),
        ],
    };
    header(f, h, app, t, what);

    let block = panel(
        t,
        1,
        if run.finished.is_some() {
            "Result"
        } else {
            "Progress"
        },
        false,
        vec![],
    );
    let inner = block.inner(top);
    f.render_widget(block, top);
    let done = ok + failed + blocked + skipped;
    let mut l1 = vec![
        Span::styled(
            format!("{done}"),
            Style::new().fg(t.fg).add_modifier(Modifier::BOLD),
        ),
        Span::styled(format!(" / {n}   "), t.s(t.dim)),
        Span::styled(format!("✓ {ok} done  "), t.s(t.green)),
    ];
    if failed > 0 {
        l1.push(Span::styled(format!("✗ {failed} failed  "), t.s(t.red)));
    }
    if blocked > 0 {
        l1.push(Span::styled(
            format!("⊘ {blocked} blocked  "),
            t.s(t.yellow),
        ));
    }
    if skipped > 0 {
        l1.push(Span::styled(format!("– {skipped} skipped  "), t.s(t.dim)));
    }
    if running > 0 {
        l1.push(Span::styled(
            format!("{} {running} running  ", SPINNER[(app.tick % 10) as usize]),
            t.s(t.blue),
        ));
    }
    if queued > 0 {
        l1.push(Span::styled(format!("· {queued} queued"), t.s(t.dim)));
    }
    let gauge = bar(
        t,
        inner.width,
        &[
            (ok, t.green),
            (failed, t.red),
            (blocked, t.yellow),
            (skipped, t.dim),
            (running, t.blue),
        ],
        n,
    );
    f.render_widget(Paragraph::new(vec![Line::from(l1), gauge]), inner);

    let [left, right] = Layout::horizontal([
        Constraint::Length(if run.zoom { 0 } else { 52.min(area.width / 2) }),
        Constraint::Min(30),
    ])
    .areas(main);
    let [steps, info] = Layout::vertical([Constraint::Min(4), Constraint::Length(7)]).areas(left);
    let block = panel(t, 2, "Steps", false, vec![]);
    let inner = block.inner(steps);
    f.render_widget(block, steps);
    let height = inner.height as usize;
    let offset = app.steps_view.top(run.sel, run.units.len(), height);
    app.hit(inner, Hit::Panel(Panel::Steps));
    for (row, i) in (offset..run.units.len()).take(height).enumerate() {
        let y = inner.y + row as u16;
        app.hit(Rect::new(inner.x, y, inner.width, 1), Hit::StepRow(i));
    }
    // Narrow: the action column goes before the outcome gets under 12 columns; it is cut with
    // an ellipsis.
    let id_w = run
        .plan
        .units
        .iter()
        .map(|u| u.id.chars().count())
        .max()
        .unwrap_or(0)
        .clamp(8, 18) as u16;
    let fixed = 1 + 1 + id_w + 1 + 7 + 1 + 6 + 1;
    let with_action = inner.width.saturating_sub(fixed) >= 12;
    let info_w = inner
        .width
        .saturating_sub(if with_action { fixed } else { fixed - 8 })
        .max(1) as usize;
    let rows: Vec<Row> = run
        .plan
        .units
        .iter()
        .zip(&run.units)
        .enumerate()
        .skip(offset)
        .take(height)
        .map(|(i, (u, s))| {
            let (g, gc, time, info, ic) = match s {
                UnitState::Queued => ("·", t.dim, String::new(), "queued".to_string(), t.dim),
                UnitState::Running(st) => (
                    SPINNER[(app.tick % 10) as usize],
                    t.blue,
                    fmt_duration(st.elapsed()),
                    retry_attempt(run.logs.get(i).map(Vec::as_slice).unwrap_or(&[]))
                        .map(|a| format!("running · attempt {a}"))
                        .unwrap_or_else(|| "running".into()),
                    t.blue,
                ),
                UnitState::Done(o, d) => match o {
                    Outcome::Ok { version } => (
                        "✓",
                        t.green,
                        fmt_duration(*d),
                        version.clone().unwrap_or_else(|| "ok".into()),
                        t.dim,
                    ),
                    Outcome::Failed(e) => ("✗", t.red, fmt_duration(*d), e.clone(), t.red),
                    Outcome::Cancelled => ("✗", t.red, fmt_duration(*d), "cancelled".into(), t.red),
                    Outcome::Blocked(dep) => (
                        "⊘",
                        t.yellow,
                        String::new(),
                        format!("needs {dep}"),
                        t.yellow,
                    ),
                    Outcome::Skipped => ("–", t.dim, String::new(), "skipped".into(), t.dim),
                },
            };
            let mut cells = vec![
                Cell::from(Span::styled(g, t.s(gc))),
                Cell::from(truncate(&u.id, id_w as usize)),
            ];
            if with_action {
                cells.push(Cell::from(Span::styled(u.action.as_str(), t.s(t.dim))));
            }
            cells.extend([
                Cell::from(Span::styled(time, t.s(t.dim))),
                Cell::from(Span::styled(truncate(&info, info_w), t.s(ic))),
            ]);
            let row = Row::new(cells);
            if i == run.sel {
                row.style(t.cursor())
            } else {
                row
            }
        })
        .collect();
    let mut widths = vec![Constraint::Length(1), Constraint::Length(id_w)];
    if with_action {
        widths.push(Constraint::Length(7));
    }
    widths.extend([Constraint::Length(6), Constraint::Min(1)]);
    f.render_widget(Table::new(rows, widths).column_spacing(1), inner);

    let block = panel(t, 3, "Info", false, vec![]);
    let inner = block.inner(info);
    f.render_widget(block, info);
    let mut lines = Vec::new();
    if let Some(u) = run.plan.units.get(run.sel) {
        lines.push(Line::from(vec![
            Span::styled(
                u.id.clone(),
                Style::new().fg(t.blue).add_modifier(Modifier::BOLD),
            ),
            Span::styled(
                format!(
                    " · {} via {} · {}",
                    u.action.as_str(),
                    via_name(&u.via),
                    u.scope
                ),
                t.s(t.dim),
            ),
        ]));
        lines.push(Line::styled(why(&u.reason), t.s(t.dim)));
        if !u.deps.is_empty() {
            lines.push(Line::from(vec![
                Span::styled("needs ", t.s(t.dim)),
                Span::raw(u.deps.join(", ")),
            ]));
        }
        if let Some(h) = run.logs.get(run.sel).and_then(|log| {
            log.iter()
                .rev()
                .find_map(|l| l.strip_prefix("[envira] hint "))
        }) {
            lines.push(Line::styled(format!("hint {h}"), t.s(t.yellow)));
        }
        lines.push(Line::styled(
            format!(
                "log {}",
                app.paths
                    .run_dir(&run.run_id)
                    .join(format!("{}.log", u.id))
                    .display()
            ),
            t.s(t.dim),
        ));
    }
    if let Some(r) = &run.shell_env {
        lines.push(match r {
            Ok(files) if files.is_empty() => Line::styled("shell setup unchanged", t.s(t.dim)),
            Ok(files) => Line::styled(
                format!(
                    "shell setup: {} files updated · open a new shell",
                    files.len()
                ),
                t.s(t.green),
            ),
            Err(e) => Line::styled(format!("shell setup not updated: {e}"), t.s(t.red)),
        });
    }
    f.render_widget(Paragraph::new(lines).wrap(Wrap { trim: true }), inner);

    let unit = run.plan.units.get(run.sel);
    let extra = vec![
        Span::styled(
            format!("· {} ", unit.map(|u| u.id.as_str()).unwrap_or("")),
            t.s(t.dim),
        ),
        if run.follow {
            Span::styled("● follow", t.s(t.green))
        } else {
            Span::styled("○ follow", t.s(t.dim))
        },
    ];
    let f_key = KeyEvent::new(KeyCode::Char('f'), KeyModifiers::NONE);
    hit_title(app, right, "[4] Log", &extra, vec![(1, Hit::Key(f_key))]);
    let block = panel(t, 4, "Log", true, extra);
    let inner = block.inner(right);
    f.render_widget(block, right);
    app.hit(inner, Hit::Panel(Panel::Log));
    let log = run.logs.get(run.sel).map(Vec::as_slice).unwrap_or(&[]);
    let height = inner.height as usize;
    let end = log.len().saturating_sub(run.scroll.min(log.len()));
    let start = end.saturating_sub(height);
    let lines: Vec<Line> = log[start..end].iter().map(|l| log_line(t, l)).collect();
    f.render_widget(Paragraph::new(lines), inner);

    let keyset: &[(&str, &str)] = if run.finished.is_some() {
        &[
            ("Step", "↑↓"),
            ("Scroll", "PgUp/PgDn"),
            ("Zoom log", "z"),
            ("Help", "?"),
            ("Dashboard", "enter"),
            ("Quit", "q"),
        ]
    } else {
        &[
            ("Step", "↑↓"),
            ("Follow", "f"),
            ("Scroll", "PgUp/PgDn"),
            ("Zoom log", "z"),
            ("Help", "?"),
            ("Cancel running", "c"),
            ("Stop after running", "ctrl-c"),
        ]
    };
    let toast = toast_span(app, t);
    let retry = toast.is_none() && run.finished.is_some() && failed + blocked + skipped > 0;
    let right_span = toast.or_else(|| {
        retry.then(|| {
            Span::styled(
                format!("Retry unfinished ({}): r", failed + blocked + skipped),
                Style::new().fg(t.magenta).add_modifier(Modifier::BOLD),
            )
        })
    });
    let r_key = retry.then(|| KeyEvent::new(KeyCode::Char('r'), KeyModifiers::NONE));
    keybar(f, keys, app, t, keyset, right_span, r_key);
}

// ------------------------------------------------------------------------------------------------
// Help
// ------------------------------------------------------------------------------------------------

const DASHBOARD_KEYS: &[(&str, &str)] = &[
    ("← →  h l", "Bundles · Items"),
    ("↑↓  j k", "move"),
    (
        "space",
        "bundle: follow; what it lacks gets selected, now and on later starts",
    ),
    ("space", "item: select or unselect it"),
    ("a  n", "select all not installed in view · none"),
    ("enter", "install the selection, update its outdated tools"),
    ("u", "update every outdated tool"),
    ("/  f", "search · cycle status filter"),
    ("tab  J K", "Info / Commands / Log · scroll it"),
    ("x", "revert selected configs"),
    ("S", "new installs: system (sudo) or user scope"),
    (
        "A",
        "take over tools installed outside envira (selected, or this one)",
    ),
    ("t", "retry what the last run left unfinished"),
    ("U", "update envira itself (envira self-update)"),
    ("R  r", "re-detect + check updates · reload catalog"),
    ("esc", "clear the search"),
    ("q", "quit"),
    ("", ""),
    ("status", "✓ installed  ↑ outdated  ! failed or drifted"),
    ("", "◌ wanted  ◆ new  · available  – not for this machine"),
    ("Selected", "+ install  ↑ update  ◇ config  ↺ revert"),
    (
        "Bundles",
        "● followed  ○ not followed · 7/9: installed / total",
    ),
];

/// Keys of the current screen.
fn help_rows(screen: Screen) -> &'static [(&'static str, &'static str)] {
    match screen {
        Screen::Loading => &[("q  esc", "quit")],
        Screen::Plan => &[
            ("↑↓ j k", "move"),
            ("enter", "run the plan"),
            ("c", "save the plan as a shell script"),
            ("esc  q", "back without running"),
            (
                "",
                "steps run in order; a failed step blocks only its dependents",
            ),
        ],
        Screen::Run => &[
            ("↑↓ j k", "choose a step (stops following)"),
            ("f", "follow the running step"),
            ("PgUp PgDn", "scroll the log"),
            ("z", "full-width log"),
            ("c", "cancel the running steps"),
            ("ctrl-c", "start nothing new; finish the running steps"),
            ("r", "when finished: retry what did not finish"),
            ("enter  esc", "when finished: back to the dashboard"),
            ("q", "when finished: quit"),
        ],
        Screen::Dashboard => DASHBOARD_KEYS,
    }
}

fn help(f: &mut Frame, area: Rect, app: &App, t: &Theme) {
    const KEY: usize = 12;
    let rows = help_rows(app.screen);
    let widest = rows
        .iter()
        .map(|(_, v)| v.chars().count())
        .max()
        .unwrap_or(0);
    // As wide as the longest line plus a space; on a narrow terminal the descriptions wrap.
    let w = ((KEY + widest + 3) as u16).min(area.width);
    let text = (w as usize).saturating_sub(KEY + 3).max(1);
    let mut lines: Vec<Line> = Vec::new();
    for (k, v) in rows {
        for (i, part) in wrap_words(v, text).into_iter().enumerate() {
            let key = if i == 0 { *k } else { "" };
            lines.push(Line::from(vec![
                Span::styled(format!("{key:>10}  "), t.s(t.cyan)),
                Span::raw(part),
            ]));
        }
    }
    // Too tall for the terminal: the keys scroll, the last line says so.
    let room = area.height.saturating_sub(2) as usize;
    let fits = lines.len() + 2 <= room;
    let max = if fits {
        0
    } else {
        (lines.len() + 1).saturating_sub(room) as u16
    };
    app.help_max.set(max);
    let scroll = app.help_scroll.min(max);
    if fits {
        lines.extend([Line::raw(""), Line::styled("any key closes", t.s(t.dim))]);
    } else {
        let body: Vec<Line> = lines
            .into_iter()
            .skip(scroll as usize)
            .take(room.saturating_sub(1))
            .collect();
        lines = body;
        let more = match (scroll > 0, scroll < max) {
            (true, true) => "↑↓ more",
            (false, _) => "↓ more",
            (true, false) => "↑ more",
        };
        lines.push(Line::styled(
            format!("{more} · any other key closes"),
            t.s(t.dim),
        ));
    }
    let h = (lines.len() as u16 + 2).min(area.height);
    let r = Rect {
        x: area.x + (area.width - w) / 2,
        y: area.y + (area.height - h) / 2,
        width: w,
        height: h,
    };
    f.render_widget(Clear, r);
    let block = Block::bordered()
        .border_type(BorderType::Rounded)
        .border_style(t.s(t.focus))
        .title(Span::styled(
            " Keys ",
            Style::new().fg(t.focus).add_modifier(Modifier::BOLD),
        ));
    f.render_widget(Paragraph::new(lines).block(block), r);
}

/// Splits `s` at spaces into lines of at most `n` characters (a longer word gets its own line).
fn wrap_words(s: &str, n: usize) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for word in s.split(' ') {
        match out.last_mut() {
            Some(line) if line.chars().count() + 1 + word.chars().count() <= n => {
                line.push(' ');
                line.push_str(word);
            }
            _ => out.push(word.to_string()),
        }
    }
    out
}

/// A system-scope tool envira manages, on a machine where this user cannot use sudo.
fn update_needs_sudo(app: &App, it: &ItemStatus) -> bool {
    it.ownership == Some(Ownership::Managed)
        && it.route.as_ref().is_some_and(|r| r.scope == Scope::System)
        && !app.facts.privilege.can_elevate()
}

/// `~/…` for paths in the home directory, keeping the first two levels below it.
fn short_path(app: &App, path: &str) -> String {
    let home = format!("{}/", app.facts.home.display());
    match path.strip_prefix(&home) {
        Some(rest) => {
            let parts: Vec<&str> = rest.split('/').collect();
            if parts.len() > 3 {
                format!("~/{}/{}/…", parts[0], parts[1])
            } else {
                format!("~/{rest}")
            }
        }
        None => path.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tui::app::tests::app;
    use crate::tui::app::{Cmd, RunView};
    use crossterm::event::{MouseButton, MouseEvent, MouseEventKind};
    use ratatui::Terminal;
    use ratatui::backend::TestBackend;

    #[test]
    fn the_sudo_dialog_asks_for_the_password_over_the_plan() {
        use crate::sudo_prompt::{Check, Prompt};
        let mut a = app();
        a.selected = ["nmap".to_string()].into();
        a.on_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
        a.sudo = Some(Prompt::new(1, "Back", true));
        // Not a string a build path or hash could hold.
        for c in "Q%~".chars() {
            a.on_key(KeyEvent::new(KeyCode::Char(c), KeyModifiers::NONE));
        }
        let out = render(&a, 100, 30);
        assert!(out.contains("Run 1 step: enter"), "{out}");
        assert!(
            out.contains("1 item in this plan needs sudo. Password:"),
            "{out}"
        );
        assert!(out.contains("•••"), "{out}");
        assert!(!out.contains("Q%~"), "{out}");
        assert!(
            out.contains("Use user mode") && out.contains("Back"),
            "{out}"
        );
        assert!(out.contains("Confirm: enter"), "{out}");
        // A click on a button presses it.
        a.on_sudo(Check::Wrong);
        let out = render(&a, 100, 30);
        assert!(out.contains("Wrong password, try again"), "{out}");
        let hit = a
            .hits
            .borrow()
            .iter()
            .find(|(_, h)| *h == Hit::Sudo(crate::sudo_prompt::Focus::Back))
            .map(|(r, _)| *r)
            .unwrap();
        a.on_mouse(MouseEvent {
            kind: MouseEventKind::Down(MouseButton::Left),
            column: hit.x,
            row: hit.y,
            modifiers: KeyModifiers::NONE,
        });
        assert!(a.sudo.is_none());
    }

    fn render(app: &App, w: u16, h: u16) -> String {
        let mut term = Terminal::new(TestBackend::new(w, h)).unwrap();
        term.draw(|f| draw(f, app, &Theme::tokyo_night())).unwrap();
        let buf = term.backend().buffer().clone();
        (0..h)
            .map(|y| {
                (0..w)
                    .map(|x| buf[(x, y)].symbol().to_string())
                    .collect::<String>()
                    .trim_end()
                    .to_string()
            })
            .collect::<Vec<_>>()
            .join("\n")
    }

    #[test]
    fn help_lines_are_whole_at_any_width() {
        let mut a = app();
        a.on_key(KeyEvent::new(KeyCode::Char('?'), KeyModifiers::NONE));
        for w in [140, 80, 50] {
            let s = render(&a, w, 40);
            // Wrapped lines join back with the key column's indent in between.
            let text = s
                .lines()
                .map(|l| l.trim_matches(|c: char| c == '│' || c == ' '))
                .collect::<Vec<_>>()
                .join(" ");
            let text = text.split_whitespace().collect::<Vec<_>>().join(" ");
            for (_, desc) in DASHBOARD_KEYS {
                let desc = desc.split_whitespace().collect::<Vec<_>>().join(" ");
                assert!(text.contains(&desc), "{w} cols: {desc:?} is cut\n{s}");
            }
        }
    }

    #[test]
    fn dashboard_shows_panels_and_items() {
        let a = app();
        // Tall enough to show every catalog item.
        let s = render(&a, 140, 160);
        for needle in [
            "Overview",
            "Bundles",
            "Selected",
            "Items",
            "Info - Commands - Log",
            "● Terminal",
            "◆",
            "herdr",
            "Panels: ← →",
            "Install/Update: enter",
            "Update all: u",
        ] {
            assert!(s.contains(needle), "missing {needle:?} in\n{s}");
        }
        assert!(s.contains("needs an NVIDIA GPU"), "{s}");
        // Small terminals must not panic.
        render(&a, 80, 24);
        render(&a, 40, 10);
    }

    #[test]
    fn bundle_list_scrolls_to_the_cursor() {
        let mut a = app();
        a.focus = Focus::Bundles;
        a.bundle_cursor = a.bundle_ids().len() - 1;
        let last = a
            .catalog
            .bundle_name(a.bundle_ids().last().unwrap())
            .to_string();
        let s = render(&a, 80, 24);
        assert!(s.contains(&last), "missing {last:?} in\n{s}");
    }

    #[test]
    fn outdated_tools_show_latest_and_glyph() {
        let mut a = app();
        a.on_latest(std::collections::BTreeMap::from([(
            "rust-toolchain".to_string(),
            crate::latest::Latest::Version("2.0.0".into()),
        )]));
        let s = render(&a, 140, 160);
        let row = s
            .lines()
            .find(|l| l.contains("rust-toolchain"))
            .unwrap_or_default();
        assert!(
            row.contains('↑') && row.contains("2.0.0") && row.contains("outdated"),
            "{row}"
        );
        assert!(s.contains("latest") && s.contains("Update all: u"), "{s}");
        assert!(!s.contains("u updates"), "{s}");
    }

    #[test]
    fn a_selected_outdated_tool_shows_as_an_update_before_and_in_the_plan() {
        let mut a = app();
        a.on_latest(std::collections::BTreeMap::from([(
            "rust-toolchain".to_string(),
            crate::latest::Latest::Version("2.0.0".into()),
        )]));
        a.selected = ["rust-toolchain".to_string(), "codex".to_string()].into();
        let s = render(&a, 140, 40);
        assert!(s.contains("↑ rust-toolchain"), "{s}");
        assert!(s.contains("Selected 2 · 5 steps (+3 deps)"), "{s}");
        a.on_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
        let s = render(&a, 140, 40);
        assert!(s.contains("1 update") && !s.contains("0 configs"), "{s}");
    }

    #[test]
    fn the_key_bar_offers_retry_only_after_an_unfinished_run() {
        let mut a = app();
        assert!(!render(&a, 160, 40).contains("Retry: t"));
        a.last_run = Some(("r1".into(), Default::default(), true));
        assert!(render(&a, 160, 40).contains("Retry: t"));
    }

    #[test]
    fn the_key_bar_leaves_a_gap_before_a_notice_and_drops_hints_that_do_not_fit() {
        let mut a = app();
        a.notify("Following AI agents: 2 items to install");
        let s = render(&a, 150, 40);
        let bar = s.lines().last().unwrap();
        assert!(bar.contains("  Following AI agents"), "{bar}");
        assert!(!bar.contains("qFollowing"), "{bar}");
        // Every hint shown is whole.
        let keys = bar.split("  Following").next().unwrap();
        for hint in keys.split(" | ") {
            assert!(hint.trim().contains(": "), "{bar}");
        }
    }

    #[test]
    fn small_terminals_keep_what_matters_readable() {
        for (w, h) in [(80, 24), (100, 30), (120, 35)] {
            let mut a = app();
            a.focus = Focus::Items;
            let s = render(&a, w, h);
            let lines: Vec<&str> = s.lines().collect();
            // The scope and its key, the main action and help are always there.
            assert!(
                lines[0].contains("new installs →") && lines[0].contains("(S →"),
                "{w}x{h}\n{s}"
            );
            let bar = lines.last().unwrap();
            for hint in ["Install/Update: enter", "Help: ?", "Quit: q"] {
                assert!(bar.contains(hint), "{w}x{h}: {hint:?} missing in {bar:?}");
            }
            // Names are whole and the last column's title is not cut.
            assert!(s.contains(" codex-sandbox-network"), "{w}x{h}\n{s}");
            assert!(s.contains("managed by / note"), "{w}x{h}\n{s}");
        }
    }

    #[test]
    fn help_scrolls_when_the_terminal_is_too_short() {
        let mut a = app();
        a.on_key(KeyEvent::new(KeyCode::Char('?'), KeyModifiers::NONE));
        let s = render(&a, 80, 24);
        assert!(s.contains("↓ more · any other key closes"), "{s}");
        assert!(!s.contains("Bundles  ● followed"), "{s}");
        for _ in 0..30 {
            a.on_key(KeyEvent::new(KeyCode::Down, KeyModifiers::NONE));
        }
        assert!(a.help, "arrows scroll, they do not close");
        let s = render(&a, 80, 24);
        assert!(
            s.contains("Bundles  ● followed") && s.contains("↑ more"),
            "{s}"
        );
        a.on_key(KeyEvent::new(KeyCode::Char('x'), KeyModifiers::NONE));
        assert!(!a.help && a.help_scroll == 0);
        // Tall enough: no scrolling, any key closes.
        a.on_key(KeyEvent::new(KeyCode::Char('?'), KeyModifiers::NONE));
        assert!(render(&a, 140, 60).contains("any key closes"));
        a.on_key(KeyEvent::new(KeyCode::Down, KeyModifiers::NONE));
        assert!(!a.help);
    }

    #[test]
    fn a_small_run_screen_keeps_cancel_and_cuts_outcomes_with_an_ellipsis() {
        let mut a = app();
        a.on_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
        let plan = a.plan.clone().unwrap().0;
        a.start_run(plan, "20261029-140500".into(), None);
        a.on_exec(crate::exec::Event::UnitStarted(0));
        let s = render(&a, 80, 24);
        let bar = s.lines().last().unwrap();
        assert!(
            bar.contains("Cancel running: c") && bar.contains("Help: ?"),
            "{bar}"
        );
        assert!(s.contains("running") && !s.contains("runnin│"), "{s}");
        a.on_exec(crate::exec::Event::UnitFinished(
            0,
            Outcome::Failed("exit 22 after a long and winding failure".into()),
            std::time::Duration::from_secs(3),
        ));
        let s = render(&a, 80, 24);
        assert!(s.contains("exit 22") && s.contains('…'), "{s}");
    }

    #[test]
    fn plan_and_run_screens_render() {
        let mut a = app();
        // Enter installs the selection, seeded with what the kept bundles miss.
        a.on_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
        let s = render(&a, 140, 40);
        assert!(
            s.contains("[2] Steps") && s.contains("codex") && s.contains("Run 4 steps: enter"),
            "{s}"
        );
        let plan = a.plan.clone().unwrap().0;
        a.start_run(plan, "20261029-140500".into(), None);
        a.on_exec(crate::exec::Event::UnitStarted(0));
        a.on_exec(crate::exec::Event::Line(
            0,
            "$ curl -fsSL https://fnm.vercel.app/install".into(),
        ));
        let s = render(&a, 140, 40);
        assert!(
            s.contains("[4] Log") && s.contains("fnm.vercel.app") && s.contains("running"),
            "{s}"
        );
        a.on_exec(crate::exec::Event::Line(
            0,
            "[envira] hint (network): a download failed on the network".into(),
        ));
        a.on_exec(crate::exec::Event::UnitFinished(
            0,
            Outcome::Failed("exit 22".into()),
            std::time::Duration::from_secs(2),
        ));
        a.on_exec(crate::exec::Event::Finished(Default::default()));
        let s = render(&a, 140, 40);
        assert!(
            s.contains("run finished")
                && s.contains("Retry unfinished")
                && s.contains("hint (network): a download failed"),
            "{s}"
        );
        render(&a, 60, 16);
        a.on_key(KeyEvent::new(KeyCode::Char('?'), KeyModifiers::NONE));
        let s = render(&a, 140, 40);
        assert!(
            s.contains("full-width log") && !s.contains("focus panel"),
            "{s}"
        );
        a.on_key(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE));
        assert!(!a.help);
        assert!(a.run.is_some(), "closing help keeps the run screen");
        // Zoomed: the log takes the whole width, the steps panel is gone.
        a.on_key(KeyEvent::new(KeyCode::Char('z'), KeyModifiers::NONE));
        let s = render(&a, 140, 40);
        assert!(s.contains("[4] Log") && !s.contains("[2] Steps"), "{s}");
        render(&a, 60, 16);
    }

    #[test]
    fn the_header_leaves_out_the_catalog_source() {
        let a = app();
        let s = render(&a, 100, 30);
        let first = s.lines().next().unwrap();
        let start = format!("envira {} · workstation · ", env!("CARGO_PKG_VERSION"));
        assert!(first.starts_with(&start), "{first}");
        assert!(!first.contains("catalog"), "{first}");
    }

    #[test]
    fn a_newer_envira_shows_in_the_header_and_u_updates_it() {
        let mut a = app();
        a.on_self_latest(Some("9.9.9".into()));
        let s = render(&a, 100, 30);
        let first = s.lines().next().unwrap();
        let start = format!(
            "envira {} → 9.9.9 · workstation · ",
            env!("CARGO_PKG_VERSION")
        );
        assert!(first.starts_with(&start), "{first}");
        assert!(
            s.lines()
                .last()
                .unwrap()
                .contains("envira 9.9.9 is out · U updates envira"),
            "{s}"
        );
        // Said once: the next check with the same answer stays quiet.
        a.notify("something else");
        a.on_self_latest(Some("9.9.9".into()));
        assert_eq!(a.toast_text(), Some("something else"));
        assert!(matches!(
            a.on_key(KeyEvent::new(KeyCode::Char('U'), KeyModifiers::SHIFT))
                .as_slice(),
            [Cmd::SelfUpdate]
        ));
        // A catalog warning on screen is not covered.
        a.warn("Catalog: x");
        a.on_self_latest(Some("10.0.0".into()));
        assert_eq!(a.toast_text(), Some("Catalog: x"));
        a.on_self_latest(None);
        let first = render(&a, 100, 30).lines().next().unwrap().to_string();
        assert!(!first.contains("→ 10.0.0"), "{first}");
        // Updated but not restarted: U does not download it again, checks stay quiet.
        a.self_updated = true;
        assert!(
            a.on_key(KeyEvent::new(KeyCode::Char('U'), KeyModifiers::SHIFT))
                .is_empty()
        );
        assert!(a.toast_text().unwrap().contains("restart it"));
        a.on_self_latest(Some("10.0.0".into()));
        assert_eq!(a.self_latest, None);
    }

    #[test]
    fn toasts_wider_than_the_key_bar_are_cut_not_dropped() {
        let mut a = app();
        a.notify(format!(
            "Catalog: using the embedded catalog: {}",
            "x".repeat(200)
        ));
        let s = render(&a, 100, 30);
        let last = s.lines().last().unwrap();
        assert!(last.starts_with("Catalog: using the embedded"), "{last}");
        assert!(last.trim_end().ends_with('…'), "{last}");
    }

    #[test]
    fn catalog_warnings_show_once_the_dashboard_opens() {
        let mut a = app();
        a.screen = Screen::Loading;
        a.toast = None;
        a.catalog_warnings = vec!["could not fetch x: timed out; using the cache".into()];
        let snap = a.snap.take().unwrap();
        a.on_snapshot(snap, None);
        assert_eq!(
            a.toast_text(),
            Some("Catalog: could not fetch x: timed out; using the cache")
        );
        // The update count arriving later does not cover it.
        a.on_latest(std::collections::BTreeMap::from([(
            "bat".to_string(),
            crate::latest::Latest::Version("99.0".into()),
        )]));
        assert!(a.toast_text().unwrap().starts_with("Catalog:"));
        a.catalog_warnings.push("second".into());
        assert_eq!(
            crate::tui::app::catalog_warning(&a.catalog_warnings).as_deref(),
            Some("Catalog: could not fetch x: timed out; using the cache (+1 more: envira doctor)")
        );
    }

    #[test]
    fn running_steps_show_their_retry_attempt() {
        let log = vec![
            "$ curl x".to_string(),
            "[envira] step 1/1 failed (network); retrying in 5.0s (attempt 2/3)".to_string(),
            "curl: (56) reset".to_string(),
        ];
        assert_eq!(retry_attempt(&log), Some("2/3"));
        assert_eq!(retry_attempt(&log[..1]), None);
    }

    #[test]
    fn log_lines_dim_times_and_headers() {
        let t = Theme::tokyo_night();
        let l = log_line(&t, "15:57:41 $ echo start");
        assert_eq!(l.spans[0].content, "15:57:41 ");
        assert_eq!(l.spans[1].content, "$ echo start");
        assert_eq!(l.spans[1].style.fg, Some(t.magenta));
        let l = log_line(&t, "# finished ok in 3.0s");
        assert_eq!(l.style.fg, Some(t.dim));
        let l = log_line(&t, "[envira] live line");
        assert_eq!(l.spans[1].content, "[envira] live line");
    }

    #[test]
    fn first_run_renders_the_dashboard() {
        let mut a = app();
        let mut snap = a.snap.clone().unwrap();
        snap.first_run = true;
        for i in &mut snap.items {
            i.desired = false;
        }
        a.screen = Screen::Loading;
        a.state = None;
        a.selected.clear();
        a.on_snapshot(snap, None);
        let s = render(&a, 140, 40);
        assert!(
            s.contains("Bundles")
                && s.contains("Nothing selected.")
                && s.contains("Install/Update: enter"),
            "{s}"
        );
        render(&a, 70, 20);
    }

    #[test]
    fn items_show_where_tools_live_and_would_go() {
        let mut a = app();
        let mut snap = a.snap.clone().unwrap();
        for i in &mut snap.items {
            match i.id.as_str() {
                "bat" => {
                    i.via = Some("apt".into());
                    i.scope = Some(Scope::System);
                    i.ownership = Some(Ownership::Managed);
                }
                "yazi" => {
                    i.via = None;
                    i.scope = Some(Scope::User);
                    i.ownership = Some(Ownership::External);
                    i.route = None;
                    i.path = Some("/home/user/.nvm/versions/node/bin/yazi".into());
                }
                "codex" => {
                    i.via = Some("npm".into());
                    i.scope = Some(Scope::User);
                }
                _ => {}
            }
        }
        a.on_snapshot(snap, None);
        a.filter = Filter::All;
        let s = render(&a, 180, 60);
        for needle in ["new installs → system (S → user)", "scope", "1 system"] {
            assert!(s.contains(needle), "{needle:?} missing in\n{s}");
        }
        let row = |a: &mut App, id: &str| {
            a.search = id.into();
            let s = render(a, 180, 60);
            s.lines()
                .find(|l| l.contains(&format!(" {id} ")))
                .unwrap_or_else(|| panic!("no {id} row in\n{s}"))
                .to_string()
        };
        let bat = row(&mut a, "bat");
        assert!(bat.contains("apt") && bat.contains("system"), "{bat}");
        let codex = row(&mut a, "codex");
        assert!(codex.contains("→ npm") && codex.contains("user"), "{codex}");
        let yazi = row(&mut a, "yazi");
        assert!(yazi.contains("other · ~/.nvm/versions/…"), "{yazi}");
        a.search.clear();
        a.prefer_system = false;
        let s = render(&a, 180, 60);
        assert!(s.contains("new installs → user (S → system)"), "{s}");
    }

    fn area_of(a: &App, h: &Hit) -> Rect {
        a.hits
            .borrow()
            .iter()
            .rev()
            .find(|(_, x)| x == h)
            .map(|(r, _)| *r)
            .unwrap_or_else(|| panic!("no {h:?} on screen"))
    }

    fn mouse(a: &mut App, kind: MouseEventKind, r: Rect) -> Vec<Cmd> {
        a.on_mouse(MouseEvent {
            kind,
            column: r.x + r.width / 2,
            row: r.y,
            modifiers: KeyModifiers::NONE,
        })
    }

    fn click(a: &mut App, h: Hit) -> Vec<Cmd> {
        render(a, 140, 40);
        let r = area_of(a, &h);
        mouse(a, MouseEventKind::Down(MouseButton::Left), r)
    }

    fn key(c: KeyCode) -> KeyEvent {
        KeyEvent::new(c, KeyModifiers::NONE)
    }

    #[test]
    fn a_row_click_moves_the_cursor_and_the_mark_selects() {
        let mut a = app();
        a.on_key(key(KeyCode::Left));
        let before = a.selected.clone();
        click(&mut a, Hit::ItemRow(2));
        assert_eq!((a.focus, a.cursor), (Focus::Items, 2));
        assert_eq!(a.selected, before, "a row click only moves");
        let id = a.rows()[3].id.clone();
        click(&mut a, Hit::ItemMark(3));
        assert_eq!(a.cursor, 3);
        assert_ne!(a.selected.contains(&id), before.contains(&id));
        click(&mut a, Hit::ItemMark(3));
        assert_eq!(a.selected.contains(&id), before.contains(&id));
    }

    #[test]
    fn a_bundle_mark_keeps_the_bundle() {
        let mut a = app();
        let i = a
            .bundle_ids()
            .iter()
            .position(|b| !a.subscribed(b))
            .unwrap();
        let b = a.bundle_ids()[i].clone();
        click(&mut a, Hit::BundleRow(i));
        assert_eq!((a.focus, a.bundle_cursor), (Focus::Bundles, i));
        assert!(!a.subscribed(&b));
        let cmds = click(&mut a, Hit::BundleMark(i));
        assert!(a.subscribed(&b));
        assert!(matches!(
            cmds[..],
            [Cmd::SaveBundles { .. }, Cmd::Refresh(_)]
        ));
    }

    #[test]
    fn following_a_bundle_shows_its_missing_items_as_wanted_at_once() {
        let mut a = app();
        let i = a
            .bundle_ids()
            .iter()
            .position(|b| !a.subscribed(b))
            .unwrap();
        let b = a.bundle_ids()[i].clone();
        let lacking: Vec<String> = a
            .catalog
            .bundle_items(&b)
            .into_iter()
            .filter(|id| {
                a.snap
                    .as_ref()
                    .and_then(|s| s.get(id))
                    .is_some_and(|it| it.label == Label::Available)
            })
            .collect();
        assert!(!lacking.is_empty(), "the fixture bundle {b} lacks nothing");
        let label = |a: &App, id: &str| a.snap.as_ref().unwrap().get(id).unwrap().label;
        click(&mut a, Hit::BundleMark(i));
        for id in &lacking {
            assert_eq!(label(&a, id), Label::Missing, "{id}");
        }
        click(&mut a, Hit::BundleMark(i));
        for id in &lacking {
            assert_eq!(label(&a, id), Label::Available, "{id}");
        }
    }

    #[test]
    fn one_selected_step_is_singular() {
        let mut a = app();
        a.selected = ["bat".to_string()].into();
        a.on_latest(std::collections::BTreeMap::from([(
            "bat".to_string(),
            crate::latest::Latest::Version("99.0".into()),
        )]));
        let s = render(&a, 140, 40);
        assert!(s.contains("Selected 1 · 1 step─"), "{s}");
    }

    #[test]
    fn rows_say_who_manages_a_tool_without_a_dash_for_an_unknown_route() {
        let mut a = app();
        let mut snap = a.snap.clone().unwrap();
        for i in &mut snap.items {
            match i.id.as_str() {
                "bat" => {
                    i.ownership = Some(Ownership::Adoptable);
                }
                "yazi" => {
                    i.ownership = Some(Ownership::External);
                    i.via = None;
                    i.path = Some(a.facts.home.join(".nvm/bin/yazi").display().to_string());
                }
                _ => {}
            }
        }
        a.on_snapshot(snap, None);
        a.filter = Filter::All;
        let s = render(&a, 180, 160);
        let row = |id: &str| {
            s.lines()
                .find(|l| l.contains(&format!(" {id} ")))
                .unwrap_or_default()
                .to_string()
        };
        assert!(row("bat").contains("not envira · A"), "{}", row("bat"));
        let yazi = row("yazi");
        assert!(
            yazi.contains("other · ~/.nvm/") && !yazi.contains('—'),
            "{yazi}"
        );
    }

    #[test]
    fn an_empty_list_names_the_filter_that_hides_items() {
        let mut a = app();
        a.filter = Filter::Outdated;
        a.search = "no-such-item".into();
        assert!(render(&a, 140, 40).contains("no items match (filter: Outdated)"));
        a.filter = Filter::All;
        let s = render(&a, 140, 40);
        assert!(
            s.contains("no items match") && !s.contains("(filter:"),
            "{s}"
        );
    }

    #[test]
    fn tabs_counters_and_the_scope_hint_click() {
        let mut a = app();
        click(&mut a, Hit::Filter(Filter::Failed));
        assert_eq!(a.filter, Filter::Failed);
        click(&mut a, Hit::Counter(Filter::Installed));
        assert_eq!((a.filter, a.focus), (Filter::Installed, Focus::Items));
        click(&mut a, Hit::Detail(DetailTab::Log));
        assert_eq!(a.detail, DetailTab::Log);
        click(&mut a, Hit::Key(key(KeyCode::Char('S'))));
        assert!(!a.prefer_system);
        // The hint sits where the header shows it.
        render(&a, 140, 40);
        let r = area_of(&a, &Hit::Key(key(KeyCode::Char('S'))));
        let s = render(&a, 140, 40);
        let row: String = s.lines().nth(r.y as usize).unwrap().chars().collect();
        let text: String = row
            .chars()
            .skip(r.x as usize)
            .take(r.width as usize)
            .collect();
        assert_eq!(text, " (S → system)");
    }

    #[test]
    fn a_name_in_to_install_jumps_to_its_row() {
        let mut a = app();
        a.filter = Filter::Installed;
        render(&a, 140, 40);
        let unit = a
            .hits
            .borrow()
            .iter()
            .find_map(|(_, h)| match h {
                Hit::Unit(id) if id == "codex" => Some(h.clone()),
                _ => None,
            })
            .expect("codex in To install");
        click(&mut a, unit);
        assert_eq!(a.current().unwrap().id, "codex");
        assert_eq!(a.focus, Focus::Items);
    }

    #[test]
    fn key_bar_clicks_press_keys_and_the_plan_runs_from_its_label() {
        let mut a = app();
        click(&mut a, Hit::Key(key(KeyCode::Enter)));
        assert_eq!(a.screen, Screen::Plan);
        click(&mut a, Hit::PlanRow(1));
        assert_eq!(a.plan_cursor, 1);
        let cmds = click(&mut a, Hit::Key(key(KeyCode::Enter)));
        assert!(matches!(&cmds[..], [Cmd::StartRun { .. }]), "{cmds:?}");
    }

    #[test]
    fn the_wheel_scrolls_the_view_not_the_cursor() {
        let mut a = app();
        a.on_key(key(KeyCode::Left));
        let first_row = |a: &App| {
            render(a, 140, 40);
            let r = area_of(a, &Hit::Panel(Panel::Items));
            a.hits
                .borrow()
                .iter()
                .find_map(|(x, h)| match h {
                    Hit::ItemRow(i) if x.y == r.y + 1 => Some(*i),
                    _ => None,
                })
                .unwrap()
        };
        assert_eq!(first_row(&a), 0);
        let items = area_of(&a, &Hit::Panel(Panel::Items));
        mouse(&mut a, MouseEventKind::ScrollDown, items);
        mouse(&mut a, MouseEventKind::ScrollDown, items);
        // The panel under the pointer scrolls; focus, cursor and selection stay.
        assert_eq!(first_row(&a), 6);
        assert_eq!((a.focus, a.cursor), (Focus::Bundles, 0));
        mouse(&mut a, MouseEventKind::ScrollUp, items);
        assert_eq!(first_row(&a), 3);
        // Moving the cursor brings it back into view.
        a.on_key(key(KeyCode::Right));
        a.on_key(key(KeyCode::Down));
        assert_eq!(first_row(&a), 1);
        // A row click after scrolling selects the row that was clicked.
        mouse(&mut a, MouseEventKind::ScrollDown, items);
        mouse(&mut a, MouseEventKind::ScrollDown, items);
        let r = first_row(&a);
        click(&mut a, Hit::ItemRow(r + 2));
        assert_eq!(a.cursor, r + 2);
        assert_eq!(first_row(&a), r, "clicking a visible row does not scroll");
    }

    #[test]
    fn a_click_closes_help_and_does_nothing_else() {
        let mut a = app();
        a.help = true;
        let cmds = click(&mut a, Hit::ItemMark(0));
        assert!(cmds.is_empty() && !a.help);
        assert_eq!(a.cursor, 0);
    }

    #[test]
    fn steps_click_but_stopping_a_run_stays_on_the_keyboard() {
        let mut a = app();
        a.on_key(key(KeyCode::Enter));
        let plan = a.plan.take().unwrap().0;
        a.run = Some(RunView::new(plan, "r1".into(), None));
        a.screen = Screen::Run;
        click(&mut a, Hit::StepRow(1));
        let run = a.run.as_ref().unwrap();
        assert_eq!((run.sel, run.follow), (1, false));
        click(&mut a, Hit::Key(key(KeyCode::Char('f'))));
        assert!(a.run.as_ref().unwrap().follow);
        let keys: Vec<Hit> = a.hits.borrow().iter().map(|(_, h)| h.clone()).collect();
        assert!(!keys.contains(&Hit::Key(key(KeyCode::Char('c')))));
        assert_eq!(bar_key("Cancel running", "c"), None);
        assert_eq!(bar_key("Select", "space"), Some(key(KeyCode::Char(' '))));
        assert_eq!(bar_key("Panels", "← →"), None);
    }
}
