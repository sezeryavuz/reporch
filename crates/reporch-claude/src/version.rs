//! Parsing `claude --version`.
//!
//! Cross-session messaging requires `>= 2.1.224` (§4.5), and Phase 0 was measured on 2.1.226. The
//! version string is a product surface that has changed shape before, so parsing is deliberately
//! forgiving about everything except the three numbers.

use std::cmp::Ordering;
use std::fmt;

/// A three-part version. Pre-release and build metadata are ignored: reporch only ever asks
/// "is this at least 2.1.224?".
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Version {
    pub major: u32,
    pub minor: u32,
    pub patch: u32,
}

impl Version {
    pub const fn new(major: u32, minor: u32, patch: u32) -> Self {
        Self {
            major,
            minor,
            patch,
        }
    }

    /// The minimum Claude Code that has cross-session messaging (§4.5).
    pub const MINIMUM: Version = Version::new(2, 1, 224);

    /// Parse the first `N.N.N` found anywhere in the output.
    ///
    /// `claude --version` prints `2.1.226 (Claude Code)`, but it has printed other shapes in the
    /// past and may again; scanning for the first version-looking token survives that.
    pub fn parse(s: &str) -> Option<Self> {
        let bytes = s.as_bytes();
        let mut i = 0;
        while i < bytes.len() {
            if bytes[i].is_ascii_digit() {
                let start = i;
                while i < bytes.len() && (bytes[i].is_ascii_digit() || bytes[i] == b'.') {
                    i += 1;
                }
                let token = &s[start..i];
                let mut parts = token.split('.');
                if let (Some(a), Some(b), Some(c)) = (parts.next(), parts.next(), parts.next()) {
                    if let (Ok(a), Ok(b), Ok(c)) =
                        (a.parse::<u32>(), b.parse::<u32>(), c.parse::<u32>())
                    {
                        return Some(Version::new(a, b, c));
                    }
                }
            } else {
                i += 1;
            }
        }
        None
    }

    pub fn meets_minimum(self) -> bool {
        self >= Self::MINIMUM
    }
}

impl fmt::Display for Version {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}.{}.{}", self.major, self.minor, self.patch)
    }
}

/// Compare two version strings, for the update check (§5.1). `None` if either will not parse.
pub fn compare(a: &str, b: &str) -> Option<Ordering> {
    Some(Version::parse(a)?.cmp(&Version::parse(b)?))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_the_real_output() {
        // Verbatim from `claude --version` on this machine.
        assert_eq!(
            Version::parse("2.1.226 (Claude Code)"),
            Some(Version::new(2, 1, 226))
        );
    }

    #[test]
    fn parses_bare_and_prefixed_forms() {
        assert_eq!(Version::parse("2.1.224"), Some(Version::new(2, 1, 224)));
        assert_eq!(Version::parse("v0.1.0"), Some(Version::new(0, 1, 0)));
        assert_eq!(
            Version::parse("Claude Code version 10.20.30 (build x)"),
            Some(Version::new(10, 20, 30))
        );
    }

    #[test]
    fn ignores_prerelease_and_build_metadata() {
        assert_eq!(
            Version::parse("2.1.226-beta.1"),
            Some(Version::new(2, 1, 226))
        );
    }

    #[test]
    fn returns_none_when_there_is_no_version() {
        for s in ["", "command not found", "2.1", "abc"] {
            assert_eq!(Version::parse(s), None, "should not parse {s:?}");
        }
    }

    #[test]
    fn ordering_is_numeric_not_lexicographic() {
        // The trap: "2.1.9" > "2.1.10" as strings.
        assert!(Version::new(2, 1, 10) > Version::new(2, 1, 9));
        assert!(Version::new(2, 2, 0) > Version::new(2, 1, 999));
        assert!(Version::new(3, 0, 0) > Version::new(2, 99, 99));
    }

    #[test]
    fn the_minimum_gate_matches_section_4_5() {
        assert!(!Version::new(2, 1, 223).meets_minimum());
        assert!(Version::new(2, 1, 224).meets_minimum(), "the exact minimum");
        assert!(
            Version::new(2, 1, 226).meets_minimum(),
            "verified in Phase 0"
        );
        assert!(Version::new(2, 2, 0).meets_minimum());
        assert!(!Version::new(1, 9, 999).meets_minimum());
    }

    #[test]
    fn compare_drives_the_update_check() {
        assert_eq!(compare("0.1.0", "0.2.0"), Some(Ordering::Less));
        assert_eq!(compare("v1.0.0", "1.0.0"), Some(Ordering::Equal));
        assert_eq!(compare("junk", "1.0.0"), None);
    }
}
