//! `envira catalog check`: structural validation of a merged catalog.

use std::collections::{BTreeMap, BTreeSet};

use super::load::Loaded;
use super::schema::{Catalog, Params, Query};
use crate::template::{BUILTINS, parse_pred, placeholders};

pub const SUPPORTED_SCHEMA: u32 = 1;

#[derive(Debug, Default)]
pub struct Report {
    pub errors: Vec<String>,
    pub warnings: Vec<String>,
}

impl Report {
    pub fn is_ok(&self) -> bool {
        self.errors.is_empty()
    }
}

struct Ctx<'a> {
    loaded: &'a Loaded,
    report: Report,
}

impl Ctx<'_> {
    fn err(&mut self, section: &str, id: &str, msg: impl AsRef<str>) {
        let origin = self.loaded.origin(section, id);
        self.report
            .errors
            .push(format!("{section}.{id} ({origin}): {}", msg.as_ref()));
    }
    fn warn(&mut self, section: &str, id: &str, msg: impl AsRef<str>) {
        let origin = self.loaded.origin(section, id);
        self.report
            .warnings
            .push(format!("{section}.{id} ({origin}): {}", msg.as_ref()));
    }
}

pub fn validate(loaded: &Loaded) -> Report {
    let cat = &loaded.catalog;
    let mut c = Ctx {
        loaded,
        report: Report::default(),
    };
    c.report.warnings.extend(loaded.warnings.iter().cloned());

    if let Some(s) = cat.schema
        && s > SUPPORTED_SCHEMA
    {
        c.report.errors.push(format!(
            "catalog schema {s} is newer than supported ({SUPPORTED_SCHEMA})"
        ));
    }
    if let Some(min) = &cat.min_envira
        && crate::version::is_newer(min, env!("CARGO_PKG_VERSION"))
    {
        c.report.errors.push(format!(
            "catalog needs envira >= {min}, this is {}",
            env!("CARGO_PKG_VERSION")
        ));
    }

    let ids: BTreeSet<&str> = cat
        .tools
        .keys()
        .chain(cat.configs.keys())
        .map(String::as_str)
        .collect();
    for id in cat.tools.keys() {
        if cat.configs.contains_key(id) {
            c.err("tools", id, "id is also used by a config");
        }
    }

    check_methods(&mut c, cat);
    check_actions(&mut c, cat);
    check_tools(&mut c, cat, &ids);
    check_configs(&mut c, cat, &ids);

    for (id, b) in &cat.bundles {
        for item in &b.items {
            if !ids.contains(item.as_str()) {
                c.err("bundles", id, format!("unknown item {item:?}"));
            }
        }
        for nested in &b.bundles {
            if !cat.bundles.contains_key(nested) {
                c.err("bundles", id, format!("unknown bundle {nested:?}"));
            } else if bundle_reaches(cat, nested, id, &mut Vec::new()) {
                c.err("bundles", id, format!("bundle cycle through {nested:?}"));
            }
        }
    }
    check_cycles(&mut c, cat);
    c.report
}

/// True when item `from` is `target` or depends on it, directly or through any install entry.
fn reaches(cat: &Catalog, from: &str, target: &str, seen: &mut Vec<String>) -> bool {
    if from == target {
        return true;
    }
    if seen.iter().any(|s| s == from) {
        return false;
    }
    seen.push(from.to_string());
    let deps: Vec<&String> = match (cat.tools.get(from), cat.configs.get(from)) {
        (Some(t), _) => t
            .depends
            .iter()
            .chain(t.install.iter().flat_map(|e| &e.depends))
            .collect(),
        (None, Some(c)) => c.depends.iter().collect(),
        _ => vec![],
    };
    deps.into_iter().any(|d| reaches(cat, d, target, seen))
}

/// True when bundle `from` includes `target`, directly or through nested bundles.
fn bundle_reaches(cat: &Catalog, from: &str, target: &str, seen: &mut Vec<String>) -> bool {
    if from == target {
        return true;
    }
    if seen.iter().any(|s| s == from) {
        return false;
    }
    seen.push(from.to_string());
    cat.bundles.get(from).is_some_and(|b| {
        b.bundles
            .iter()
            .any(|n| bundle_reaches(cat, n, target, seen))
    })
}

fn check_when(c: &mut Ctx, section: &str, id: &str, when: &[String]) {
    for w in when {
        if let Err(e) = parse_pred(w) {
            c.err(section, id, format!("when: {e}"));
        }
    }
}

/// Placeholders a raw command may use: builtins only.
fn check_raw(c: &mut Ctx, section: &str, id: &str, what: &str, cmd: &str) {
    for p in placeholders(cmd) {
        if !BUILTINS.contains(&p.as_str()) {
            c.err(
                section,
                id,
                format!("{what}: unknown placeholder {{{p}}} (raw commands only get builtins)"),
            );
        }
    }
}

fn check_regex(c: &mut Ctx, section: &str, id: &str, what: &str, re: &str) {
    let probe: String = {
        let mut s = re.to_string();
        for p in placeholders(re) {
            s = s.replace(&format!("{{{p}}}"), "x");
        }
        s
    };
    if let Err(e) = regex::Regex::new(&probe) {
        c.err(section, id, format!("{what}: invalid regex: {e}"));
    }
}

/// A query needs exactly one source; `json` only applies to HTTP responses.
fn check_query(c: &mut Ctx, section: &str, id: &str, what: &str, q: &Query) {
    match (&q.cmd, &q.http) {
        (Some(_), Some(_)) => c.err(section, id, format!("{what}: has both `cmd` and `http`")),
        (None, None) => c.err(section, id, format!("{what}: needs `cmd` or `http`")),
        _ => {}
    }
    if q.json.is_some() && q.http.is_none() {
        c.err(section, id, format!("{what}: `json` needs `http`"));
    }
    if q.cmd.is_some() && q.regex.is_none() {
        c.warn(
            section,
            id,
            format!("{what}: no `regex`; the generic version pattern is used"),
        );
    }
    if let Some(r) = &q.regex {
        check_regex(c, section, id, &format!("{what}.regex"), r);
    }
}

fn query_templates(q: Option<&Query>) -> Vec<&str> {
    q.map(|q| {
        [&q.cmd, &q.http, &q.regex]
            .into_iter()
            .filter_map(|s| s.as_deref())
            .collect()
    })
    .unwrap_or_default()
}

fn check_methods(c: &mut Ctx, cat: &Catalog) {
    for (id, m) in &cat.methods {
        check_when(c, "methods", id, &m.when);
        if m.install.is_none() {
            c.err("methods", id, "missing `install` template");
        }
        if let Some(q) = &m.installed {
            check_query(c, "methods", id, "installed", q);
        }
        if let Some(q) = &m.latest {
            check_query(c, "methods", id, "latest", q);
        }
        for e in &m.expects {
            if !cat.tools.contains_key(e) && !cat.configs.contains_key(e) {
                c.warn("methods", id, format!("expects unknown item {e:?}"));
            }
        }
    }
}

fn check_actions(c: &mut Ctx, cat: &Catalog) {
    for (id, a) in &cat.actions {
        if a.check.is_none() {
            c.err("actions", id, "missing `check` template");
        }
        if a.apply.is_none() {
            c.err("actions", id, "missing `apply` template");
        }
    }
}

/// Checks that every placeholder in `templates` is a builtin or supplied, and that every supplied
/// parameter is used.
fn check_params(
    c: &mut Ctx,
    section: &str,
    id: &str,
    what: &str,
    templates: &[&str],
    defaults: &Params,
    params: &Params,
) {
    let mut used = BTreeSet::new();
    for t in templates {
        for p in placeholders(t) {
            if !BUILTINS.contains(&p.as_str())
                && !params.contains_key(&p)
                && !defaults.contains_key(&p)
            {
                c.err(section, id, format!("{what}: missing parameter {{{p}}}"));
            }
            used.insert(p);
        }
    }
    for (k, v) in params {
        if !used.contains(k) {
            c.warn(
                section,
                id,
                format!("{what}: parameter {k:?} is not used by the template"),
            );
        }
        for s in strings(v) {
            for p in placeholders(s) {
                if !BUILTINS.contains(&p.as_str()) {
                    c.err(
                        section,
                        id,
                        format!(
                            "{what}: parameter {k:?} uses {{{p}}}; values may only use builtins"
                        ),
                    );
                }
            }
        }
    }
}

fn strings(v: &serde_json::Value) -> Vec<&str> {
    match v {
        serde_json::Value::String(s) => vec![s.as_str()],
        serde_json::Value::Array(a) => a.iter().flat_map(strings).collect(),
        serde_json::Value::Object(o) => o.values().flat_map(strings).collect(),
        _ => vec![],
    }
}

fn check_tools(c: &mut Ctx, cat: &Catalog, ids: &BTreeSet<&str>) {
    for (id, t) in &cat.tools {
        check_when(c, "tools", id, &t.when);
        for d in &t.depends {
            if !ids.contains(d.as_str()) {
                c.err("tools", id, format!("depends on unknown item {d:?}"));
            }
        }
        if let Some(det) = &t.detect {
            if let Some(cmd) = &det.cmd {
                check_raw(c, "tools", id, "detect.cmd", cmd);
            }
            if let Some(v) = &det.version {
                check_regex(c, "tools", id, "detect.version", v);
            }
        }
        match (t.install.is_empty(), t.manual.is_some()) {
            (true, false) => c.err("tools", id, "no install entries (or `manual`)"),
            (false, true) => c.err("tools", id, "both `manual` and install entries"),
            _ => {}
        }
        for (i, e) in t.install.iter().enumerate() {
            let what = format!("install[{i}]");
            check_when(c, "tools", id, &e.when);
            if let Some(q) = &e.latest {
                check_query(c, "tools", id, &format!("{what}.latest"), q);
            }
            for d in &e.depends {
                if !ids.contains(d.as_str()) {
                    c.err(
                        "tools",
                        id,
                        format!("{what}: depends on unknown item {d:?}"),
                    );
                }
            }
            for l in &e.lanes {
                if !crate::exec::LANES.contains(&l.as_str()) {
                    c.err(
                        "tools",
                        id,
                        format!(
                            "{what}: unknown lane {l:?} (one of {:?})",
                            crate::exec::LANES
                        ),
                    );
                }
            }
            match (&e.method, &e.run) {
                (Some(_), Some(_)) => {
                    c.err("tools", id, format!("{what}: has both `method` and `run`"))
                }
                (None, None) => c.err("tools", id, format!("{what}: needs `method` or `run`")),
                (None, Some(run)) => {
                    check_raw(c, "tools", id, &what, run);
                    if let Some(u) = &e.update {
                        check_raw(c, "tools", id, &format!("{what}.update"), u);
                    }
                    for t in query_templates(e.latest.as_ref()) {
                        check_raw(c, "tools", id, &format!("{what}.latest"), t);
                    }
                    for k in e.params.keys() {
                        c.err(
                            "tools",
                            id,
                            format!("{what}: unknown key {k:?} in a raw `run` entry"),
                        );
                    }
                }
                (Some(m), None) => match cat.methods.get(m) {
                    None => c.err("tools", id, format!("{what}: unknown method {m:?}")),
                    Some(method) => {
                        let mut tpls: Vec<&str> = Vec::new();
                        tpls.extend(method.install.as_deref());
                        tpls.extend(e.update.as_deref().or(method.update.as_deref()));
                        tpls.extend(query_templates(method.installed.as_ref()));
                        tpls.extend(query_templates(
                            e.latest.as_ref().or(method.latest.as_ref()),
                        ));
                        check_params(
                            c,
                            "tools",
                            id,
                            &format!("{what} ({m})"),
                            &tpls,
                            &method.defaults,
                            &e.params,
                        );
                        for x in &method.expects {
                            let direct = t.depends.iter().chain(&e.depends);
                            if !direct
                                .into_iter()
                                .any(|d| reaches(cat, d, x, &mut Vec::new()))
                            {
                                c.warn(
                                    "tools",
                                    id,
                                    format!("{what}: method {m:?} expects {x:?}; add depends = [{x:?}] if it is needed"),
                                );
                            }
                        }
                    }
                },
            }
        }
    }
}

fn check_configs(c: &mut Ctx, cat: &Catalog, ids: &BTreeSet<&str>) {
    for (id, cf) in &cat.configs {
        check_when(c, "configs", id, &cf.when);
        for d in &cf.depends {
            if !ids.contains(d.as_str()) {
                c.err("configs", id, format!("depends on unknown item {d:?}"));
            }
        }
        let raw = cf.check.is_some() || cf.apply.is_some() || cf.revert.is_some();
        if raw && !cf.steps.is_empty() {
            c.err(
                "configs",
                id,
                "use either `steps` or raw `check`/`apply`/`revert`, not both",
            );
            continue;
        }
        if raw {
            if cf.apply.is_none() {
                c.err("configs", id, "raw form needs `apply`");
            }
            if cf.check.is_none() {
                c.err("configs", id, "raw `apply` without `check` is not allowed");
            }
            for (what, cmd) in [
                ("check", &cf.check),
                ("apply", &cf.apply),
                ("revert", &cf.revert),
            ] {
                if let Some(cmd) = cmd {
                    check_raw(c, "configs", id, what, cmd);
                }
            }
            continue;
        }
        if cf.steps.is_empty() {
            c.err("configs", id, "no steps");
            continue;
        }
        let mut any_check = false;
        for (i, s) in cf.steps.iter().enumerate() {
            let what = format!("steps[{i}]");
            match (&s.use_, &s.run) {
                (Some(_), Some(_)) => {
                    c.err("configs", id, format!("{what}: has both `use` and `run`"))
                }
                (None, None) => c.err("configs", id, format!("{what}: needs `use` or `run`")),
                (None, Some(run)) => {
                    check_raw(c, "configs", id, &what, run);
                    if let Some(chk) = &s.check {
                        any_check = true;
                        check_raw(c, "configs", id, &what, chk);
                    }
                    if let Some(rev) = &s.revert {
                        check_raw(c, "configs", id, &what, rev);
                    }
                    for k in s.params.keys() {
                        c.err(
                            "configs",
                            id,
                            format!("{what}: unknown key {k:?} in a raw `run` step"),
                        );
                    }
                }
                (Some(a), None) => {
                    if s.check.is_some() || s.revert.is_some() {
                        c.err(
                            "configs",
                            id,
                            format!("{what}: `check`/`revert` come from action {a:?}"),
                        );
                    }
                    match cat.actions.get(a) {
                        None => c.err("configs", id, format!("{what}: unknown action {a:?}")),
                        Some(act) => {
                            any_check |= act.check.is_some();
                            let tpls: Vec<&str> = [&act.check, &act.apply, &act.revert]
                                .into_iter()
                                .flatten()
                                .map(String::as_str)
                                .collect();
                            check_params(
                                c,
                                "configs",
                                id,
                                &format!("{what} ({a})"),
                                &tpls,
                                &act.defaults,
                                &s.params,
                            );
                        }
                    }
                }
            }
        }
        if !any_check {
            c.err(
                "configs",
                id,
                "no step has a check, so its status cannot be detected",
            );
        }
    }
}

fn check_cycles(c: &mut Ctx, cat: &Catalog) {
    let mut graph: BTreeMap<&str, Vec<&str>> = BTreeMap::new();
    for (id, t) in &cat.tools {
        let deps = graph.entry(id).or_default();
        deps.extend(t.depends.iter().map(String::as_str));
        for e in &t.install {
            deps.extend(e.depends.iter().map(String::as_str));
        }
    }
    for (id, cf) in &cat.configs {
        graph
            .entry(id)
            .or_default()
            .extend(cf.depends.iter().map(String::as_str));
    }
    #[derive(Clone, Copy, PartialEq)]
    enum Mark {
        Visiting,
        Done,
    }
    fn visit<'a>(
        n: &'a str,
        g: &BTreeMap<&'a str, Vec<&'a str>>,
        marks: &mut BTreeMap<&'a str, Mark>,
        stack: &mut Vec<&'a str>,
        found: &mut Vec<String>,
    ) {
        match marks.get(n) {
            Some(Mark::Done) => return,
            Some(Mark::Visiting) => {
                let start = stack.iter().position(|x| *x == n).unwrap_or(0);
                let mut cyc: Vec<&str> = stack[start..].to_vec();
                cyc.push(n);
                found.push(cyc.join(" -> "));
                return;
            }
            None => {}
        }
        marks.insert(n, Mark::Visiting);
        stack.push(n);
        for d in g.get(n).map(Vec::as_slice).unwrap_or(&[]) {
            visit(d, g, marks, stack, found);
        }
        stack.pop();
        marks.insert(n, Mark::Done);
    }
    let mut marks = BTreeMap::new();
    let mut found = Vec::new();
    for n in graph.keys() {
        visit(n, &graph, &mut marks, &mut Vec::new(), &mut found);
    }
    for cyc in found {
        c.report.errors.push(format!("dependency cycle: {cyc}"));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::catalog::load::{Layer, Source, embedded_layer, load};

    fn check(extra: &str) -> Report {
        let templates = embedded_layer()
            .sources
            .into_iter()
            .filter(|s| s.name == "templates.toml")
            .collect();
        let base = Layer {
            label: "t".into(),
            sources: templates,
        };
        let extra = Layer {
            label: "x".into(),
            sources: vec![Source {
                name: "x.toml".into(),
                text: extra.into(),
            }],
        };
        validate(&load(&[base, extra]).unwrap())
    }

    fn has(v: &[String], needle: &str) -> bool {
        v.iter().any(|e| e.contains(needle))
    }

    #[test]
    fn embedded_catalog_is_valid() {
        let r = validate(&load(&[embedded_layer()]).unwrap());
        assert!(r.errors.is_empty(), "{:#?}", r.errors);
        assert!(r.warnings.is_empty(), "{:#?}", r.warnings);
    }

    #[test]
    fn unknown_dependency_and_method() {
        let r = check("[tools.a]\ndepends = [\"nope\"]\ninstall = [{ method = \"carg\" }]\n");
        assert!(has(&r.errors, "unknown item \"nope\""), "{:?}", r.errors);
        assert!(has(&r.errors, "unknown method \"carg\""), "{:?}", r.errors);
    }

    #[test]
    fn manual_tools_need_no_install_entries() {
        let r = check("[tools.a]\nmanual = \"see https://example.org\"\n[tools.b]\ndesc = \"x\"\n");
        assert_eq!(r.errors.len(), 1, "{:?}", r.errors);
        assert!(has(&r.errors, "tools.b"), "{:?}", r.errors);
        let r = check("[tools.a]\nmanual = \"x\"\ninstall = [{ run = \"true\" }]\n");
        assert!(
            has(&r.errors, "both `manual` and install entries"),
            "{:?}",
            r.errors
        );
    }

    #[test]
    fn unknown_lane() {
        let r = check("[tools.a]\ninstall = [{ run = \"x\", lanes = [\"npm\", \"gpu\"] }]\n");
        assert_eq!(r.errors.len(), 1, "{:?}", r.errors);
        assert!(has(&r.errors, "unknown lane \"gpu\""), "{:?}", r.errors);
    }

    #[test]
    fn missing_and_unused_params() {
        let r = check("[tools.a]\ninstall = [{ method = \"npm\", pkg = \"x\" }]\n");
        assert!(
            has(&r.errors, "missing parameter {package}"),
            "{:?}",
            r.errors
        );
        assert!(has(&r.warnings, "\"pkg\" is not used"), "{:?}", r.warnings);
    }

    #[test]
    fn expects_is_a_lint_only() {
        let r = check(
            "[tools.nodejs]\ninstall = [{ run = \"true\" }]\n[tools.a]\ninstall = [{ method = \"npm\", package = \"x\" }]\n",
        );
        assert!(r.errors.is_empty(), "{:?}", r.errors);
        assert!(has(&r.warnings, "expects \"nodejs\""), "{:?}", r.warnings);
        // Satisfied through a dependency of a dependency.
        let r = check(
            "[tools.nodejs]\ninstall = [{ run = \"true\" }]\n[tools.n]\ndepends=[\"nodejs\"]\ninstall = [{ run = \"true\" }]\n[tools.a]\ninstall = [{ method = \"npm\", package = \"x\", depends = [\"n\"] }]\n",
        );
        assert!(!has(&r.warnings, "tools.a"), "{:?}", r.warnings);
    }

    #[test]
    fn cycle_is_reported() {
        let r = check(
            "[tools.a]\ndepends=[\"b\"]\ninstall=[{run=\"true\"}]\n[tools.b]\ninstall=[{run=\"true\", }]\n[configs.c]\ndepends=[\"a\"]\ncheck=\"true\"\napply=\"true\"\n[tools.b.shell]\n",
        );
        assert!(r.errors.is_empty(), "{:?}", r.errors);
        let r = check(
            "[tools.a]\ndepends=[\"b\"]\ninstall=[{run=\"true\"}]\n[tools.b]\ninstall=[{run=\"true\", depends=[\"a\"]}]\n",
        );
        assert!(has(&r.errors, "dependency cycle"), "{:?}", r.errors);
    }

    #[test]
    fn raw_apply_needs_check() {
        let r = check("[configs.x]\napply = \"echo hi >> ~/.x\"\n");
        assert!(has(&r.errors, "without `check`"), "{:?}", r.errors);
        let r = check("[configs.y]\nsteps = [{ run = \"sysctl --system\" }]\n");
        assert!(has(&r.errors, "no step has a check"), "{:?}", r.errors);
    }

    #[test]
    fn raw_commands_only_get_builtins() {
        let r = check("[tools.a]\ninstall = [{ run = \"install {package}\" }]\n");
        assert!(
            has(&r.errors, "unknown placeholder {package}"),
            "{:?}",
            r.errors
        );
    }

    #[test]
    fn latest_and_update_are_checked() {
        let r = check(concat!(
            "[tools.a]\n",
            "install = [{ run = \"x\", update = \"up {pkg}\", latest = { cmd = \"c\", http = \"h\" } }]\n",
            "[tools.b]\n",
            "install = [{ run = \"x\", latest = { cmd = \"c\", json = \"v\", regex = \"(\" } }]\n",
            "[tools.c]\n",
            "install = [{ method = \"npm\", package = \"c\", latest = { http = \"https://x/{pkgname}\" } }]\n",
        ));
        assert!(
            has(&r.errors, "update: unknown placeholder {pkg}"),
            "{:?}",
            r.errors
        );
        assert!(
            has(&r.errors, "has both `cmd` and `http`"),
            "{:?}",
            r.errors
        );
        assert!(has(&r.errors, "`json` needs `http`"), "{:?}", r.errors);
        assert!(
            has(&r.errors, "latest.regex: invalid regex"),
            "{:?}",
            r.errors
        );
        assert!(
            has(&r.errors, "missing parameter {pkgname}"),
            "{:?}",
            r.errors
        );
    }

    #[test]
    fn bad_predicate_and_bundle_item() {
        let r = check(
            "[tools.a]\nwhen=[\"gpuu\"]\ninstall=[{run=\"true\"}]\n[bundles.b]\nitems=[\"zzz\"]\n",
        );
        assert!(has(&r.errors, "unknown predicate"), "{:?}", r.errors);
        assert!(has(&r.errors, "unknown item \"zzz\""), "{:?}", r.errors);
    }

    #[test]
    fn nested_bundles_flatten_and_cycles_fail() {
        let src = concat!(
            "[tools.a]\ninstall=[{run=\"true\"}]\n[tools.b]\ninstall=[{run=\"true\"}]\n",
            "[bundles.x]\nitems=[\"a\"]\n[bundles.y]\nitems=[\"b\",\"a\"]\nbundles=[\"x\"]\n",
            "[bundles.all]\nbundles=[\"y\",\"x\"]\n",
        );
        let r = check(src);
        assert!(r.errors.is_empty(), "{:?}", r.errors);
        let cat: Catalog = toml::from_str(src).unwrap();
        assert_eq!(cat.bundle_items("all"), vec!["b", "a"]);
        let r = check("[bundles.p]\nbundles=[\"q\"]\n[bundles.q]\nbundles=[\"p\", \"zz\"]\n");
        assert!(
            has(&r.errors, "bundle cycle through \"q\""),
            "{:?}",
            r.errors
        );
        assert!(has(&r.errors, "unknown bundle \"zz\""), "{:?}", r.errors);
    }
}
