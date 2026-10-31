//! Rendering (BC1: lazygit-style numbered panels with btop-style gauges).

use ratatui::Frame;
use ratatui::layout::{Alignment, Constraint, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, BorderType, Cell, Clear, Paragraph, Row, Table, Wrap};

use super::app::{App, DetailTab, Filter, Focus, Screen, UnitState};
use super::theme::Theme;
use crate::commands::fmt_duration;
use crate::exec::Outcome;
use crate::plan::{Action, Kind, Planner};
use crate::status::{ItemStatus, Label};

const SPINNER: [&str; 10] = ["⠋", "⠙", "⠹", "⠸", "⠼", "⠴", "⠦", "⠧", "⠇", "⠏"];

pub fn draw(f: &mut Frame, app: &App, t: &Theme) {
    let area = f.area();
    match app.screen {
        Screen::Loading => loading(f, area, app, t),
        Screen::Dashboard => dashboard(f, area, app, t),
        Screen::Plan => plan_view(f, area, app, t),
        Screen::Run => run_view(f, area, app, t),
    }
    if app.help {
        help(f, area, app, t);
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
        Label::Missing | Label::Failed => t.red,
        Label::New => t.cyan,
        Label::Drifted => t.yellow,
        Label::Outdated => t.blue,
        Label::Available | Label::NotApplicable => t.dim,
    }
}

fn keybar(f: &mut Frame, area: Rect, t: &Theme, keys: &[(&str, &str)], right: Option<Span>) {
    let mut spans = Vec::new();
    for (i, (name, key)) in keys.iter().enumerate() {
        if i > 0 {
            spans.push(Span::styled(" | ", t.s(t.dim)));
        }
        spans.push(Span::styled(format!("{name}: {key}"), t.s(t.cyan)));
    }
    let right = right.map(Line::from).unwrap_or_default();
    let rw = right.width() as u16;
    let [l, r] = Layout::horizontal([Constraint::Min(0), Constraint::Length(rw)]).areas(area);
    f.render_widget(Paragraph::new(Line::from(spans)), l);
    f.render_widget(Paragraph::new(right).alignment(Alignment::Right), r);
}

fn header(f: &mut Frame, area: Rect, app: &App, t: &Theme, what: Vec<Span>) {
    let mut left = vec![
        Span::styled(
            "envira",
            Style::new().fg(t.blue).add_modifier(Modifier::BOLD),
        ),
        Span::styled(
            format!(" {} · {} · ", env!("CARGO_PKG_VERSION"), app.facts.hostname),
            t.s(t.dim),
        ),
    ];
    left.extend(what);
    let right = if app.detecting {
        format!("{} detecting…", SPINNER[(app.tick % 10) as usize])
    } else {
        format!("catalog {}", app.loaded_layers.join(" + "))
    };
    // Long catalog paths keep their end and never push out the left side.
    let max = (area.width / 2) as usize;
    let n = right.chars().count();
    let right = if n + 1 > max && max > 1 {
        let tail: String = right.chars().skip(n + 2 - max).collect();
        format!("…{tail}")
    } else {
        right
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
    header(f, h, app, t, vec![Span::raw(app.facts.summary())]);
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
    let keyset: &[(&str, &str)] = if app.searching {
        &[("Filter", "type"), ("Keep", "enter"), ("Clear", "esc")]
    } else {
        &[
            ("Panels", "← →"),
            ("Select", "space"),
            ("Install", "enter"),
            ("Update", "u"),
            ("Search", "/"),
            ("Filter", "f"),
            ("Help", "?"),
            ("Quit", "q"),
        ]
    };
    keybar(f, keys, t, keyset, right_span);
}

fn overview(f: &mut Frame, area: Rect, app: &App, t: &Theme) {
    let block = pane(t, "Overview", false, vec![]);
    let inner = block.inner(area);
    f.render_widget(block, area);
    let Some(snap) = &app.snap else { return };
    let applicable = snap.count(|i| i.present || i.label != Label::NotApplicable);
    let present = snap.count(|i| i.present);
    let to_install = snap.to_install().len();
    let missing = snap.count(|i| i.label == Label::Missing);
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
    let cells: [(Line, Line); 4] = [
        (
            stat(t, present, t.green, format!("/ {applicable} installed")),
            bar(t, cols[0].width, &[(present, t.green)], applicable),
        ),
        (
            stat(
                t,
                to_install,
                t.cyan,
                format!("to install · {missing} missing {new} new"),
            ),
            bar(t, cols[1].width, &[(to_install, t.cyan)], applicable),
        ),
        (
            stat(
                t,
                failed.len(),
                t.red,
                match &app.last_run {
                    Some((_, left, _)) => format!("failed · last run left {} · t", left.len()),
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
                } else if outdated > 0 {
                    "outdated · u updates".into()
                } else {
                    "outdated".into()
                },
            ),
            bar(t, cols[3].width, &[(outdated, t.blue)], updatable.max(1)),
        ),
    ];
    for (c, (a, b)) in cols.iter().zip(cells) {
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
        vec![Span::styled("space keeps", t.s(t.dim))],
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
    // Keep the cursor visible when there are more bundles than rows.
    let skip = app
        .bundle_cursor
        .saturating_sub(inner.height.saturating_sub(1) as usize);
    f.render_widget(Paragraph::new(lines).scroll((skip as u16, 0)), inner);
}

fn selection(f: &mut Frame, area: Rect, app: &App, t: &Theme) {
    let targets: Vec<String> = app
        .catalog
        .item_ids()
        .into_iter()
        .filter(|id| app.selected.contains(id))
        .collect();
    let plan = (!targets.is_empty()).then(|| app.make_plan(&targets));
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
                        " → {} steps{}",
                        p.units.len(),
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
    let block = pane(t, "To install", false, extra);
    let inner = block.inner(area);
    f.render_widget(block, area);
    let Some(plan) = plan else {
        let lines = vec![
            Line::styled("Nothing selected.", t.s(t.dim)),
            Line::styled("space: select bundles, items", t.s(t.dim)),
            Line::styled("enter: install them", t.s(t.dim)),
        ];
        f.render_widget(Paragraph::new(lines), inner);
        return;
    };
    let mut spans: Vec<Span> = Vec::new();
    for u in &plan.units {
        let (g, c) = match u.action {
            Action::Install => ("+", t.cyan),
            Action::Update => ("↑", t.blue),
            Action::Apply => ("◇", t.magenta),
            Action::Revert => ("↺", t.yellow),
        };
        spans.push(Span::styled(format!("{g} "), t.s(c)));
        spans.push(Span::styled(
            format!("{}  ", u.id),
            t.s(if app.selected.contains(&u.id) {
                t.fg
            } else {
                t.dim
            }),
        ));
    }
    for (id, _) in plan.skipped.iter().filter(|(_, w)| w != "already present") {
        spans.push(Span::styled(format!("– {id}  "), t.s(t.dim)));
    }
    f.render_widget(
        Paragraph::new(Line::from(spans)).wrap(Wrap { trim: true }),
        inner,
    );
}

fn table(f: &mut Frame, area: Rect, app: &App, t: &Theme) {
    let mut tabs: Vec<Span> = Vec::new();
    for (i, fl) in Filter::ALL.iter().enumerate() {
        if i > 0 {
            tabs.push(Span::styled(" - ", t.s(t.dim)));
        }
        let st = if *fl == app.filter {
            Style::new().fg(t.focus).add_modifier(Modifier::BOLD)
        } else {
            t.s(t.dim)
        };
        tabs.push(Span::styled(fl.name(), st));
    }
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
    let offset = if app.cursor >= height {
        app.cursor + 1 - height
    } else {
        0
    };
    let head = Row::new([
        "",
        "",
        "name",
        "version",
        "latest",
        "status",
        "via",
        "owner / note",
    ])
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
                _ => it
                    .ownership
                    .map(|o| o.as_str().to_string())
                    .unwrap_or_default(),
            };
            let note_color = match (it.label, it.ownership) {
                (Label::Failed, _) => t.red,
                (_, Some(crate::status::Ownership::Adoptable)) => t.yellow,
                _ => t.dim,
            };
            let version = match (it.kind, it.method_version.as_ref().or(it.version.as_ref())) {
                (Kind::Config, _) => String::new(),
                (_, Some(v)) => v.clone(),
                _ => "—".into(),
            };
            let latest = match (&it.latest, it.present) {
                (Some(l), true) => Span::styled(
                    l.clone(),
                    t.s(if it.label == Label::Outdated {
                        t.blue
                    } else {
                        t.dim
                    }),
                ),
                _ => Span::raw(""),
            };
            let row = Row::new(vec![
                Cell::from(Span::styled(
                    if sel { "[x]" } else { "[ ]" },
                    t.s(if sel { t.magenta } else { t.dim }),
                )),
                Cell::from(Span::styled(
                    it.label.glyph(),
                    t.s(label_color(t, it.label)),
                )),
                Cell::from(Span::styled(
                    it.id.clone(),
                    t.s(if it.label == Label::NotApplicable {
                        t.dim
                    } else {
                        t.fg
                    }),
                )),
                Cell::from(version),
                Cell::from(latest),
                Cell::from(Span::styled(it.label.text(), t.s(label_color(t, it.label)))),
                Cell::from(Span::styled(
                    it.via.clone().unwrap_or_else(|| "—".into()),
                    t.s(t.dim),
                )),
                Cell::from(Span::styled(note, t.s(note_color))),
            ]);
            if i == app.cursor && app.focus == Focus::Items {
                row.style(t.cursor())
            } else {
                row
            }
        })
        .collect();
    let widths = [
        Constraint::Length(3),
        Constraint::Length(1),
        Constraint::Length(22),
        Constraint::Length(10),
        Constraint::Length(10),
        Constraint::Length(9),
        Constraint::Length(14),
        Constraint::Min(8),
    ];
    if rows.is_empty() {
        let msg = if app.snap.is_none() {
            "detecting…"
        } else {
            "no items match"
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
        tabs.push(Span::styled(*name, st));
    }
    tabs.push(Span::styled("  tab", t.s(t.dim)));
    let block = Block::bordered()
        .border_type(BorderType::Rounded)
        .border_style(t.s(t.border))
        .title(Line::from([vec![], tabs].concat()));
    let inner = block.inner(area);
    f.render_widget(block, area);
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
    if let Some(s) = it.scope {
        first.push(Span::styled(format!(" · {s}"), t.s(t.dim)));
    }
    if let Some(v) = &it.via {
        first.push(Span::styled(format!(" · via {v}"), t.s(t.dim)));
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
            Some(r) => spans.push(Span::styled(
                format!("  · updates via {} ({})", r.via, r.scope),
                t.s(t.dim),
            )),
            None => spans.push(Span::styled(
                "  · not updated by envira (installed elsewhere)",
                t.s(t.dim),
            )),
        }
        lines.push(Line::from(spans));
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
                format!("via {} · scope {}", c.via, c.scope),
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

fn last_log(app: &App, id: &str) -> Option<(String, Vec<String>)> {
    let st = app.state.as_ref()?;
    let run = st
        .tools
        .get(id)
        .and_then(|r| r.last_run.clone())
        .or_else(|| st.configs.get(id).and_then(|r| r.last_run.clone()))?;
    let text = std::fs::read_to_string(app.paths.run_dir(&run).join(format!("{id}.log"))).ok()?;
    Some((run, text.lines().map(String::from).collect()))
}

fn log_lines<'a>(app: &'a App, it: &'a ItemStatus, t: &Theme, height: usize) -> Vec<Line<'a>> {
    match last_log(app, &it.id) {
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
    let installs = plan
        .units
        .iter()
        .filter(|u| u.action == Action::Install)
        .count();
    let applies = plan
        .units
        .iter()
        .filter(|u| u.action == Action::Apply)
        .count();
    let reverts = plan
        .units
        .iter()
        .filter(|u| u.action == Action::Revert)
        .count();
    let block = panel(t, 1, "Plan", false, vec![]);
    let inner = block.inner(top);
    f.render_widget(block, top);
    let mut first = vec![
        Span::styled(
            format!("{n} {}  ", plural(n, "step", "steps")),
            Style::new().fg(t.fg).add_modifier(Modifier::BOLD),
        ),
        Span::styled(
            format!("{installs} {}  ", plural(installs, "install", "installs")),
            t.s(t.cyan),
        ),
        Span::styled(
            format!("{applies} {}  ", plural(applies, "config", "configs")),
            t.s(t.magenta),
        ),
    ];
    if reverts > 0 {
        first.push(Span::styled(
            format!("{reverts} {}  ", plural(reverts, "revert", "reverts")),
            t.s(t.yellow),
        ));
    }
    first.push(Span::styled("+ shell env", t.s(t.dim)));
    let sudo = if plan.needs_sudo(&app.facts) {
        Span::styled("! sudo needed: asked once before the run", t.s(t.yellow))
    } else {
        Span::styled("✓ sudo not needed", t.s(t.green))
    };
    let low = plan.low_space(&app.facts);
    let rest = if low.is_empty() {
        Span::styled(
            " · steps run in order · a failed step only blocks its dependents",
            t.s(t.dim),
        )
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
    let head = Row::new(["#", "action", "item", "via", "scope", "why"]).style(t.s(t.dim));
    let height = inner.height.saturating_sub(1) as usize;
    let offset = if app.plan_cursor >= height {
        app.plan_cursor + 1 - height
    } else {
        0
    };
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
            let row = Row::new(vec![
                Cell::from(Span::styled((i + 1).to_string(), t.s(t.dim))),
                Cell::from(Span::styled(u.action.as_str(), t.s(c))),
                Cell::from(u.id.clone()),
                Cell::from(Span::styled(u.via.clone(), t.s(t.dim))),
                Cell::from(Span::styled(u.scope.to_string(), t.s(t.dim))),
                Cell::from(Span::styled(u.reason.clone(), t.s(t.dim))),
            ]);
            if i == app.plan_cursor {
                row.style(t.cursor())
            } else {
                row
            }
        })
        .collect();
    let widths = [
        Constraint::Length(3),
        Constraint::Length(8),
        Constraint::Length(24),
        Constraint::Length(15),
        Constraint::Length(7),
        Constraint::Min(10),
    ];
    f.render_widget(
        Table::new(rows, widths).header(head).column_spacing(1),
        inner,
    );

    let [cmds, skipped] =
        Layout::horizontal([Constraint::Min(30), Constraint::Length(44)]).areas(bottom);
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
            Span::styled("cwd ", t.s(t.dim)),
            Span::raw("$ENVIRA_TMP (removed after) · "),
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

    keybar(
        f,
        keys,
        t,
        &[("Step", "↑↓"), ("Save as script", "c"), ("Back", "esc")],
        toast_span(app, t).or_else(|| {
            Some(Span::styled(
                format!("Run {n} steps: enter"),
                Style::new().fg(t.magenta).add_modifier(Modifier::BOLD),
            ))
        }),
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
    let offset = if run.sel >= height {
        run.sel + 1 - height
    } else {
        0
    };
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
            let row = Row::new(vec![
                Cell::from(Span::styled(g, t.s(gc))),
                Cell::from(u.id.clone()),
                Cell::from(Span::styled(u.action.as_str(), t.s(t.dim))),
                Cell::from(Span::styled(time, t.s(t.dim))),
                Cell::from(Span::styled(info, t.s(ic))),
            ]);
            if i == run.sel {
                row.style(t.cursor())
            } else {
                row
            }
        })
        .collect();
    let widths = [
        Constraint::Length(1),
        Constraint::Length(18),
        Constraint::Length(7),
        Constraint::Length(6),
        Constraint::Min(6),
    ];
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
                format!(" · {} via {} · {}", u.action.as_str(), u.via, u.scope),
                t.s(t.dim),
            ),
        ]));
        lines.push(Line::styled(u.reason.clone(), t.s(t.dim)));
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
            Ok(files) if files.is_empty() => Line::styled("shell env unchanged", t.s(t.dim)),
            Ok(files) => Line::styled(
                format!(
                    "shell env: {} files updated · open a new shell",
                    files.len()
                ),
                t.s(t.green),
            ),
            Err(e) => Line::styled(format!("shell env not updated: {e}"), t.s(t.red)),
        });
    }
    f.render_widget(Paragraph::new(lines).wrap(Wrap { trim: true }), inner);

    let unit = run.plan.units.get(run.sel);
    let block = panel(
        t,
        4,
        "Log",
        true,
        vec![
            Span::styled(
                format!("· {} ", unit.map(|u| u.id.as_str()).unwrap_or("")),
                t.s(t.dim),
            ),
            if run.follow {
                Span::styled("● follow", t.s(t.green))
            } else {
                Span::styled("○ follow", t.s(t.dim))
            },
        ],
    );
    let inner = block.inner(right);
    f.render_widget(block, right);
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
    let right_span = toast_span(app, t).or_else(|| {
        (run.finished.is_some() && failed + blocked + skipped > 0).then(|| {
            Span::styled(
                format!("Retry unfinished ({}): r", failed + blocked + skipped),
                Style::new().fg(t.magenta).add_modifier(Modifier::BOLD),
            )
        })
    });
    keybar(f, keys, t, keyset, right_span);
}

// ------------------------------------------------------------------------------------------------
// Help
// ------------------------------------------------------------------------------------------------

const DASHBOARD_KEYS: &[(&str, &str)] = &[
    ("← →  h l", "Bundles · Items"),
    ("↑↓  j k", "move"),
    ("space", "bundle: keep it and select what it misses"),
    ("space", "item: select or unselect it"),
    ("a  n", "select everything missing in view · nothing"),
    ("enter", "install the selection"),
    ("u", "update selected tools, else all outdated"),
    ("/  f", "search · cycle status filter"),
    ("tab  J K", "Info / Commands / Log · scroll it"),
    ("x", "revert selected configs"),
    ("t", "retry what the last run left unfinished"),
    ("R  r", "re-detect + check updates · reload catalog"),
    ("esc", "clear the search"),
    ("q", "quit"),
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
    let rows = help_rows(app.screen);
    let w = 58.min(area.width);
    let h = (rows.len() as u16 + 4).min(area.height);
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
    let lines: Vec<Line> = rows
        .iter()
        .map(|(k, v)| {
            Line::from(vec![
                Span::styled(format!("{k:>10}  "), t.s(t.cyan)),
                Span::raw(*v),
            ])
        })
        .chain([Line::raw(""), Line::styled("any key closes", t.s(t.dim))])
        .collect();
    f.render_widget(Paragraph::new(lines).block(block), r);
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tui::app::tests::app;
    use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
    use ratatui::Terminal;
    use ratatui::backend::TestBackend;

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
    fn dashboard_shows_panels_and_items() {
        let a = app();
        // Tall enough to show every catalog item.
        let s = render(&a, 140, 160);
        for needle in [
            "Overview",
            "Bundles",
            "To install",
            "Items",
            "Info - Commands - Log",
            "● Terminal",
            "◆",
            "herdr",
            "Panels: ← →",
            "Install: enter",
            "Update: u",
        ] {
            assert!(s.contains(needle), "missing {needle:?} in\n{s}");
        }
        assert!(s.contains("when gpu is not met"), "{s}");
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
        assert!(s.contains("latest") && s.contains("Update: u"), "{s}");
        assert!(s.contains("outdated · u updates"), "{s}");
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
    fn long_catalog_names_do_not_hide_the_header() {
        let mut a = app();
        a.loaded_layers = vec![format!(
            "explicit /{} (1 files)",
            "very/long/path".repeat(20)
        )];
        let s = render(&a, 100, 30);
        let first = s.lines().next().unwrap();
        let start = format!("envira {} · workstation · ", env!("CARGO_PKG_VERSION"));
        assert!(first.starts_with(&start), "{first}");
        assert!(
            first.contains("…") && first.ends_with("(1 files)"),
            "{first}"
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
                && s.contains("Install: enter"),
            "{s}"
        );
        render(&a, 70, 20);
    }
}
