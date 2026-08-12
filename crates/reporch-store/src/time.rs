//! Time. **UTC, RFC 3339, everywhere** (CLAUDE.md).
//!
//! `reporch-core` deliberately has no clock — every protocol event carries an `at_ms` supplied by
//! the caller, so the state machine is exact rather than flaky. This is where those numbers come
//! from.

use jiff::Timestamp;

/// The current instant as UTC RFC 3339, e.g. `2026-08-07T21:44:12Z`.
///
/// Second precision: these end up in filenames, YAML frontmatter and JSON that a human reads, and
/// nanoseconds are noise there.
pub fn now_rfc3339() -> String {
    format_rfc3339(Timestamp::now())
}

/// Format a timestamp as UTC RFC 3339 with second precision.
pub fn format_rfc3339(ts: Timestamp) -> String {
    // `Timestamp` is always UTC in jiff, and its `Display` is RFC 3339.
    ts.strftime("%Y-%m-%dT%H:%M:%SZ").to_string()
}

/// Milliseconds since the Unix epoch — the `at_ms` the protocol state machine consumes.
pub fn now_unix_ms() -> u64 {
    Timestamp::now().as_millisecond().max(0) as u64
}

/// Parse an RFC 3339 timestamp back, for computing durations against stored values.
pub fn parse_rfc3339(s: &str) -> Option<Timestamp> {
    s.parse::<Timestamp>().ok()
}

/// A compact human duration for the run screen: `6m12s`, `45s`, `2h04m`.
pub fn human_duration(secs: u64) -> String {
    if secs < 60 {
        format!("{secs}s")
    } else if secs < 3600 {
        format!("{}m{:02}s", secs / 60, secs % 60)
    } else {
        format!("{}h{:02}m", secs / 3600, (secs % 3600) / 60)
    }
}

/// Seconds elapsed since an RFC 3339 timestamp, or `None` if it will not parse.
pub fn secs_since(rfc3339: &str) -> Option<u64> {
    let then = parse_rfc3339(rfc3339)?;
    let now = Timestamp::now();
    let d = now.as_second() - then.as_second();
    Some(d.max(0) as u64)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn now_is_rfc3339_utc_with_second_precision() {
        let s = now_rfc3339();
        assert!(s.ends_with('Z'), "must be UTC: {s}");
        assert_eq!(s.len(), 20, "YYYY-MM-DDTHH:MM:SSZ is 20 chars: {s}");
        assert!(parse_rfc3339(&s).is_some(), "must round-trip: {s}");
    }

    #[test]
    fn parses_the_spec_examples() {
        for s in [
            "2026-08-07T21:44:12Z",
            "2026-08-07T21:30:00Z",
            "2026-08-07T21:59:10Z",
        ] {
            assert!(parse_rfc3339(s).is_some(), "{s}");
        }
    }

    #[test]
    fn rejects_junk() {
        assert!(parse_rfc3339("not a time").is_none());
        assert!(parse_rfc3339("").is_none());
    }

    #[test]
    fn unix_ms_is_plausible_and_monotonic_enough() {
        let a = now_unix_ms();
        // Later than 2020-01-01 and earlier than 2100-01-01 — catches a units mix-up.
        assert!(a > 1_577_836_800_000, "too small: {a}");
        assert!(a < 4_102_444_800_000, "too large: {a}");
        assert!(now_unix_ms() >= a);
    }

    #[test]
    fn durations_render_compactly() {
        assert_eq!(human_duration(0), "0s");
        assert_eq!(human_duration(45), "45s");
        assert_eq!(human_duration(372), "6m12s", "the §5.8 example");
        assert_eq!(human_duration(60), "1m00s");
        assert_eq!(human_duration(3600), "1h00m");
        assert_eq!(human_duration(7440), "2h04m");
    }

    #[test]
    fn secs_since_a_past_timestamp_is_positive() {
        assert!(secs_since("2020-01-01T00:00:00Z").unwrap() > 0);
        assert_eq!(secs_since("garbage"), None);
    }

    /// A future timestamp must clamp to zero rather than wrapping — clock skew between the
    /// machine writing a brief and the one reading it should never produce an absurd age.
    #[test]
    fn a_future_timestamp_clamps_to_zero() {
        assert_eq!(secs_since("2099-01-01T00:00:00Z"), Some(0));
    }
}
