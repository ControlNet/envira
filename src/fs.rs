//! `envira fs`: idempotent file edits used by config action templates.
//!
//! The first time envira modifies a file it copies the original into the backups dir
//! (`$ENVIRA_STATE_DIR/backups`, default `~/.local/state/envira/backups`), and every key it sets
//! records the previous value, so `envira fs restore` can undo exactly what envira did.

use std::collections::BTreeMap;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};

pub fn backups_dir() -> PathBuf {
    let state = std::env::var_os("ENVIRA_STATE_DIR")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("XDG_STATE_HOME").map(|p| PathBuf::from(p).join("envira")))
        .unwrap_or_else(|| {
            PathBuf::from(std::env::var_os("HOME").unwrap_or_default()).join(".local/state/envira")
        });
    state.join("backups")
}

/// Expands a leading `~/` with `$HOME` (template parameters are usually expanded already).
pub fn expand(p: &str) -> PathBuf {
    match p.strip_prefix("~/") {
        Some(rest) => PathBuf::from(std::env::var_os("HOME").unwrap_or_default()).join(rest),
        None => PathBuf::from(p),
    }
}

#[derive(Debug, Default, Serialize, Deserialize)]
struct Meta {
    path: String,
    existed: bool,
    /// Original symlink target when the path was a symlink.
    link: Option<String>,
    #[serde(default)]
    keys: BTreeMap<String, KeyRecord>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct KeyRecord {
    kind: String,
    /// Previous value (TOML literal for `toml`, raw text for `ini`, the whole line for
    /// `line`); `None` when the key was absent.
    prior: Option<String>,
}

pub struct Backups {
    dir: PathBuf,
}

impl Backups {
    pub fn new(dir: PathBuf) -> Self {
        Backups { dir }
    }

    pub fn from_env() -> Self {
        Backups::new(backups_dir())
    }

    fn slot(&self, path: &Path) -> PathBuf {
        self.dir.join(path.display().to_string().replace('/', "%"))
    }

    fn load(&self, path: &Path) -> Option<Meta> {
        let text = std::fs::read_to_string(self.slot(path).join("meta.toml")).ok()?;
        toml::from_str(&text).ok()
    }

    fn save(&self, meta: &Meta, path: &Path) -> Result<()> {
        let slot = self.slot(path);
        std::fs::create_dir_all(&slot)?;
        std::fs::write(slot.join("meta.toml"), toml::to_string_pretty(meta)?)?;
        Ok(())
    }

    /// Saves the original file once, before the first modification.
    fn ensure(&self, path: &Path) -> Result<Meta> {
        if let Some(m) = self.load(path) {
            return Ok(m);
        }
        let slot = self.slot(path);
        std::fs::create_dir_all(&slot).with_context(|| format!("creating {}", slot.display()))?;
        let lmeta = std::fs::symlink_metadata(path).ok();
        let mut meta = Meta {
            path: path.display().to_string(),
            existed: lmeta.is_some(),
            ..Default::default()
        };
        if let Some(m) = lmeta {
            if m.file_type().is_symlink() {
                meta.link = std::fs::read_link(path)
                    .ok()
                    .map(|p| p.display().to_string());
            } else if m.is_file() {
                std::fs::copy(path, slot.join("orig"))
                    .with_context(|| format!("backing up {}", path.display()))?;
            }
        }
        self.save(&meta, path)?;
        Ok(meta)
    }

    fn record_key(&self, path: &Path, key: &str, kind: &str, prior: Option<String>) -> Result<()> {
        let mut meta = self.ensure(path)?;
        meta.keys.entry(key.to_string()).or_insert(KeyRecord {
            kind: kind.into(),
            prior,
        });
        self.save(&meta, path)
    }
}

fn read_opt(path: &Path) -> Result<Option<String>> {
    match std::fs::read_to_string(path) {
        Ok(s) => Ok(Some(s)),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(e).with_context(|| format!("reading {}", path.display())),
    }
}

fn write_atomic(path: &Path, data: &str, mode: Option<u32>) -> Result<()> {
    let dir = path
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .unwrap_or(Path::new("."));
    std::fs::create_dir_all(dir).with_context(|| format!("creating {}", dir.display()))?;
    let keep_mode = std::fs::metadata(path)
        .ok()
        .map(|m| m.permissions().mode() & 0o7777);
    let tmp = dir.join(format!(
        ".{}.envira-tmp",
        path.file_name().and_then(|n| n.to_str()).unwrap_or("f")
    ));
    std::fs::write(&tmp, data).with_context(|| format!("writing {}", tmp.display()))?;
    if let Some(m) = mode.or(keep_mode) {
        std::fs::set_permissions(&tmp, std::fs::Permissions::from_mode(m))?;
    }
    std::fs::rename(&tmp, path).with_context(|| format!("replacing {}", path.display()))?;
    Ok(())
}

/// Whole-file content. Returns whether the file is (or now is) in the desired state.
pub fn write(
    b: &Backups,
    path: &Path,
    content: &str,
    mode: Option<u32>,
    check: bool,
) -> Result<bool> {
    let cur = read_opt(path)?;
    let mode_ok = mode.is_none_or(|m| {
        std::fs::metadata(path).is_ok_and(|md| md.permissions().mode() & 0o7777 == m)
    });
    let ok = cur.as_deref() == Some(content) && mode_ok;
    if check || ok {
        return Ok(ok);
    }
    b.ensure(path)?;
    write_atomic(path, content, mode)?;
    Ok(true)
}

fn markers(id: &str, comment: &str) -> (String, String) {
    (
        format!("{comment} >>> {id} >>>"),
        format!("{comment} <<< {id} <<<"),
    )
}

/// Marked block. `content = None` removes it.
pub fn block(
    b: &Backups,
    path: &Path,
    id: &str,
    comment: &str,
    content: Option<&str>,
    check: bool,
) -> Result<bool> {
    let (start, end) = markers(id, comment);
    let cur = read_opt(path)?.unwrap_or_default();
    let lines: Vec<&str> = cur.lines().collect();
    let s = lines.iter().position(|l| l.trim_end() == start);
    let e = s.and_then(|s| {
        lines[s..]
            .iter()
            .position(|l| l.trim_end() == end)
            .map(|p| s + p)
    });
    let existing: Option<String> = match (s, e) {
        (Some(s), Some(e)) => Some(lines[s + 1..e].join("\n")),
        _ => None,
    };
    let want = content.map(|c| c.trim_end_matches('\n').to_string());
    let ok = existing == want;
    if check || ok {
        return Ok(ok);
    }
    if s.is_some() && e.is_none() {
        bail!("{}: found `{start}` without `{end}`", path.display());
    }
    b.ensure(path)?;
    let mut out: Vec<String> = Vec::new();
    match (s, e) {
        (Some(s), Some(e)) => {
            out.extend(lines[..s].iter().map(|l| l.to_string()));
            if let Some(w) = &want {
                out.push(start.clone());
                out.extend(w.lines().map(String::from));
                out.push(end.clone());
            }
            out.extend(lines[e + 1..].iter().map(|l| l.to_string()));
        }
        _ => {
            out.extend(lines.iter().map(|l| l.to_string()));
            if let Some(w) = &want {
                if out.last().is_some_and(|l| !l.trim().is_empty()) {
                    out.push(String::new());
                }
                out.push(start.clone());
                out.extend(w.lines().map(String::from));
                out.push(end.clone());
            }
        }
    }
    let mut text = out.join("\n");
    if !text.is_empty() {
        text.push('\n');
    }
    write_atomic(path, &text, None)?;
    Ok(true)
}

fn parse_value(v: &str) -> toml_edit::Value {
    v.parse::<toml_edit::Value>()
        .unwrap_or_else(|_| toml_edit::Value::from(v))
}

fn canon(v: &toml_edit::Value) -> String {
    let mut v = v.clone();
    v.decor_mut().clear();
    v.to_string()
}

/// Sets a dotted key in a TOML file, keeping comments and formatting elsewhere.
pub fn toml_set(b: &Backups, path: &Path, key: &str, value: &str, check: bool) -> Result<bool> {
    let text = read_opt(path)?.unwrap_or_default();
    let mut doc: toml_edit::DocumentMut = text
        .parse()
        .with_context(|| format!("parsing {}", path.display()))?;
    let parts: Vec<&str> = key.split('.').collect();
    let want = parse_value(value);
    let cur = lookup(doc.as_table(), &parts).map(canon);
    let ok = cur.as_deref() == Some(canon(&want).as_str());
    if check || ok {
        return Ok(ok);
    }
    b.record_key(path, key, "toml", cur)?;
    let (last, parents) = parts.split_last().context("empty key")?;
    let mut table = doc.as_table_mut();
    for (i, p) in parents.iter().enumerate() {
        let item = table.entry(p).or_insert_with(|| {
            let mut t = toml_edit::Table::new();
            t.set_implicit(i + 1 < parents.len());
            toml_edit::Item::Table(t)
        });
        table = item
            .as_table_mut()
            .with_context(|| format!("{p} in {} is not a table", path.display()))?;
    }
    table.insert(last, toml_edit::value(want));
    write_atomic(path, &doc.to_string(), None)?;
    Ok(true)
}

fn lookup<'a>(t: &'a toml_edit::Table, parts: &[&str]) -> Option<&'a toml_edit::Value> {
    let (last, parents) = parts.split_last()?;
    let mut cur: &dyn toml_edit::TableLike = t;
    for p in parents {
        cur = cur.get(p)?.as_table_like()?;
    }
    cur.get(last)?.as_value()
}

fn remove_toml_key(doc: &mut toml_edit::DocumentMut, parts: &[&str]) {
    let Some((last, parents)) = parts.split_last() else {
        return;
    };
    let mut cur: &mut dyn toml_edit::TableLike = doc.as_table_mut();
    for p in parents {
        match cur.get_mut(p).and_then(|i| i.as_table_like_mut()) {
            Some(t) => cur = t,
            None => return,
        }
    }
    cur.remove(last);
}

fn ini_find(lines: &[String], section: &str, key: &str) -> (Option<usize>, Option<usize>, usize) {
    // (section header index, key line index, insertion index)
    let mut in_section = section.is_empty();
    let mut header = None;
    let mut insert = if section.is_empty() { 0 } else { lines.len() };
    let mut key_line = None;
    let mut last_in = None;
    for (i, l) in lines.iter().enumerate() {
        let t = l.trim();
        if t.starts_with('[') && t.ends_with(']') {
            if in_section && section.is_empty() {
                insert = last_in.map(|x| x + 1).unwrap_or(i);
            }
            in_section = &t[1..t.len() - 1] == section;
            if in_section {
                header = Some(i);
                last_in = Some(i);
            }
            continue;
        }
        if in_section {
            if !t.is_empty() {
                last_in = Some(i);
            }
            if let Some((k, _)) = t.split_once('=')
                && k.trim() == key
                && !t.starts_with('#')
                && !t.starts_with(';')
            {
                key_line = Some(i);
            }
        }
    }
    if !section.is_empty() && header.is_some() {
        insert = last_in.map(|x| x + 1).unwrap_or(lines.len());
    } else if section.is_empty() && insert == 0 {
        insert = last_in.map(|x| x + 1).unwrap_or(0);
    }
    (header, key_line, insert)
}

/// Sets `key = value` in an INI section (`""` = before the first section).
pub fn ini_set(
    b: &Backups,
    path: &Path,
    section: &str,
    key: &str,
    value: &str,
    check: bool,
) -> Result<bool> {
    let text = read_opt(path)?.unwrap_or_default();
    let mut lines: Vec<String> = text.lines().map(String::from).collect();
    let (header, key_line, insert) = ini_find(&lines, section, key);
    let cur = key_line.map(|i| {
        lines[i]
            .split_once('=')
            .map(|(_, v)| v.trim().to_string())
            .unwrap_or_default()
    });
    let ok = cur.as_deref() == Some(value);
    if check || ok {
        return Ok(ok);
    }
    b.record_key(path, &format!("{section}.{key}"), "ini", cur)?;
    let line = format!("{key} = {value}");
    match (header, key_line) {
        (_, Some(i)) => lines[i] = line,
        (Some(_), None) => lines.insert(insert, line),
        (None, None) if section.is_empty() => lines.insert(insert, line),
        (None, None) => {
            if lines.last().is_some_and(|l| !l.trim().is_empty()) {
                lines.push(String::new());
            }
            lines.push(format!("[{section}]"));
            lines.push(line);
        }
    }
    write_atomic(path, &(lines.join("\n") + "\n"), None)?;
    Ok(true)
}

fn remove_ini_key(path: &Path, section: &str, key: &str, prior: Option<&str>) -> Result<()> {
    let text = read_opt(path)?.unwrap_or_default();
    let mut lines: Vec<String> = text.lines().map(String::from).collect();
    if let (_, Some(i), _) = ini_find(&lines, section, key) {
        match prior {
            Some(p) => lines[i] = format!("{key} = {p}"),
            None => {
                lines.remove(i);
            }
        }
        write_atomic(path, &(lines.join("\n") + "\n"), None)?;
    }
    Ok(())
}

/// One whole line, found by `pattern`: replaced when it differs, else inserted after the first line
/// matching `after` (appended when it is empty or matches nothing). Recorded under the pattern as key.
pub fn line(
    b: &Backups,
    path: &Path,
    pattern: &str,
    text: &str,
    after: Option<&str>,
    check: bool,
) -> Result<bool> {
    let re = regex::Regex::new(pattern).with_context(|| format!("bad --match {pattern:?}"))?;
    let after = after
        .filter(|a| !a.is_empty())
        .map(|a| regex::Regex::new(a).with_context(|| format!("bad --after {a:?}")))
        .transpose()?;
    let text_now = read_opt(path)?.unwrap_or_default();
    let mut lines: Vec<String> = text_now.lines().map(String::from).collect();
    let found = lines.iter().position(|l| re.is_match(l));
    let ok = found.is_some_and(|i| lines[i] == text);
    if check || ok {
        return Ok(ok);
    }
    b.record_key(path, pattern, "line", found.map(|i| lines[i].clone()))?;
    match found {
        Some(i) => lines[i] = text.to_string(),
        None => {
            let at = after
                .and_then(|a| lines.iter().position(|l| a.is_match(l)))
                .map_or(lines.len(), |i| i + 1);
            lines.insert(at, text.to_string());
        }
    }
    write_atomic(path, &(lines.join("\n") + "\n"), None)?;
    Ok(true)
}

fn restore_line(path: &Path, pattern: &str, prior: Option<&str>) -> Result<()> {
    let re = regex::Regex::new(pattern)?;
    let text = read_opt(path)?.unwrap_or_default();
    let mut lines: Vec<String> = text.lines().map(String::from).collect();
    if let Some(i) = lines.iter().position(|l| re.is_match(l)) {
        match prior {
            Some(p) => lines[i] = p.to_string(),
            None => {
                lines.remove(i);
            }
        }
        write_atomic(path, &(lines.join("\n") + "\n"), None)?;
    }
    Ok(())
}

/// Symbolic link `link -> target`.
pub fn link(b: &Backups, target: &str, link: &Path, check: bool) -> Result<bool> {
    let ok = std::fs::symlink_metadata(link).is_ok_and(|m| m.file_type().is_symlink())
        && std::fs::read_link(link).is_ok_and(|t| t == Path::new(target));
    if check || ok {
        return Ok(ok);
    }
    b.ensure(link)?;
    if let Some(parent) = link.parent() {
        std::fs::create_dir_all(parent)?;
    }
    match std::fs::symlink_metadata(link) {
        Ok(m) if m.is_dir() => bail!("{} is a directory", link.display()),
        Ok(_) => std::fs::remove_file(link)?,
        Err(_) => {}
    }
    std::os::unix::fs::symlink(target, link)
        .with_context(|| format!("linking {}", link.display()))?;
    Ok(true)
}

/// Undoes envira's changes: one key (`key = Some`) or the whole file.
pub fn restore(b: &Backups, path: &Path, key: Option<&str>) -> Result<()> {
    let Some(mut meta) = b.load(path) else {
        bail!("no envira backup for {}", path.display());
    };
    if let Some(key) = key {
        let Some(rec) = meta.keys.remove(key) else {
            bail!("no envira record for key {key} in {}", path.display());
        };
        match rec.kind.as_str() {
            "toml" => {
                let text = read_opt(path)?.unwrap_or_default();
                let mut doc: toml_edit::DocumentMut = text.parse()?;
                let parts: Vec<&str> = key.split('.').collect();
                match &rec.prior {
                    Some(p) => {
                        let (last, parents) = parts.split_last().context("empty key")?;
                        let mut t = doc.as_table_mut();
                        for p in parents {
                            t = t
                                .entry(p)
                                .or_insert(toml_edit::table())
                                .as_table_mut()
                                .context("not a table")?;
                        }
                        t.insert(last, toml_edit::value(parse_value(p)));
                    }
                    None => remove_toml_key(&mut doc, &parts),
                }
                write_atomic(path, &doc.to_string(), None)?;
            }
            "ini" => {
                let (section, k) = key.rsplit_once('.').unwrap_or(("", key));
                remove_ini_key(path, section, k, rec.prior.as_deref())?;
            }
            "line" => restore_line(path, key, rec.prior.as_deref())?,
            other => bail!("unknown key record kind {other}"),
        }
        if meta.keys.is_empty() {
            // Nothing else recorded: forget the file so a later apply starts a fresh backup.
            let _ = std::fs::remove_dir_all(b.slot(path));
        } else {
            b.save(&meta, path)?;
        }
        return Ok(());
    }
    let slot = b.slot(path);
    if std::fs::symlink_metadata(path).is_ok() {
        std::fs::remove_file(path).with_context(|| format!("removing {}", path.display()))?;
    }
    if let Some(t) = &meta.link {
        std::os::unix::fs::symlink(t, path)?;
    } else if meta.existed {
        std::fs::copy(slot.join("orig"), path)
            .with_context(|| format!("restoring {}", path.display()))?;
    }
    let _ = std::fs::remove_dir_all(slot);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn setup() -> (tempfile::TempDir, Backups) {
        let d = tempfile::tempdir().unwrap();
        let b = Backups::new(d.path().join("backups"));
        (d, b)
    }

    #[test]
    fn write_is_idempotent_and_restorable() {
        let (d, b) = setup();
        let f = d.path().join("etc/sysctl.d/60-x.conf");
        assert!(!write(&b, &f, "a=1\n", Some(0o644), true).unwrap());
        assert!(write(&b, &f, "a=1\n", Some(0o644), false).unwrap());
        assert!(write(&b, &f, "a=1\n", Some(0o644), true).unwrap());
        restore(&b, &f, None).unwrap();
        assert!(
            !f.exists(),
            "file did not exist before, so restore removes it"
        );

        std::fs::write(&f, "old\n").unwrap();
        write(&b, &f, "new\n", None, false).unwrap();
        restore(&b, &f, None).unwrap();
        assert_eq!(std::fs::read_to_string(&f).unwrap(), "old\n");
    }

    #[test]
    fn block_insert_replace_remove() {
        let (d, b) = setup();
        let rc = d.path().join(".zshrc");
        std::fs::write(&rc, "export A=1\n").unwrap();
        block(&b, &rc, "envira", "#", Some(". ~/x\n"), false).unwrap();
        block(&b, &rc, "envira", "#", Some(". ~/x\n"), false).unwrap();
        let t = std::fs::read_to_string(&rc).unwrap();
        assert_eq!(
            t,
            "export A=1\n\n# >>> envira >>>\n. ~/x\n# <<< envira <<<\n"
        );
        assert!(block(&b, &rc, "envira", "#", Some(". ~/x"), true).unwrap());
        block(&b, &rc, "envira", "#", Some(". ~/y"), false).unwrap();
        assert_eq!(
            std::fs::read_to_string(&rc)
                .unwrap()
                .matches(">>> envira")
                .count(),
            1
        );
        block(&b, &rc, "envira", "#", None, false).unwrap();
        assert_eq!(std::fs::read_to_string(&rc).unwrap(), "export A=1\n\n");
    }

    #[test]
    fn toml_set_keeps_comments_and_reverts_key() {
        let (d, b) = setup();
        let f = d.path().join("config.toml");
        std::fs::write(&f, "# my settings\nmodel = \"x\"\n").unwrap();
        let key = "sandbox_workspace_write.network_access";
        assert!(!toml_set(&b, &f, key, "true", true).unwrap());
        toml_set(&b, &f, key, "true", false).unwrap();
        let t = std::fs::read_to_string(&f).unwrap();
        assert_eq!(
            t,
            "# my settings\nmodel = \"x\"\n\n[sandbox_workspace_write]\nnetwork_access = true\n"
        );
        assert!(toml_set(&b, &f, key, "true", true).unwrap());
        // Matches verify.sh's check_codex_network_access semantics: a real boolean.
        let parsed: toml::Table = t.parse().unwrap();
        assert_eq!(
            parsed["sandbox_workspace_write"]["network_access"].as_bool(),
            Some(true)
        );

        toml_set(&b, &f, "model", "y", false).unwrap();
        restore(&b, &f, Some(key)).unwrap();
        let t = std::fs::read_to_string(&f).unwrap();
        assert!(!t.contains("network_access"), "{t}");
        assert!(t.contains("model = \"y\""), "other keys untouched: {t}");
        restore(&b, &f, Some("model")).unwrap();
        assert!(
            std::fs::read_to_string(&f)
                .unwrap()
                .contains("model = \"x\"")
        );
    }

    #[test]
    fn toml_set_string_values() {
        let (d, b) = setup();
        let f = d.path().join("theme.toml");
        toml_set(&b, &f, "flavor.dark", "onedark", false).unwrap();
        assert_eq!(
            std::fs::read_to_string(&f).unwrap(),
            "[flavor]\ndark = \"onedark\"\n"
        );
        assert!(toml_set(&b, &f, "flavor.dark", "onedark", true).unwrap());
    }

    #[test]
    fn ini_set_update_insert_and_revert() {
        let (d, b) = setup();
        let f = d.path().join("x.ini");
        std::fs::write(&f, "[core]\neditor = vim\n\n[user]\nname = a\n").unwrap();
        ini_set(&b, &f, "core", "pager", "less", false).unwrap();
        ini_set(&b, &f, "user", "name", "b", false).unwrap();
        ini_set(&b, &f, "new", "k", "v", false).unwrap();
        let t = std::fs::read_to_string(&f).unwrap();
        assert_eq!(
            t,
            "[core]\neditor = vim\npager = less\n\n[user]\nname = b\n\n[new]\nk = v\n"
        );
        assert!(ini_set(&b, &f, "core", "pager", "less", true).unwrap());
        restore(&b, &f, Some("user.name")).unwrap();
        restore(&b, &f, Some("core.pager")).unwrap();
        let t = std::fs::read_to_string(&f).unwrap();
        assert!(t.contains("name = a") && !t.contains("pager"), "{t}");
    }

    #[test]
    fn line_replaces_or_inserts_and_reverts() {
        let (d, b) = setup();
        let f = d.path().join(".zshrc");
        std::fs::write(
            &f,
            "export ZSH=\"$HOME/.oh-my-zsh\"\nZSH_THEME=\"robbyrussell\"\nsource x\n",
        )
        .unwrap();
        let theme = "ZSH_THEME=\"mzz-ys\"";
        assert!(!line(&b, &f, "^ZSH_THEME=", theme, None, true).unwrap());
        assert!(line(&b, &f, "^ZSH_THEME=", theme, None, false).unwrap());
        assert!(line(&b, &f, "^ZSH_THEME=", theme, None, true).unwrap());
        // Absent: inserted after the anchor, or appended without one.
        line(
            &b,
            &f,
            "^DISABLE_UPDATE_PROMPT=",
            "DISABLE_UPDATE_PROMPT=true",
            Some("^export ZSH="),
            false,
        )
        .unwrap();
        line(
            &b,
            &f,
            "^export TERM=",
            "export TERM=xterm-256color",
            Some("^nomatch"),
            false,
        )
        .unwrap();
        let t = std::fs::read_to_string(&f).unwrap();
        assert_eq!(
            t,
            "export ZSH=\"$HOME/.oh-my-zsh\"\nDISABLE_UPDATE_PROMPT=true\nZSH_THEME=\"mzz-ys\"\nsource x\nexport TERM=xterm-256color\n"
        );
        // A second apply changes nothing and keeps the original value recorded.
        assert!(line(&b, &f, "^ZSH_THEME=", theme, None, false).unwrap());
        assert_eq!(std::fs::read_to_string(&f).unwrap(), t);
        restore(&b, &f, Some("^ZSH_THEME=")).unwrap();
        restore(&b, &f, Some("^DISABLE_UPDATE_PROMPT=")).unwrap();
        restore(&b, &f, Some("^export TERM=")).unwrap();
        let t = std::fs::read_to_string(&f).unwrap();
        assert_eq!(
            t,
            "export ZSH=\"$HOME/.oh-my-zsh\"\nZSH_THEME=\"robbyrussell\"\nsource x\n"
        );
        assert!(line(&b, &f, "(", "x", None, true).is_err());
    }

    #[test]
    fn link_and_restore_previous_file() {
        let (d, b) = setup();
        let l = d.path().join("bin/bat");
        std::fs::create_dir_all(l.parent().unwrap()).unwrap();
        std::fs::write(&l, "old").unwrap();
        assert!(!link(&b, "/usr/bin/batcat", &l, true).unwrap());
        link(&b, "/usr/bin/batcat", &l, false).unwrap();
        assert!(link(&b, "/usr/bin/batcat", &l, true).unwrap());
        restore(&b, &l, None).unwrap();
        assert_eq!(std::fs::read_to_string(&l).unwrap(), "old");
    }

    #[test]
    fn restore_without_backup_errors() {
        let (d, b) = setup();
        assert!(restore(&b, &d.path().join("nope"), None).is_err());
    }
}
