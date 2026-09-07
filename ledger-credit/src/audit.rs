//! The decision audit log. Reuses `RecordStorage` exactly the way
//! TechOps's `create_incident` does -- a separate append-only store, never
//! the real ledger's own segments, opened fresh per operation rather than
//! held open across requests (same simplicity tradeoff TechOps makes).
//!
//! Storing the full signal snapshot alongside the outcome -- not just
//! approved/declined -- is what makes `explain_decision` able to show
//! exactly what was known at decision time without re-deriving history
//! that may have since changed.

use crate::scoring::ScoreResult;
use crate::signals::{ExternalSignals, InternalSignals};
use ledger_log::RecordStorage;
use ledger_store::SegmentedFileStorage;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Decision {
    pub decision_id: u128,
    pub account_id: u64,
    pub requested_amount: u128,
    pub decided_at_unix_ms: u128,
    pub approved: bool,
    pub total_score: i32,
    pub reason_codes: Vec<String>,
    pub internal_signals: InternalSignals,
    pub external_signals: ExternalSignals,
}

impl Decision {
    pub fn new(
        account_id: u64,
        requested_amount: u128,
        internal_signals: InternalSignals,
        external_signals: ExternalSignals,
        result: &ScoreResult,
    ) -> Self {
        let decided_at_unix_ms = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_millis())
            .unwrap_or(0);
        // A millisecond timestamp is not a guaranteed-unique id on its
        // own (two decisions in the same millisecond would collide) --
        // acceptable for a POC's request volume, but worth noting as the
        // thing to fix first if this ever needs real concurrency
        // guarantees.
        let decision_id = decided_at_unix_ms;
        Decision {
            decision_id,
            account_id,
            requested_amount,
            decided_at_unix_ms,
            approved: result.approved,
            total_score: result.total_score,
            reason_codes: result.reason_codes.clone(),
            internal_signals,
            external_signals,
        }
    }
}

pub fn persist_decision(audit_dir: &str, decision: &Decision) -> anyhow::Result<()> {
    let payload = serde_json::to_vec(decision)?;
    let mut store = SegmentedFileStorage::open(audit_dir)?;
    store.append(&payload)?;
    store.sync()?;
    Ok(())
}

/// Every decision ever recorded, oldest first. An audit log that doesn't
/// exist yet (no decision has ever been made) is not an error -- opening
/// it creates an empty one, matching TechOps's incident log the same way.
pub fn load_all_decisions(audit_dir: &str) -> anyhow::Result<Vec<Decision>> {
    let store = SegmentedFileStorage::open(audit_dir)?;
    let outcome = store.recover()?;
    outcome
        .records
        .iter()
        .map(|payload| {
            serde_json::from_slice(payload)
                .map_err(|e| anyhow::anyhow!("corrupt decision record: {e}"))
        })
        .collect()
}

pub fn find_decision(audit_dir: &str, decision_id: u128) -> anyhow::Result<Option<Decision>> {
    Ok(load_all_decisions(audit_dir)?
        .into_iter()
        .find(|d| d.decision_id == decision_id))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::signals::RecurringCredit;
    use std::time::{SystemTime, UNIX_EPOCH};

    fn temp_dir(label: &str) -> String {
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        std::env::temp_dir()
            .join(format!("ledger_credit_audit_test_{label}_{nanos}"))
            .to_str()
            .unwrap()
            .to_string()
    }

    fn sample_internal() -> InternalSignals {
        InternalSignals {
            account_id: 1,
            current_balance_net: 5000,
            lowest_balance_net: 0,
            times_went_negative: 0,
            currently_negative: false,
            total_credits: 5000,
            total_debits: 0,
            posting_count: 1,
            detected_recurring_credit: Some(RecurringCredit {
                amount: 1000,
                occurrences: 3,
            }),
            average_balance_lookback: None,
            lookback_days: None,
            distinct_value_times: 1,
        }
    }

    fn sample_external() -> ExternalSignals {
        ExternalSignals {
            synthetic: true,
            mock_bureau_score: 700,
            mock_external_dti: 0.2,
        }
    }

    #[test]
    fn empty_audit_log_returns_no_decisions_not_an_error() {
        let dir = temp_dir("empty");
        let decisions = load_all_decisions(&dir).unwrap();
        std::fs::remove_dir_all(&dir).ok();
        assert!(decisions.is_empty());
    }

    #[test]
    fn persisted_decisions_round_trip_and_are_individually_findable() {
        let dir = temp_dir("roundtrip");

        let result_a = crate::scoring::score(&sample_internal(), &sample_external(), 1000);
        let decision_a = Decision::new(1, 1000, sample_internal(), sample_external(), &result_a);
        persist_decision(&dir, &decision_a).unwrap();

        // Ensure a distinct millisecond so the two decisions get distinct
        // ids (see the doc note on Decision::new about id uniqueness).
        std::thread::sleep(std::time::Duration::from_millis(2));

        let result_b = crate::scoring::score(&sample_internal(), &sample_external(), 2000);
        let decision_b = Decision::new(2, 2000, sample_internal(), sample_external(), &result_b);
        persist_decision(&dir, &decision_b).unwrap();

        let all = load_all_decisions(&dir).unwrap();
        assert_eq!(all.len(), 2);
        assert_eq!(all[0], decision_a);
        assert_eq!(all[1], decision_b);

        let found = find_decision(&dir, decision_a.decision_id).unwrap();
        assert_eq!(found, Some(decision_a));

        let missing = find_decision(&dir, 0).unwrap();
        assert_eq!(missing, None);

        std::fs::remove_dir_all(&dir).ok();
    }
}
