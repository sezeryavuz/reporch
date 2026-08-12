//! Effort levels and the master → worker ladder (§5.6).
//!
//! The rule in one line: **workers run one step below the master, capped at `xhigh`.**
//! Ultracode is an independent toggle, not a sixth level — see the §4.3 Deviation, and D007 in
//! `docs/decisions.md` for why reporch spells "max + ultracode" as `--effort max` plus the
//! `ultracode` settings key rather than `--effort ultracode`.

use serde::{Deserialize, Serialize};
use std::fmt;
use std::str::FromStr;

/// The five effort levels Claude Code accepts, in ascending order.
///
/// `ultracode` is deliberately absent. It *is* accepted by `--effort`, but it pins effort to
/// `xhigh`, so it cannot express §5.6's top row (max effort *and* ultracode). reporch keeps the
/// two orthogonal: this enum plus a `bool`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Effort {
    Low,
    Medium,
    High,
    XHigh,
    Max,
}

impl Effort {
    /// Every level, ascending. The order is load-bearing: `PartialOrd` is derived from it.
    pub const ALL: [Effort; 5] = [
        Effort::Low,
        Effort::Medium,
        Effort::High,
        Effort::XHigh,
        Effort::Max,
    ];

    /// The value passed to `claude --effort`.
    pub const fn as_str(self) -> &'static str {
        match self {
            Effort::Low => "low",
            Effort::Medium => "medium",
            Effort::High => "high",
            Effort::XHigh => "xhigh",
            Effort::Max => "max",
        }
    }

    /// One level down, saturating at `Low`.
    ///
    /// The floor is what makes the `low → low` row of §5.6's table work.
    pub const fn step_down(self) -> Effort {
        match self {
            Effort::Low => Effort::Low,
            Effort::Medium => Effort::Low,
            Effort::High => Effort::Medium,
            Effort::XHigh => Effort::High,
            Effort::Max => Effort::XHigh,
        }
    }

    /// The effort every worker session runs at, given the master's (§5.6).
    ///
    /// `worker = min(step_down(master), xhigh)`. The cap is redundant today because
    /// `step_down(Max)` is already `XHigh`, but it is written explicitly because it is the rule
    /// the spec states, and a sixth level appearing above `Max` must not silently raise workers.
    pub const fn worker_effort(master: Effort) -> Effort {
        let stepped = master.step_down();
        // `min` is not const for a derived Ord, so this is spelled out.
        if (stepped as u8) < (Effort::XHigh as u8) {
            stepped
        } else {
            Effort::XHigh
        }
    }

    /// A one-line human description, shown on the §5.6 effort screen.
    pub const fn description(self) -> &'static str {
        match self {
            Effort::Low => "fastest, least thorough",
            Effort::Medium => "balanced",
            Effort::High => "thorough; a good default for real work",
            Effort::XHigh => "very thorough; slower and more expensive",
            Effort::Max => "maximum capability; slowest and most expensive",
        }
    }
}

impl fmt::Display for Effort {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// Rejected `--effort` value.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("unknown effort level `{0}`; valid values are low, medium, high, xhigh, max")]
pub struct ParseEffortError(pub String);

impl FromStr for Effort {
    type Err = ParseEffortError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s.trim().to_ascii_lowercase().as_str() {
            "low" => Ok(Effort::Low),
            "medium" => Ok(Effort::Medium),
            "high" => Ok(Effort::High),
            "xhigh" => Ok(Effort::XHigh),
            "max" => Ok(Effort::Max),
            other => Err(ParseEffortError(other.to_string())),
        }
    }
}

/// The effort choice made once per task on the §5.6 screen, and derived from it, the effort every
/// session in the task runs at.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct EffortPlan {
    pub master: Effort,
    pub worker: Effort,
    /// Applies to the **master only**. Fan-out inside a worker multiplies cost without improving
    /// coordination (§5.6).
    pub ultracode: bool,
}

impl EffortPlan {
    /// Build the plan from the single choice the user makes.
    pub const fn new(master: Effort, ultracode: bool) -> Self {
        Self {
            master,
            worker: Effort::worker_effort(master),
            ultracode,
        }
    }

    /// The effort a given role runs at.
    pub const fn for_role(&self, role: crate::Role) -> Effort {
        match role {
            crate::Role::Master => self.master,
            crate::Role::Worker => self.worker,
        }
    }

    /// Whether ultracode is on for a given role. Never for workers.
    pub const fn ultracode_for_role(&self, role: crate::Role) -> bool {
        match role {
            crate::Role::Master => self.ultracode,
            crate::Role::Worker => false,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Role;

    /// §5.6's table, verbatim. All six rows.
    #[test]
    fn ladder_matches_spec_table_5_6() {
        // | User picks | Master | Workers |
        let rows: &[(Effort, bool, Effort, Effort, bool)] = &[
            // picks,         ultra, master,        worker,        worker_ultra
            (Effort::Low, false, Effort::Low, Effort::Low, false),
            (Effort::Medium, false, Effort::Medium, Effort::Low, false),
            (Effort::High, false, Effort::High, Effort::Medium, false),
            (Effort::XHigh, false, Effort::XHigh, Effort::High, false),
            (Effort::Max, false, Effort::Max, Effort::XHigh, false),
            (Effort::Max, true, Effort::Max, Effort::XHigh, false),
        ];
        for &(picks, ultra, want_master, want_worker, want_worker_ultra) in rows {
            let plan = EffortPlan::new(picks, ultra);
            assert_eq!(plan.master, want_master, "master for {picks} ultra={ultra}");
            assert_eq!(plan.worker, want_worker, "worker for {picks} ultra={ultra}");
            assert_eq!(
                plan.for_role(Role::Master),
                want_master,
                "for_role(master) for {picks}"
            );
            assert_eq!(
                plan.for_role(Role::Worker),
                want_worker,
                "for_role(worker) for {picks}"
            );
            assert_eq!(
                plan.ultracode_for_role(Role::Master),
                ultra,
                "master ultracode for {picks}"
            );
            assert_eq!(
                plan.ultracode_for_role(Role::Worker),
                want_worker_ultra,
                "worker ultracode is never on, for {picks}"
            );
        }
    }

    #[test]
    fn worker_never_exceeds_master() {
        for m in Effort::ALL {
            assert!(
                Effort::worker_effort(m) <= m,
                "worker {} must not exceed master {m}",
                Effort::worker_effort(m)
            );
        }
    }

    #[test]
    fn worker_is_capped_at_xhigh() {
        for m in Effort::ALL {
            assert!(Effort::worker_effort(m) <= Effort::XHigh);
        }
    }

    #[test]
    fn step_down_has_a_floor() {
        assert_eq!(Effort::Low.step_down(), Effort::Low);
        // Repeated application converges rather than underflowing.
        let mut e = Effort::Max;
        for _ in 0..10 {
            e = e.step_down();
        }
        assert_eq!(e, Effort::Low);
    }

    #[test]
    fn ordering_is_ascending() {
        assert!(Effort::Low < Effort::Medium);
        assert!(Effort::Medium < Effort::High);
        assert!(Effort::High < Effort::XHigh);
        assert!(Effort::XHigh < Effort::Max);
    }

    #[test]
    fn parses_and_round_trips() {
        for e in Effort::ALL {
            assert_eq!(e.as_str().parse::<Effort>().unwrap(), e);
            assert_eq!(e.to_string(), e.as_str());
        }
        assert_eq!("  XHIGH ".parse::<Effort>().unwrap(), Effort::XHigh);
    }

    /// `ultracode` is accepted by `claude --effort`, but reporch does not model it as a level.
    /// Rejecting it here is correct: reporch expresses it as `--effort max` + a settings key
    /// (§4.3 Deviation, D007).
    #[test]
    fn ultracode_is_not_an_effort_level_in_reporch() {
        assert!("ultracode".parse::<Effort>().is_err());
    }

    #[test]
    fn rejects_unknown() {
        let err = "turbo".parse::<Effort>().unwrap_err();
        assert!(err.to_string().contains("low, medium, high, xhigh, max"));
    }

    #[test]
    fn serde_uses_the_cli_spelling() {
        let json = serde_json::to_string(&Effort::XHigh).unwrap();
        assert_eq!(json, "\"xhigh\"");
        assert_eq!(
            serde_json::from_str::<Effort>("\"max\"").unwrap(),
            Effort::Max
        );
    }
}
