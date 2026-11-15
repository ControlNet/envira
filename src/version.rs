//! Lenient version comparison for tool versions (`v1.2.3`, `0.24.0`, `1.93.0-nightly`, `2:9.1`)
//! and the package managers' own ordering of package versions.

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

/// How a package manager orders its versions.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Scheme {
    /// apt: dpkg's algorithm.
    Dpkg,
    /// dnf and zypper: rpm's `rpmvercmp` (`~` before anything, `^` after the base version).
    Rpm,
    /// pacman: libalpm's `alpm_pkg_vercmp`, rpm's older algorithm (`1.0a` older than `1.0`).
    Pacman,
}

impl Scheme {
    /// The scheme of a package method (`status::PACKAGE_METHODS`); dpkg's for others.
    pub fn of(method: &str) -> Scheme {
        match method {
            "dnf" | "zypper" => Scheme::Rpm,
            "pacman" => Scheme::Pacman,
            _ => Scheme::Dpkg,
        }
    }
}

/// Compares whole package versions, `[epoch:]version[-revision]`, the way the package manager
/// does: epochs numerically, then the version, then the revision (pacman compares revisions
/// only when both have one). `None` when either side does not start with a digit (`(none)`, an
/// error message).
pub fn compare_package(scheme: Scheme, a: &str, b: &str) -> Option<Ordering> {
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
    let cmp = |x: &str, y: &str| match scheme {
        Scheme::Dpkg => verrevcmp(x, y),
        Scheme::Rpm => rpmvercmp(x, y, false),
        Scheme::Pacman => rpmvercmp(x, y, true),
    };
    Some(ea.cmp(&eb).then_with(|| cmp(&va, &vb)).then_with(|| {
        if scheme == Scheme::Pacman && (ra.is_empty() || rb.is_empty()) {
            Ordering::Equal
        } else {
            cmp(&ra, &rb)
        }
    }))
}

/// rpm's `rpmvercmp` (`alpm`: libalpm's copy of an older one, without `~` and `^`, where a
/// letter left over is older and longer separators are newer). Alphanumeric segments in turn:
/// numbers numerically and newer than letters, letters by bytes.
fn rpmvercmp(a: &str, b: &str, alpm: bool) -> Ordering {
    if a == b {
        return Ordering::Equal;
    }
    let (a, b) = (a.as_bytes(), b.as_bytes());
    let (mut i, mut j) = (0, 0);
    let special = |c: u8| !alpm && (c == b'~' || c == b'^');
    while i < a.len() || j < b.len() {
        let (si, sj) = (i, j);
        while i < a.len() && !a[i].is_ascii_alphanumeric() && !special(a[i]) {
            i += 1;
        }
        while j < b.len() && !b[j].is_ascii_alphanumeric() && !special(b[j]) {
            j += 1;
        }
        let (ca, cb) = (a.get(i).copied(), b.get(j).copied());
        if !alpm {
            // `~` sorts before everything, the end included.
            if ca == Some(b'~') || cb == Some(b'~') {
                if ca != Some(b'~') {
                    return Ordering::Greater;
                }
                if cb != Some(b'~') {
                    return Ordering::Less;
                }
                i += 1;
                j += 1;
                continue;
            }
            // `^` sorts after the end (the base version) and before anything else.
            if ca == Some(b'^') || cb == Some(b'^') {
                if ca.is_none() {
                    return Ordering::Less;
                }
                if cb.is_none() {
                    return Ordering::Greater;
                }
                if ca != Some(b'^') {
                    return Ordering::Greater;
                }
                if cb != Some(b'^') {
                    return Ordering::Less;
                }
                i += 1;
                j += 1;
                continue;
            }
        }
        if ca.is_none() || cb.is_none() {
            break;
        }
        if alpm && i - si != j - sj {
            return (i - si).cmp(&(j - sj));
        }
        let num = a[i].is_ascii_digit();
        let seg = |s: &[u8], k: usize| -> usize {
            let mut e = k;
            while e < s.len()
                && (if num {
                    s[e].is_ascii_digit()
                } else {
                    s[e].is_ascii_alphabetic()
                })
            {
                e += 1;
            }
            e
        };
        let (ei, ej) = (seg(a, i), seg(b, j));
        // Different kinds: a number is newer than letters.
        if ej == j {
            return if num {
                Ordering::Greater
            } else {
                Ordering::Less
            };
        }
        let (mut x, mut y) = (&a[i..ei], &b[j..ej]);
        if num {
            while x.first() == Some(&b'0') {
                x = &x[1..];
            }
            while y.first() == Some(&b'0') {
                y = &y[1..];
            }
            if x.len() != y.len() {
                return x.len().cmp(&y.len());
            }
        }
        match x.cmp(y) {
            Ordering::Equal => {}
            o => return o,
        }
        i = ei;
        j = ej;
    }
    let (ra, rb) = (&a[i.min(a.len())..], &b[j.min(b.len())..]);
    if ra.is_empty() && rb.is_empty() {
        return Ordering::Equal;
    }
    if alpm {
        // A letter left over never beats the end.
        let alpha = |s: &[u8]| s.first().is_some_and(u8::is_ascii_alphabetic);
        if (ra.is_empty() && !alpha(rb)) || alpha(ra) {
            Ordering::Less
        } else {
            Ordering::Greater
        }
    } else if ra.is_empty() {
        Ordering::Less
    } else {
        Ordering::Greater
    }
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

/// [`is_newer`] for whole package versions of a package method ([`compare_package`]).
pub fn is_newer_package(method: &str, latest: &str, current: &str) -> bool {
    compare_package(Scheme::of(method), latest, current) == Some(Ordering::Greater)
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
        let newer = |a, b| is_newer_package("apt", a, b);
        assert!(newer("1.2.3-2", "1.2.3-1"));
        assert!(newer("1:0.9-1", "2.0-1"));
        assert!(!newer("1.2.3-1ubuntu1", "1.2.3-1ubuntu1"));
        assert!(newer("1.2.3-1ubuntu2", "1.2.3-1ubuntu1"));
        assert!(newer("1.0", "1.0~rc1"));
        assert!(newer("1.10-1", "1.9-1"));
        assert!(newer("1.0a-1", "1.0-1"));
        assert!(!newer("(none)", "1.0-1"));
        assert_eq!(
            compare_package(Scheme::Dpkg, "1.0-1", "01.0-1"),
            Some(Ordering::Equal)
        );
    }

    /// Expected orders from `rpm --eval '%{lua:print(rpm.vercmp(a, b))}'` (rpm 4.19) and
    /// pacman's `vercmp a b`.
    #[test]
    fn compares_package_versions_like_rpm_and_pacman() {
        use Ordering::*;
        let rpm = |a, b| compare_package(Scheme::Rpm, a, b).unwrap();
        assert_eq!(rpm("1.0.1-1", "1.0^git-1"), Greater);
        assert_eq!(rpm("1.0^git-1", "1.0-1"), Greater);
        assert_eq!(rpm("1.0~rc1-1", "1.0-1"), Less);
        assert_eq!(rpm("1.0~rc1-1", "1.0~rc2-1"), Less);
        assert_eq!(rpm("2.39.2-1.fc41", "2.39.1-3.fc41"), Greater);
        assert_eq!(rpm("1.10-1", "1.9-1"), Greater);
        assert_eq!(rpm("1.0a-1", "1.0-1"), Greater);
        assert_eq!(rpm("1.0.a-1", "1.0.1-1"), Less);
        assert_eq!(rpm("1.0_1-1", "1.0.1-1"), Equal);
        assert_eq!(rpm("1.0-2", "1.0-10"), Less);
        assert!(is_newer_package("dnf", "1.0.1-1", "1.0^git-1"));
        assert!(is_newer_package("zypper", "1.2.3-2.1", "1.2.3-1.1"));
        let pac = |a, b| compare_package(Scheme::Pacman, a, b).unwrap();
        assert_eq!(pac("1.0a-1", "1.0-1"), Less);
        assert_eq!(pac("1.0-2", "1.0-1"), Greater);
        assert_eq!(pac("1.0", "1.0-1"), Equal);
        assert_eq!(pac("1.0..1", "1.0.1"), Greater);
        assert_eq!(pac("1:1.0-1", "2.0-1"), Greater);
        assert_eq!(pac("6.12.1.arch1-1", "6.12.arch1-1"), Greater);
    }
}
