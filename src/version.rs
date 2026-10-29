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
}
