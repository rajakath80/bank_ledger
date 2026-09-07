//! Three distinct time types with no implicit conversion between them.
//! Converting between them is always a named function that states its
//! policy explicitly (see [`business_date_from_value_time`]) -- there are
//! deliberately no `From`/`Into` impls between these types, so a
//! conversion always reads as a decision, not an accident of type
//! coercion.

use serde::{Deserialize, Serialize};

/// Monotonic log position. This is the only clock the state machine sees:
/// it arrives on every command via `apply(.., at: LogTime)` and must never
/// be read from a wall clock inside the state machine, or replay
/// determinism is lost.
#[derive(Copy, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Debug, Serialize, Deserialize)]
pub struct LogTime(pub u64);

impl LogTime {
    pub fn next(self) -> LogTime {
        LogTime(self.0 + 1)
    }
}

/// Economic effect date: the date a posting is considered to have happened
/// for accounting purposes, independent of when it was actually recorded.
/// Represented as days since the Unix epoch.
#[derive(Copy, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Debug, Serialize, Deserialize)]
pub struct ValueTime(pub i64);

impl ValueTime {
    /// The current UTC calendar date, as days since the Unix epoch -- the
    /// natural default when a caller has no reason to explicitly backdate
    /// or postdate a posting.
    ///
    /// Deriving this from the wall clock is deliberate and safe: unlike
    /// `LogTime`, which the state machine reads to make its replay-
    /// determinism guarantee and must never touch a clock for,
    /// `ValueTime` is opaque economic data `apply()` only ever stores and
    /// compares -- it never branches on "is this value_time close to
    /// now," so reading the wall clock here doesn't threaten determinism
    /// the way it would inside `apply()` itself.
    pub fn today() -> ValueTime {
        let secs = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0);
        ValueTime((secs / 86_400) as i64)
    }
}

/// Identifies the jurisdiction whose business-day rules apply.
#[derive(Copy, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Debug, Serialize, Deserialize)]
pub struct JurisdictionId(pub u16);

/// Minutes since midnight UTC, used only for cutoff comparisons.
#[derive(Copy, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Debug, Serialize, Deserialize)]
pub struct CutoffMinutes(pub u16);

/// A jurisdiction-specific business date, honouring a local cutoff. Not a
/// UTC calendar date: the same instant can fall on different business
/// dates in different jurisdictions, which is why this is a distinct type
/// from `ValueTime` rather than an alias for it.
#[derive(Copy, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Debug, Serialize, Deserialize)]
pub struct BusinessDate {
    pub date: ValueTime,
    pub jurisdiction: JurisdictionId,
}

/// Convert a value time to a business date under an explicit cutoff
/// policy: if the instant falls at or after `cutoff`, the value time rolls
/// forward to the next business date. This is the only sanctioned way to
/// move from a `ValueTime` to a `BusinessDate`.
pub fn business_date_from_value_time(
    value_time: ValueTime,
    instant: CutoffMinutes,
    cutoff: CutoffMinutes,
    jurisdiction: JurisdictionId,
) -> BusinessDate {
    let date = if instant.0 >= cutoff.0 {
        ValueTime(value_time.0 + 1)
    } else {
        value_time
    };
    BusinessDate { date, jurisdiction }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn before_cutoff_stays_on_the_same_value_date() {
        let bd = business_date_from_value_time(
            ValueTime(100),
            CutoffMinutes(600),
            CutoffMinutes(900),
            JurisdictionId(1),
        );
        assert_eq!(bd.date, ValueTime(100));
    }

    #[test]
    fn at_or_after_cutoff_rolls_to_the_next_value_date() {
        let bd = business_date_from_value_time(
            ValueTime(100),
            CutoffMinutes(900),
            CutoffMinutes(900),
            JurisdictionId(1),
        );
        assert_eq!(bd.date, ValueTime(101));
    }

    #[test]
    fn today_returns_a_plausible_days_since_epoch() {
        // 2020-01-01 is day 18262 since the epoch; anything from then
        // until well past this decade is a sane sanity range -- not
        // testing an exact value, since "today" changes by definition.
        let today = ValueTime::today();
        assert!(
            today.0 > 18_262,
            "expected a date after 2020-01-01, got day {}",
            today.0
        );
        assert!(
            today.0 < 29_000,
            "expected a date before ~2049, got day {} -- check for a units bug (ms vs s)",
            today.0
        );
    }
}
