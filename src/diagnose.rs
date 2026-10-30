//! Recognises common causes in the output of a failed step, to tell the user what to do and to
//! decide whether retrying the step can help.

use regex::Regex;
use std::sync::OnceLock;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Diagnosis {
    /// Short name, e.g. `network`.
    pub kind: &'static str,
    pub hint: &'static str,
    /// Retrying the same step may succeed (network hiccup, a lock held by another process).
    pub transient: bool,
}

struct Rule {
    re: &'static str,
    kind: &'static str,
    hint: &'static str,
    transient: bool,
}

/// First match wins, so specific causes come before generic ones.
const RULES: &[Rule] = &[
    Rule {
        re: r"(?i)API rate limit exceeded|HTTP 403 from GitHub|rate limit",
        kind: "rate limit",
        hint: "GitHub's API rate limit is used up; set GITHUB_TOKEN (any read-only token) or wait an hour",
        transient: false,
    },
    Rule {
        re: r"(?i)No space left on device|not enough free space|ENOSPC",
        kind: "disk full",
        hint: "the disk is full; free some space (`df -h`, `ncdu`) and retry",
        transient: false,
    },
    Rule {
        re: r"Could not get lock /var/lib/(dpkg|apt)|Unable to acquire the dpkg frontend lock|is another process using it\?|waiting for cache lock",
        kind: "package manager busy",
        hint: "another package manager (often unattended-upgrades) holds the lock; envira retries, or wait and run `envira retry`",
        transient: true,
    },
    Rule {
        re: r"externally-managed-environment",
        kind: "PEP 668",
        hint: "the system Python refuses pip installs; use pipx/uv, or PIP_BREAK_SYSTEM_PACKAGES=1 for this tool",
        transient: false,
    },
    Rule {
        re: r"curl: \((5|6|7|16|18|28|35|52|55|56|92)\)|Temporary failure in name resolution|Could not resolve host|Connection reset by peer|Connection timed out|Connection refused|Network is unreachable|TLS handshake timeout|unexpected EOF|error sending request|operation timed out|The requested URL returned error: 5\d\d|HTTP 5\d\d|HTTP 429|502 Bad Gateway|503 Service Unavailable|504 Gateway Time-?out",
        kind: "network",
        hint: "a download failed on the network; envira retries, or check the connection/proxy and run `envira retry`",
        transient: true,
    },
    Rule {
        re: r"The requested URL returned error: 404|HTTP 404|has no asset matching|404 Not Found",
        kind: "not found",
        hint: "a download URL or release asset does not exist (renamed upstream?); the catalog entry needs an update",
        transient: false,
    },
    Rule {
        re: r"sudo: a password is required|sudo: a terminal is required|is not in the sudoers file",
        kind: "sudo",
        hint: "sudo is not available without a password here; run envira from a terminal, or use --scope user",
        transient: false,
    },
    Rule {
        re: r"Permission denied|Operation not permitted|EACCES",
        kind: "permission",
        hint: "a file or directory is not writable by this user; check its owner (a root-owned file in your home?)",
        transient: false,
    },
    Rule {
        re: r"(?i)command not found|: not found$",
        kind: "missing command",
        hint: "a command the step needs is missing; install its dependency first (see `envira explain <id>`)",
        transient: false,
    },
];

fn compiled() -> &'static [(Regex, &'static Rule)] {
    static C: OnceLock<Vec<(Regex, &'static Rule)>> = OnceLock::new();
    C.get_or_init(|| {
        RULES
            .iter()
            .map(|r| (Regex::new(r.re).expect("valid rule regex"), r))
            .collect()
    })
}

/// Diagnoses the last lines of a failed step's output (envira's own `[envira]` lines are
/// ignored, except fetch-release's errors which are what failed).
pub fn diagnose<S: AsRef<str>>(lines: &[S]) -> Option<Diagnosis> {
    let tail = &lines[lines.len().saturating_sub(40)..];
    for (re, rule) in compiled() {
        let hit = tail.iter().any(|l| {
            let l = l.as_ref();
            !l.starts_with("[envira] ") && !l.starts_with("$ ") && re.is_match(l)
        });
        if hit {
            return Some(Diagnosis {
                kind: rule.kind,
                hint: rule.hint,
                transient: rule.transient,
            });
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    fn kind(log: &str) -> Option<(&'static str, bool)> {
        let lines: Vec<&str> = log.lines().collect();
        diagnose(&lines).map(|d| (d.kind, d.transient))
    }

    #[test]
    fn recognises_real_failures() {
        assert_eq!(
            kind("curl: (56) OpenSSL SSL_read: Connection reset by peer, errno 104"),
            Some(("network", true))
        );
        assert_eq!(
            kind("curl: (6) Could not resolve host: fnm.vercel.app"),
            Some(("network", true))
        );
        assert_eq!(
            kind("curl: (22) The requested URL returned error: 503"),
            Some(("network", true))
        );
        assert_eq!(
            kind("curl: (22) The requested URL returned error: 404"),
            Some(("not found", false))
        );
        assert_eq!(
            kind(
                "envira: HTTP 403 from GitHub: API rate limit exceeded for 1.2.3.4. (set GITHUB_TOKEN to raise the rate limit)"
            ),
            Some(("rate limit", false))
        );
        assert_eq!(
            kind(
                "E: Could not get lock /var/lib/dpkg/lock-frontend. It is held by process 812 (unattended-upgr)\nE: Unable to acquire the dpkg frontend lock (/var/lib/dpkg/lock-frontend), is another process using it?"
            ),
            Some(("package manager busy", true))
        );
        assert_eq!(
            kind(
                "error: externally-managed-environment\n\n× This environment is externally managed"
            ),
            Some(("PEP 668", false))
        );
        assert_eq!(
            kind("tar: go/bin/go: Cannot write: No space left on device"),
            Some(("disk full", false))
        );
        assert_eq!(
            kind("mkdir: cannot create directory '/usr/local/lib/x': Permission denied"),
            Some(("permission", false))
        );
        assert_eq!(
            kind("bash: line 1: cargo: command not found"),
            Some(("missing command", false))
        );
        assert_eq!(kind("sudo: a password is required"), Some(("sudo", false)));
        assert_eq!(kind("error: could not compile `foo`"), None);
    }

    #[test]
    fn ignores_envira_lines_and_old_output() {
        // The echoed command mentions curl but did not fail on the network.
        assert_eq!(kind("$ curl -fsSL https://x | bash\nboom"), None);
        let mut lines = vec!["curl: (7) Failed to connect".to_string()];
        lines.extend((0..50).map(|i| format!("line {i}")));
        assert_eq!(diagnose(&lines), None);
    }
}
