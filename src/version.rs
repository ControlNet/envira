//! Lenient version comparison for tool versions (`v1.2.3`, `0.24.0`, `1.93.0-nightly`, `2:9.1`).

use std::cmp::Ordering;

/// Numeric components of a version: an optional leading `v` is dropped, then dot-separated
/// numbers are read until the first part that does not start with a digit. Each part
/// contributes its leading digits (`3-beta` → 3). `None` when there is no leading number.
pub fn parse(v: &str) -> Option<Vec<u64>> {
    let v = v.trim();
    let v = v.strip_prefix('v').unwrap_or(v);
    let mut out = Vec::new();
    for part in v.split('.') {
        let digits: String = part.chars().take_while(char::is_ascii_digit).collect();
        let Ok(n) = digits.parse::<u64>() else { break };
        out.push(n);
        if digits.len() != part.len() {
            break;
        }
    }
    (!out.is_empty()).then_some(out)
}

/// Compares two versions; missing trailing components count as 0. `None` when either side does
/// not parse.
pub fn compare(a: &str, b: &str) -> Option<Ordering> {
    let (mut a, mut b) = (parse(a)?, parse(b)?);
    let n = a.len().max(b.len());
    a.resize(n, 0);
    b.resize(n, 0);
    Some(a.cmp(&b))
}

/// True when `latest` is strictly newer than `current`; unknown versions are never outdated.
pub fn is_newer(latest: &str, current: &str) -> bool {
    compare(latest, current) == Some(Ordering::Greater)
}

/// Compares whole package versions, `[epoch:]version[-revision]`, the way dpkg does (rpm and
/// pacman order the usual versions the same): epochs numerically, then version and revision
/// part by part, digits numerically, `~` before anything (`1.0~rc1` is older than `1.0`).
/// `None` when either side does not start with a digit (`(none)`, an error message).
pub fn compare_package(a: &str, b: &str) -> Option<Ordering> {
    let split = |v: &str| -> Option<(u64, String, String)> {
        let v = v.trim();
        let (epoch, rest) = match v.split_once(':') {
            Some((e, r)) if !e.is_empty() && e.bytes().all(|c| c.is_ascii_digit()) => {
                (e.parse().ok()?, r)
            }
            _ => (0, v),
        };
        if !rest.starts_with(|c: char| c.is_ascii_digit()) {
            return None;
        }
        let (ver, rev) = rest.rsplit_once('-').unwrap_or((rest, ""));
        Some((epoch, ver.to_string(), rev.to_string()))
    };
    let (ea, va, ra) = split(a)?;
    let (eb, vb, rb) = split(b)?;
    Some(
        ea.cmp(&eb)
            .then_with(|| verrevcmp(&va, &vb))
            .then_with(|| verrevcmp(&ra, &rb)),
    )
}

/// dpkg's ordering of one version or revision string.
fn verrevcmp(a: &str, b: &str) -> Ordering {
    // Non-digits: `~` first, then the end, then letters, then the rest.
    let order = |c: Option<u8>| -> i32 {
        match c {
            None => 0,
            Some(b'~') => -1,
            Some(c) if c.is_ascii_digit() => 0,
            Some(c) if c.is_ascii_alphabetic() => c as i32,
            Some(c) => c as i32 + 256,
        }
    };
    let (a, b) = (a.as_bytes(), b.as_bytes());
    let (mut i, mut j) = (0, 0);
    while i < a.len() || j < b.len() {
        while (i < a.len() && !a[i].is_ascii_digit()) || (j < b.len() && !b[j].is_ascii_digit()) {
            let (ca, cb) = (order(a.get(i).copied()), order(b.get(j).copied()));
            if ca != cb {
                return ca.cmp(&cb);
            }
            i += 1;
            j += 1;
        }
        while a.get(i) == Some(&b'0') {
            i += 1;
        }
        while b.get(j) == Some(&b'0') {
            j += 1;
        }
        let mut first = Ordering::Equal;
        while i < a.len() && a[i].is_ascii_digit() && j < b.len() && b[j].is_ascii_digit() {
            if first == Ordering::Equal {
                first = a[i].cmp(&b[j]);
            }
            i += 1;
            j += 1;
        }
        if i < a.len() && a[i].is_ascii_digit() {
            return Ordering::Greater;
        }
        if j < b.len() && b[j].is_ascii_digit() {
            return Ordering::Less;
        }
        if first != Ordering::Equal {
            return first;
        }
    }
    Ordering::Equal
}

/// [`is_newer`] for whole package versions ([`compare_package`]).
pub fn is_newer_package(latest: &str, current: &str) -> bool {
    compare_package(latest, current) == Some(Ordering::Greater)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_common_shapes() {
        assert_eq!(parse("v1.2.3"), Some(vec![1, 2, 3]));
        assert_eq!(parse("0.24.0"), Some(vec![0, 24, 0]));
        assert_eq!(parse("1.95.0-nightly"), Some(vec![1, 95, 0]));
        assert_eq!(parse("26.13.0"), Some(vec![26, 13, 0]));
        assert_eq!(parse("1.2.3.4"), Some(vec![1, 2, 3, 4]));
        assert_eq!(parse("unknown"), None);
        assert_eq!(parse(""), None);
    }

    #[test]
    fn compares_numerically_with_any_length() {
        assert!(is_newer("0.10.0", "0.9.9"));
        assert!(is_newer("1.99.1", "1.93.0"));
        assert!(is_newer("1.2.3.5", "1.2.3.4"));
        assert!(!is_newer("1.2", "1.2.0"));
        assert!(!is_newer("v1.4.4", "1.4.4"));
        assert!(!is_newer("1.0.0", "1.0.1"));
        assert!(!is_newer("1.0.0", "garbage"));
        assert_eq!(compare("v1.2", "1.2.0"), Some(Ordering::Equal));
    }

    #[test]
    fn compares_package_versions_like_dpkg() {
        assert!(is_newer_package("1.2.3-2", "1.2.3-1"));
        assert!(is_newer_package("1:0.9-1", "2.0-1"));
        assert!(!is_newer_package("1.2.3-1ubuntu1", "1.2.3-1ubuntu1"));
        assert!(is_newer_package("1.2.3-1ubuntu2", "1.2.3-1ubuntu1"));
        assert!(is_newer_package("1.0", "1.0~rc1"));
        assert!(is_newer_package("2.39.2-1.fc41", "2.39.1-3.fc41"));
        assert!(is_newer_package("1.10-1", "1.9-1"));
        assert!(is_newer_package("1.0a-1", "1.0-1"));
        assert!(!is_newer_package("(none)", "1.0-1"));
        assert_eq!(compare_package("1.0-1", "01.0-1"), Some(Ordering::Equal));
    }
}
