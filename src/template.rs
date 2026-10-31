//! Template expansion (`{param}` placeholders with shell quoting) and `when` predicates.

use std::collections::BTreeMap;

use serde_json::Value;

use crate::catalog::Params;
use crate::facts::Facts;

/// Built-in placeholders available to every template and raw command.
pub const BUILTINS: &[&str] = &[
    "arch", "home", "user", "bin_dir", "sudo", "envira", "id", "bin",
];

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Ctx {
    /// Values are shell-quoted.
    Shell,
    /// Values are regex-escaped.
    Regex,
    /// Values are inserted verbatim (URLs); whitespace in a value is an error.
    Url,
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum Piece {
    Lit(String),
    Var(String),
}

fn parse(tpl: &str) -> Vec<Piece> {
    let chars: Vec<char> = tpl.chars().collect();
    let mut out = Vec::new();
    let mut lit = String::new();
    let mut i = 0;
    while i < chars.len() {
        let c = chars[i];
        if c == '{' && chars.get(i + 1) == Some(&'{') {
            lit.push('{');
            i += 2;
        } else if c == '}' && chars.get(i + 1) == Some(&'}') {
            lit.push('}');
            i += 2;
        } else if c == '$' && chars.get(i + 1) == Some(&'{') {
            // Shell parameter expansion: copy through the closing brace untouched.
            let end = chars[i..]
                .iter()
                .position(|&c| c == '}')
                .map(|p| i + p + 1)
                .unwrap_or(chars.len());
            lit.extend(&chars[i..end]);
            i = end;
        } else if c == '{' {
            let end = chars[i + 1..]
                .iter()
                .position(|&c| c == '}')
                .map(|p| i + 1 + p);
            let name: Option<String> = end.map(|e| chars[i + 1..e].iter().collect());
            match (end, name) {
                (Some(e), Some(n)) if is_ident(&n) => {
                    if !lit.is_empty() {
                        out.push(Piece::Lit(std::mem::take(&mut lit)));
                    }
                    out.push(Piece::Var(n));
                    i = e + 1;
                }
                _ => {
                    lit.push(c);
                    i += 1;
                }
            }
        } else {
            lit.push(c);
            i += 1;
        }
    }
    if !lit.is_empty() {
        out.push(Piece::Lit(lit));
    }
    out
}

fn is_ident(s: &str) -> bool {
    let mut cs = s.chars();
    matches!(cs.next(), Some(c) if c.is_ascii_alphabetic() || c == '_')
        && cs.all(|c| c.is_ascii_alphanumeric() || c == '_')
}

/// Placeholder names used by a template, in order of first use.
pub fn placeholders(tpl: &str) -> Vec<String> {
    let mut names = Vec::new();
    for p in parse(tpl) {
        if let Piece::Var(n) = p
            && !names.contains(&n)
        {
            names.push(n);
        }
    }
    names
}

/// Values for template expansion.
#[derive(Debug, Clone)]
pub struct Vars {
    /// Key of per-platform tables: the arch on Linux (`x86_64`), `<os>-<arch>` elsewhere
    /// (`macos-aarch64`), so a table of Linux assets has no value on a Mac.
    pub variant: String,
    builtins: BTreeMap<&'static str, String>,
    params: Params,
}

impl Vars {
    /// Builtins for an item; `bin_dir` and `sudo` depend on the chosen scope.
    pub fn new(facts: &Facts, id: &str, bin: &str, bin_dir: &str, system_scope: bool) -> Vars {
        let mut b = BTreeMap::new();
        b.insert("arch", facts.arch.clone());
        b.insert("home", facts.home.display().to_string());
        b.insert("user", facts.user.clone());
        b.insert("bin_dir", facts.expand_home(bin_dir));
        b.insert(
            "sudo",
            if system_scope && !facts.is_root() {
                "sudo -n".into()
            } else {
                String::new()
            },
        );
        b.insert("envira", envira_exe());
        b.insert("id", id.to_string());
        b.insert("bin", bin.to_string());
        Vars {
            variant: variant_key(facts),
            builtins: b,
            params: Params::new(),
        }
    }

    /// Adds parameters; later calls override earlier ones (defaults first, then the entry).
    pub fn with_params(mut self, params: &Params) -> Vars {
        for (k, v) in params {
            self.params.insert(k.clone(), v.clone());
        }
        self
    }

    /// Expands a template. Errors name the first unknown placeholder or a missing arch variant.
    pub fn expand(&self, tpl: &str, ctx: Ctx) -> Result<String, String> {
        let mut out = String::new();
        // An empty value (e.g. `{sudo}` in user scope) swallows one adjacent space.
        let mut skip_space = false;
        for piece in parse(tpl) {
            match piece {
                Piece::Lit(s) => {
                    let s = if skip_space && (out.is_empty() || out.ends_with(' ')) {
                        s.strip_prefix(' ').unwrap_or(&s).to_string()
                    } else {
                        s
                    };
                    skip_space = false;
                    out.push_str(&s);
                }
                Piece::Var(name) => {
                    let v = self.render(&name, ctx)?;
                    skip_space = v.is_empty() && ctx == Ctx::Shell;
                    out.push_str(&v);
                }
            }
        }
        if ctx == Ctx::Shell && skip_space {
            out.truncate(out.trim_end_matches(' ').len());
        }
        Ok(out)
    }

    fn render(&self, name: &str, ctx: Ctx) -> Result<String, String> {
        if let Some(v) = self.params.get(name) {
            let words = self.words(name, v)?;
            return Ok(match ctx {
                Ctx::Shell => words
                    .iter()
                    .map(|w| shell_quote(w))
                    .collect::<Vec<_>>()
                    .join(" "),
                Ctx::Regex => words
                    .iter()
                    .map(|w| regex::escape(w))
                    .collect::<Vec<_>>()
                    .join("|"),
                Ctx::Url => {
                    if words.len() != 1 || words[0].contains(char::is_whitespace) {
                        return Err(format!("{{{name}}} must be one word inside a URL"));
                    }
                    words[0].clone()
                }
            });
        }
        if let Some(v) = self.builtins.get(name) {
            return Ok(match ctx {
                Ctx::Shell if name == "sudo" || v.is_empty() => v.clone(),
                Ctx::Shell => shell_quote(v),
                Ctx::Regex => regex::escape(v),
                Ctx::Url => v.clone(),
            });
        }
        Err(format!("unknown placeholder {{{name}}}"))
    }

    /// A parameter value as a list of words with builtins and `~/` expanded.
    fn words(&self, name: &str, v: &Value) -> Result<Vec<String>, String> {
        match v {
            Value::String(s) => Ok(vec![self.expand_value(s)?]),
            Value::Bool(b) => Ok(vec![b.to_string()]),
            Value::Number(n) => Ok(vec![n.to_string()]),
            Value::Array(items) => {
                let mut out = Vec::new();
                for it in items {
                    out.extend(self.words(name, it)?);
                }
                Ok(out)
            }
            Value::Object(by_arch) => match by_arch.get(&self.variant) {
                Some(v) => self.words(name, v),
                None => Err(format!("{{{name}}} has no value for {}", self.variant)),
            },
            Value::Null => Ok(vec![]),
        }
    }

    fn expand_value(&self, s: &str) -> Result<String, String> {
        let mut out = String::new();
        for piece in parse(s) {
            match piece {
                Piece::Lit(l) => out.push_str(&l),
                Piece::Var(n) => match self.builtins.get(n.as_str()) {
                    Some(b) => out.push_str(b),
                    None => {
                        return Err(format!("unknown placeholder {{{n}}} in a parameter value"));
                    }
                },
            }
        }
        if out == "~" {
            out = self.builtins["home"].clone();
        } else if let Some(rest) = out.strip_prefix("~/") {
            out = format!("{}/{rest}", self.builtins["home"]);
        }
        Ok(out)
    }
}

pub fn variant_key(facts: &Facts) -> String {
    if facts.os == "linux" {
        facts.arch.clone()
    } else {
        format!("{}-{}", facts.os, facts.arch)
    }
}

/// Path of the running binary, used for `{envira}` (templates call `envira fs` / `fetch-release`).
pub fn envira_exe() -> String {
    if let Ok(p) = std::env::var("ENVIRA_EXE") {
        return p;
    }
    std::env::current_exe()
        .map(|p| exe_path(&p.display().to_string()))
        .unwrap_or_else(|_| "envira".into())
}

/// Linux reports a binary replaced while running (an upgrade) as `<path> (deleted)`; the new
/// binary is at `<path>`.
fn exe_path(p: &str) -> String {
    p.strip_suffix(" (deleted)").unwrap_or(p).to_string()
}

/// POSIX single-quote quoting; safe words are left bare for readable plans and logs.
pub fn shell_quote(s: &str) -> String {
    let safe = !s.is_empty()
        && s.chars()
            .all(|c| c.is_ascii_alphanumeric() || "@%+=:,./-_".contains(c));
    if safe {
        s.to_string()
    } else {
        format!("'{}'", s.replace('\'', r"'\''"))
    }
}

// ------------------------------------------------------------------------------------------------
// `when` predicates
// ------------------------------------------------------------------------------------------------

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Pred {
    Gpu,
    Container,
    Wsl,
    /// Root or sudo available.
    Sudo,
    Root,
    /// `/etc/os-release` ID.
    Distro(String),
    /// ID or one of ID_LIKE.
    Family(String),
    Arch(String),
    Shell(String),
    /// `linux` or `macos`.
    Os(String),
}

pub fn parse_pred(s: &str) -> Result<(bool, Pred), String> {
    let (neg, body) = match s.strip_prefix('!') {
        Some(rest) => (true, rest),
        None => (false, s),
    };
    let pred = match body.split_once(':') {
        None => match body {
            "gpu" => Pred::Gpu,
            "container" => Pred::Container,
            "wsl" => Pred::Wsl,
            "sudo" => Pred::Sudo,
            "root" => Pred::Root,
            _ => return Err(format!("unknown predicate {s:?}")),
        },
        Some((k, v)) if !v.is_empty() => match k {
            "distro" => Pred::Distro(v.to_lowercase()),
            "family" => Pred::Family(v.to_lowercase()),
            "arch" => Pred::Arch(v.to_string()),
            "shell" => Pred::Shell(v.to_string()),
            "os" => Pred::Os(v.to_lowercase()),
            _ => return Err(format!("unknown predicate {s:?}")),
        },
        Some(_) => return Err(format!("empty value in predicate {s:?}")),
    };
    Ok((neg, pred))
}

fn holds(p: &Pred, f: &Facts) -> bool {
    match p {
        Pred::Gpu => f.gpu,
        Pred::Container => f.container,
        Pred::Wsl => f.wsl,
        Pred::Sudo => f.privilege.can_elevate(),
        Pred::Root => f.is_root(),
        Pred::Distro(d) => f.distro_id == *d,
        Pred::Family(d) => f.families.iter().any(|x| x == d),
        Pred::Arch(a) => f.arch == *a,
        Pred::Shell(s) => f.has_shell(s),
        Pred::Os(o) => f.os == *o,
    }
}

/// The first predicate that does not hold, or `None` when all hold. Invalid predicates count as unmet.
pub fn unmet<'a>(when: &'a [String], facts: &Facts) -> Option<&'a str> {
    when.iter()
        .find(|w| match parse_pred(w) {
            Ok((neg, p)) => holds(&p, facts) == neg,
            Err(_) => true,
        })
        .map(String::as_str)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn replaced_binary_resolves_to_its_path() {
        assert_eq!(
            exe_path("/usr/local/bin/envira (deleted)"),
            "/usr/local/bin/envira"
        );
        assert_eq!(exe_path("/usr/local/bin/envira"), "/usr/local/bin/envira");
    }
    use serde_json::json;
    use std::path::Path;

    fn vars(params: Value) -> Vars {
        let facts = Facts::sample(Path::new("/home/user"));
        let p: Params = serde_json::from_value(params).unwrap();
        Vars::new(&facts, "yazi", "yazi", "~/.local/bin", false).with_params(&p)
    }

    #[test]
    fn quotes_and_expands_builtins_in_values() {
        let v = vars(
            json!({"asset": "yazi-{arch}-unknown-linux-musl.zip", "bins": ["yazi", "ya"], "repo": "sxyazi/yazi"}),
        );
        let out = v
            .expand(
                "{sudo} fetch --repo {repo} --asset {asset} --bin {bins} --to {bin_dir}",
                Ctx::Shell,
            )
            .unwrap();
        assert_eq!(
            out,
            "fetch --repo sxyazi/yazi --asset yazi-x86_64-unknown-linux-musl.zip --bin yazi ya --to /home/user/.local/bin"
        );
    }

    #[test]
    fn glob_and_spaces_are_quoted() {
        let v = vars(json!({"asset": "bat-v*-x.tar.gz", "content": "a b'c\n"}));
        assert_eq!(
            v.expand("{asset}", Ctx::Shell).unwrap(),
            "'bat-v*-x.tar.gz'"
        );
        assert_eq!(v.expand("{content}", Ctx::Shell).unwrap(), "'a b'\\''c\n'");
    }

    #[test]
    fn arch_table_selects_variant() {
        let v = vars(json!({"asset": {"x86_64": "a-x64.zip", "aarch64": "a-arm.zip"}}));
        assert_eq!(v.expand("{asset}", Ctx::Shell).unwrap(), "a-x64.zip");
        let v = vars(json!({"asset": {"aarch64": "a-arm.zip"}}));
        assert!(
            v.expand("{asset}", Ctx::Shell)
                .unwrap_err()
                .contains("no value for x86_64")
        );
    }

    #[test]
    fn shell_vars_and_escaped_braces_are_literal() {
        let v = vars(json!({"package": "bat"}));
        assert_eq!(
            v.expand(
                "dpkg-query -W -f='${Version}' {package}; rpm --qf '%{{VERSION}}'",
                Ctx::Shell
            )
            .unwrap(),
            "dpkg-query -W -f='${Version}' bat; rpm --qf '%{VERSION}'"
        );
        assert_eq!(placeholders("${HOME} {a} \\d{2} {b} {a}"), vec!["a", "b"]);
    }

    #[test]
    fn regex_context_escapes() {
        let v = vars(json!({"package": "@openai/codex"}));
        assert_eq!(
            v.expand("{package}@(\\S+)", Ctx::Regex).unwrap(),
            "@openai/codex@(\\S+)"
        );
        let v = vars(json!({"package": "a.b"}));
        assert_eq!(v.expand("^{package} v", Ctx::Regex).unwrap(), "^a\\.b v");
    }

    #[test]
    fn tilde_and_unknown() {
        let v = vars(json!({"file": "~/.codex/config.toml", "value": true}));
        assert_eq!(
            v.expand("{file} {value}", Ctx::Shell).unwrap(),
            "/home/user/.codex/config.toml true"
        );
        assert!(v.expand("{nope}", Ctx::Shell).is_err());
    }

    #[test]
    fn variant_tables_are_per_os() {
        let mut facts = Facts::sample(Path::new("/Users/user"));
        facts.os = "macos".into();
        facts.arch = "aarch64".into();
        let p = |v: Value| -> Params { serde_json::from_value(v).unwrap() };
        // A table with only Linux (plain arch) keys has nothing for a Mac: the entry is not
        // feasible there, rather than downloading a Linux binary.
        let linux_only = p(json!({"asset": {"x86_64": "a-linux-x64", "aarch64": "a-linux-arm"}}));
        let v = Vars::new(&facts, "a", "a", "~/.local/bin", false).with_params(&linux_only);
        let err = v.expand("{asset}", Ctx::Shell).unwrap_err();
        assert!(err.contains("macos-aarch64"), "{err}");
        let both = p(json!({"asset": {"aarch64": "a-linux-arm", "macos-aarch64": "a-darwin-arm"}}));
        let v = Vars::new(&facts, "a", "a", "~/.local/bin", false).with_params(&both);
        assert_eq!(v.expand("{asset}", Ctx::Shell).unwrap(), "a-darwin-arm");
        // Linux keeps reading the plain arch key.
        let f = Facts::sample(Path::new("/home/user"));
        let v = Vars::new(&f, "a", "a", "~/.local/bin", false).with_params(&both);
        assert!(
            v.expand("{asset}", Ctx::Shell).is_err(),
            "x86_64 has no key"
        );
        let mut f = f;
        f.arch = "aarch64".into();
        let v = Vars::new(&f, "a", "a", "~/.local/bin", false).with_params(&both);
        assert_eq!(v.expand("{asset}", Ctx::Shell).unwrap(), "a-linux-arm");
    }

    #[test]
    fn sudo_only_for_system_scope_when_not_root() {
        let facts = Facts::sample(Path::new("/home/user"));
        let v = Vars::new(&facts, "x", "x", "/usr/local/bin", true);
        assert_eq!(v.expand("{sudo} apt", Ctx::Shell).unwrap(), "sudo -n apt");
    }

    #[test]
    fn predicates() {
        let mut f = Facts::sample(Path::new("/home/user"));
        let w = |v: &[&str]| v.iter().map(|s| s.to_string()).collect::<Vec<_>>();
        assert_eq!(
            unmet(&w(&["family:debian", "!gpu", "arch:x86_64"]), &f),
            None
        );
        assert_eq!(unmet(&w(&["distro:debian"]), &f), Some("distro:debian"));
        assert_eq!(unmet(&w(&["gpu"]), &f), Some("gpu"));
        f.gpu = true;
        assert_eq!(unmet(&w(&["gpu"]), &f), None);
        assert_eq!(unmet(&w(&["os:linux"]), &f), None);
        assert_eq!(unmet(&w(&["os:macos"]), &f), Some("os:macos"));
        f.os = "macos".into();
        assert_eq!(unmet(&w(&["!os:linux"]), &f), None);
        assert!(parse_pred("colour").is_err());
        assert!(parse_pred("distro:").is_err());
    }
}
