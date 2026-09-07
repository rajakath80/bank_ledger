//! Transparent, rule-based scoring. Deliberately not a model -- see design
//! doc section 4: a formula anyone can read and a decline that comes with
//! specific, ranked reasons, mirroring the real adverse-action-notice
//! requirement (Reg B) this system otherwise has no obligation to satisfy
//! but is worth modeling honestly anyway.

use crate::signals::{ExternalSignals, InternalSignals};
use serde::{Deserialize, Serialize};

pub const APPROVAL_THRESHOLD: i32 = 50;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ScoreFactor {
    pub name: String,
    pub points: i32,
    pub explanation: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ScoreResult {
    pub total_score: i32,
    pub approved: bool,
    /// Every factor considered, in evaluation order -- not just the ones
    /// that mattered. Full transparency, not a curated summary.
    pub factors: Vec<ScoreFactor>,
    /// The lowest-scoring factors, present only on a decline -- these are
    /// the adverse-action reason codes.
    pub reason_codes: Vec<String>,
}

/// Score one application. Every weight here is a starting point to be
/// tuned once real usage exists to tune against -- not a claim of
/// calibrated real-world accuracy, which nothing about a system built on
/// mock external data could honestly claim anyway.
pub fn score(
    internal: &InternalSignals,
    external: &ExternalSignals,
    requested_amount: u128,
) -> ScoreResult {
    let mut factors = Vec::new();

    // Capital: can the current balance alone cover the request?
    let balance_points = if internal.current_balance_net >= requested_amount as i128 {
        30
    } else if internal.current_balance_net > 0 {
        15
    } else {
        -20
    };
    factors.push(ScoreFactor {
        name: "capital.current_balance".to_string(),
        points: balance_points,
        explanation: format!("current balance is {}", internal.current_balance_net),
    });

    // Capital: time-weighted average balance over the lookback window,
    // when there's enough real date diversity to compute one honestly
    // (see InternalSignals::average_balance_lookback). A smoothed average
    // catches a account that looks fine today only because of a single
    // recent deposit -- something the point-in-time balance factor above
    // can't see on its own.
    match (internal.average_balance_lookback, internal.lookback_days) {
        (Some(avg), Some(days)) => {
            let avg_points = if avg >= requested_amount as f64 {
                15
            } else if avg > 0.0 {
                5
            } else {
                -15
            };
            factors.push(ScoreFactor {
                name: "capital.average_balance_lookback".to_string(),
                points: avg_points,
                explanation: format!("average balance over the trailing {days} days was {avg:.2}"),
            });
        }
        _ => {
            factors.push(ScoreFactor {
                name: "capital.average_balance_lookback".to_string(),
                points: 0,
                explanation: "insufficient time-diverse history to compute an average balance -- neither rewarded nor penalized".to_string(),
            });
        }
    }

    // Character: overdraft history. Meaningful specifically because
    // ledger-core never blocks a negative balance -- see signals.rs.
    let overdraft_points = -(internal.times_went_negative as i32) * 10;
    factors.push(ScoreFactor {
        name: "character.overdraft_history".to_string(),
        points: overdraft_points,
        explanation: format!(
            "account has gone negative {} time(s); lowest balance was {}",
            internal.times_went_negative, internal.lowest_balance_net
        ),
    });

    // Capacity: recurring income relative to the requested amount.
    let (capacity_points, capacity_explanation) = match &internal.detected_recurring_credit {
        Some(rc) => {
            if rc.occurrences >= 3 && (rc.amount as f64) >= (requested_amount as f64) * 0.1 {
                (
                    25,
                    format!(
                        "detected recurring credit of {} ({} occurrences), comfortably sized relative to the request",
                        rc.amount, rc.occurrences
                    ),
                )
            } else {
                (
                    10,
                    format!(
                        "detected recurring credit of {} ({} occurrences), but small relative to the request",
                        rc.amount, rc.occurrences
                    ),
                )
            }
        }
        None => (
            -10,
            "no recurring income pattern detected in this account's history".to_string(),
        ),
    };
    factors.push(ScoreFactor {
        name: "capacity.recurring_income".to_string(),
        points: capacity_points,
        explanation: capacity_explanation,
    });

    // External (synthetic): mock bureau score, centered at 600.
    let bureau_points = (external.mock_bureau_score as i32 - 600) / 5;
    factors.push(ScoreFactor {
        name: "external.bureau_score".to_string(),
        points: bureau_points,
        explanation: format!(
            "[SYNTHETIC] mock bureau score {}",
            external.mock_bureau_score
        ),
    });

    // External (synthetic): debt-to-income.
    let dti_points = if external.mock_external_dti > 0.5 {
        -25
    } else if external.mock_external_dti > 0.35 {
        -10
    } else {
        15
    };
    factors.push(ScoreFactor {
        name: "external.debt_to_income".to_string(),
        points: dti_points,
        explanation: format!(
            "[SYNTHETIC] mock external debt-to-income ratio {:.2}",
            external.mock_external_dti
        ),
    });

    let total_score: i32 = factors.iter().map(|f| f.points).sum();
    let approved = total_score >= APPROVAL_THRESHOLD;

    let reason_codes = if approved {
        Vec::new()
    } else {
        let mut by_points = factors.clone();
        by_points.sort_by_key(|f| f.points);
        by_points
            .into_iter()
            .take(3)
            .map(|f| f.explanation)
            .collect()
    };

    ScoreResult {
        total_score,
        approved,
        factors,
        reason_codes,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::signals::RecurringCredit;

    fn strong_internal() -> InternalSignals {
        InternalSignals {
            account_id: 1,
            current_balance_net: 10_000,
            lowest_balance_net: 500,
            times_went_negative: 0,
            currently_negative: false,
            total_credits: 30_000,
            total_debits: 20_000,
            posting_count: 12,
            detected_recurring_credit: Some(RecurringCredit {
                amount: 2000,
                occurrences: 4,
            }),
            average_balance_lookback: None,
            lookback_days: None,
            distinct_value_times: 12,
        }
    }

    fn weak_internal() -> InternalSignals {
        InternalSignals {
            account_id: 2,
            current_balance_net: -500,
            lowest_balance_net: -3000,
            times_went_negative: 3,
            currently_negative: true,
            total_credits: 1000,
            total_debits: 4000,
            posting_count: 5,
            detected_recurring_credit: None,
            average_balance_lookback: None,
            lookback_days: None,
            distinct_value_times: 5,
        }
    }

    fn good_external() -> ExternalSignals {
        ExternalSignals {
            synthetic: true,
            mock_bureau_score: 750,
            mock_external_dti: 0.20,
        }
    }

    fn bad_external() -> ExternalSignals {
        ExternalSignals {
            synthetic: true,
            mock_bureau_score: 500,
            mock_external_dti: 0.60,
        }
    }

    #[test]
    fn strong_applicant_is_approved_with_no_reason_codes() {
        let result = score(&strong_internal(), &good_external(), 1000);
        assert!(result.approved);
        assert!(
            result.reason_codes.is_empty(),
            "an approval should carry no decline reason codes"
        );
        assert_eq!(
            result.factors.len(),
            6,
            "every factor should be reported, not just the ones that mattered"
        );
    }

    #[test]
    fn weak_applicant_is_declined_with_ranked_reason_codes() {
        let result = score(&weak_internal(), &bad_external(), 1000);
        assert!(!result.approved);
        assert!(!result.reason_codes.is_empty());
        assert!(result.reason_codes.len() <= 3);

        // The single worst factor must be the first reason code -- these
        // are meant to be ranked by severity, not arbitrary order.
        let worst_points = result.factors.iter().map(|f| f.points).min().unwrap();
        let worst_factor = result
            .factors
            .iter()
            .find(|f| f.points == worst_points)
            .unwrap();
        assert_eq!(result.reason_codes[0], worst_factor.explanation);
    }

    #[test]
    fn every_synthetic_factor_explanation_says_so() {
        let result = score(&strong_internal(), &good_external(), 1000);
        let synthetic_factors: Vec<_> = result
            .factors
            .iter()
            .filter(|f| f.name.starts_with("external."))
            .collect();
        assert_eq!(synthetic_factors.len(), 2);
        for factor in synthetic_factors {
            assert!(
                factor.explanation.contains("[SYNTHETIC]"),
                "every externally-sourced factor must self-identify as synthetic in its own explanation, not just via a separate flag the caller might drop"
            );
        }
    }

    #[test]
    fn total_score_is_the_sum_of_every_factor() {
        let result = score(&strong_internal(), &good_external(), 1000);
        let sum: i32 = result.factors.iter().map(|f| f.points).sum();
        assert_eq!(result.total_score, sum);
    }
}
