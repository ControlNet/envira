//! Machine-readable output (`--json`): one JSON document for queries, one object per line
//! (NDJSON, each with an `event` key) for runs, and an `error` object with a stable code when a
//! command fails.

use std::io::Write;

use serde::Serialize;
use serde_json::{Value, json};

use crate::exec::{Event, Outcome};
use crate::facts::Facts;
use crate::plan::{Plan, Unit};

/// An error with a stable code that `--json` reports (`unknown_item`, `run_locked`, ...).
#[derive(Debug)]
pub struct Coded {
    pub code: &'static str,
    pub message: String,
}

impl std::fmt::Display for Coded {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.message)
    }
}

impl std::error::Error for Coded {}

pub fn coded(code: &'static str, message: impl Into<String>) -> anyhow::Error {
    Coded {
        code,
        message: message.into(),
    }
    .into()
}

/// The code of an error: from a [`Coded`] anywhere in its chain, else `error`.
pub fn code_of(e: &anyhow::Error) -> &'static str {
    e.chain()
        .find_map(|c| c.downcast_ref::<Coded>())
        .map_or("error", |c| c.code)
}

pub fn error(e: &anyhow::Error) -> Value {
    json!({"event": "error", "code": code_of(e), "message": format!("{e:#}")})
}

/// Writes one compact JSON line to stdout. As with a run's text output, a closed stdout does not
/// stop envira: write errors are ignored.
pub fn emit(v: &Value) {
    let _ = writeln!(std::io::stdout(), "{v}");
}

/// Writes a document to stdout, pretty-printed. Unlike a run's events, a document that could not
/// be written fails the command: it is all the command does.
pub fn print<T: Serialize + ?Sized>(v: &T) -> anyhow::Result<()> {
    let mut out = std::io::stdout().lock();
    writeln!(out, "{}", serde_json::to_string_pretty(v)?)
        .and_then(|_| out.flush())
        .map_err(|e| anyhow::anyhow!("writing the output: {e}"))
}

pub fn plan(plan: &Plan, facts: &Facts) -> Value {
    json!({
        "units": plan.units,
        "skipped": plan
            .skipped
            .iter()
            .map(|(id, reason)| json!({"id": id, "reason": reason}))
            .collect::<Vec<_>>(),
        "needs_sudo": plan.needs_sudo(facts),
        "low_space": plan
            .low_space(facts)
            .iter()
            .map(|(dir, free)| json!({"dir": dir.display().to_string(), "free_bytes": free}))
            .collect::<Vec<_>>(),
    })
}

pub fn plan_event(p: &Plan, facts: &Facts) -> Value {
    let mut v = plan(p, facts);
    v["event"] = "plan".into();
    v
}

pub fn outcome(o: &Outcome) -> Value {
    match o {
        Outcome::Ok { version } => json!({"outcome": "ok", "version": version}),
        Outcome::Failed(e) => json!({"outcome": "failed", "error": e}),
        Outcome::Blocked(dep) => json!({"outcome": "blocked", "needs": dep}),
        Outcome::Cancelled => json!({"outcome": "cancelled"}),
        Outcome::Skipped => json!({"outcome": "skipped"}),
    }
}

/// A run's event as an NDJSON object; `units` are the plan's, which events refer to by index.
/// `None` for a line left out by `quiet` (envira's own `[envira]` lines are kept).
pub fn event(ev: &Event, units: &[Unit], quiet: bool) -> Option<Value> {
    Some(match ev {
        Event::UnitStarted(i) => {
            let u = &units[*i];
            json!({"event": "unit_started", "index": i, "id": u.id, "action": u.action,
                   "via": u.via, "scope": u.scope})
        }
        Event::Line(i, l) => {
            if quiet && !l.starts_with("[envira]") {
                return None;
            }
            json!({"event": "line", "index": i, "id": units[*i].id, "line": l})
        }
        Event::UnitFinished(i, o, d) => {
            let mut v = outcome(o);
            v["event"] = "unit_finished".into();
            v["index"] = (*i).into();
            v["id"] = units[*i].id.clone().into();
            v["seconds"] = json!((d.as_secs_f64() * 10.0).round() / 10.0);
            v
        }
        Event::ShellEnv(r) => shell_env(r),
        Event::Notice(m) => json!({"event": "notice", "message": m}),
        Event::Finished(rec) => {
            let ok = rec.units.iter().filter(|u| u.outcome == "ok").count();
            json!({
                "event": "finished",
                "ok": ok,
                "total": rec.units.len(),
                "run": rec,
                "unrecorded": rec.unrecorded,
            })
        }
    })
}

pub fn shell_env(r: &Result<Vec<String>, String>) -> Value {
    match r {
        Ok(files) => json!({"event": "shell_env", "updated": files}),
        Err(e) => json!({"event": "shell_env", "error": e}),
    }
}

pub fn notice(message: impl Into<String>) -> Value {
    json!({"event": "notice", "message": message.into()})
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    #[test]
    fn code_survives_context() {
        let e = coded("unknown_item", "unknown item \"x\"").context("planning");
        assert_eq!(code_of(&e), "unknown_item");
        assert_eq!(code_of(&anyhow::anyhow!("plain")), "error");
        let v = error(&e);
        assert_eq!(v["event"], "error");
        assert_eq!(v["code"], "unknown_item");
        assert_eq!(v["message"], "planning: unknown item \"x\"");
    }

    fn unit(id: &str) -> Unit {
        Unit {
            id: id.into(),
            kind: crate::plan::Kind::Tool,
            action: crate::plan::Action::Install,
            via: "cargo".into(),
            scope: crate::state::Scope::User,
            steps: vec![],
            verify: vec![],
            deps: vec![],
            reason: "requested".into(),
            expect: None,
            resumed: vec![],
            route: String::new(),
            entry: String::new(),
        }
    }

    #[test]
    fn events_carry_ids_and_outcomes() {
        let ids = vec![unit("bat"), unit("fd")];
        let v = event(&Event::UnitStarted(1), &ids, false).unwrap();
        assert_eq!(
            v,
            json!({"event": "unit_started", "index": 1, "id": "fd", "action": "install",
                   "via": "cargo", "scope": "user"})
        );
        let v = event(
            &Event::UnitFinished(
                0,
                Outcome::Ok {
                    version: Some("0.25.0".into()),
                },
                Duration::from_millis(1234),
            ),
            &ids,
            false,
        )
        .unwrap();
        assert_eq!(
            v,
            json!({"event": "unit_finished", "index": 0, "id": "bat", "outcome": "ok",
                   "version": "0.25.0", "seconds": 1.2})
        );
        let v = event(
            &Event::UnitFinished(1, Outcome::Blocked("bat".into()), Duration::ZERO),
            &ids,
            false,
        )
        .unwrap();
        assert_eq!(v["outcome"], "blocked");
        assert_eq!(v["needs"], "bat");
    }

    #[test]
    fn quiet_keeps_only_envira_lines() {
        let ids = vec![unit("bat")];
        assert!(event(&Event::Line(0, "Unpacking".into()), &ids, true).is_none());
        let v = event(&Event::Line(0, "[envira] hint x".into()), &ids, true).unwrap();
        assert_eq!(v["line"], "[envira] hint x");
        assert!(event(&Event::Line(0, "Unpacking".into()), &ids, false).is_some());
    }

    #[test]
    fn plan_skipped_are_objects() {
        let p = Plan {
            units: vec![],
            skipped: vec![("docker".into(), "already present".into())],
        };
        let v = plan_event(&p, &Facts::sample(std::path::Path::new("/home/user")));
        assert_eq!(v["event"], "plan");
        assert_eq!(
            v["skipped"],
            json!([{"id": "docker", "reason": "already present"}])
        );
        assert_eq!(v["needs_sudo"], false);
        assert_eq!(v["low_space"], json!([]));
    }
}
