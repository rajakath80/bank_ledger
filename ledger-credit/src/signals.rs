//! Internal (real) and external (synthetic) signal computation. See
//! `04-credit-decisioning-design.md` sections 2-3 for the reasoning
//! behind exactly what's computed here and what's deliberately not.

use ledger_core::Command;
use ledger_log::{Record, decode_payload};
use ledger_store::recover_read_only;
use ledger_types::ids::AccountId;
use ledger_types::posting::Side;
use ledger_types::time::ValueTime;
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};

/// How many days back the lookback-window balance average considers,
/// anchored to today's real calendar date.
const LOOKBACK_DAYS: i64 = 90;

/// A same-amount credit seen more than once -- a simple, honest proxy for
/// recurring income (payroll, benefits) given this system has no
/// concept of a "payer" identity to match on, only amounts.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct RecurringCredit {
    pub amount: u128,
    pub occurrences: usize,
}

/// Real signals, computed entirely by replaying this ledger's own
/// history -- no external data, no synthetic values anywhere in this
/// struct.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct InternalSignals {
    pub account_id: u64,
    pub current_balance_net: i128,
    pub lowest_balance_net: i128,
    /// Number of times the running balance crossed from non-negative
    /// into negative. Meaningful specifically because `ledger-core`
    /// never blocks a negative balance (design section 5) -- this is
    /// genuine risk signal, not a proxy for something else.
    pub times_went_negative: u32,
    pub currently_negative: bool,
    pub total_credits: u128,
    pub total_debits: u128,
    pub posting_count: usize,
    pub detected_recurring_credit: Option<RecurringCredit>,
    /// Time-weighted average balance over the trailing `lookback_days`
    /// days, anchored to today. `None` when `distinct_value_times < 2` --
    /// see that field's own doc comment for why: with only one real
    /// timestamp, "average over N days" is indistinguishable from "the
    /// one balance we happen to have," which would be a fake-looking
    /// number wearing a real one's clothes.
    pub average_balance_lookback: Option<f64>,
    pub lookback_days: Option<i64>,
    /// How many distinct `value_time`s appear across this account's
    /// postings. 0 or 1 means every posting shares the same placeholder
    /// timestamp (every example built so far uses `ValueTime(0)`) --
    /// see design doc section 3.3. This is what `average_balance_lookback`
    /// checks before computing anything: below 2, there's no real
    /// time-series signal to compute a window average from.
    pub distinct_value_times: usize,
}

/// Replay `ledger_dir` read-only (never writes to the real ledger) and
/// compute every internal signal for one account in a single pass over
/// its postings.
///
/// Two different orderings are deliberately used for two different
/// things: `current_balance_net`/`lowest_balance_net`/`times_went_negative`
/// reflect the real, as-applied sequence of events (log/`LogTime` order --
/// what actually happened and when the ledger actually recorded it), while
/// the lookback-window average reconstructs the *economic* timeline
/// (sorted by `ValueTime`), because "average balance on calendar day N"
/// is a statement about that day, regardless of when a posting affecting
/// it was actually recorded. A backdated correction (design section 6:
/// "backdated corrections are new postings, never mutations") could be
/// logged after postings with a later `ValueTime` -- sorting is what
/// keeps the lookback average correct in that case; log order alone
/// would not be.
pub fn compute_internal_signals(
    ledger_dir: &str,
    account_id: u64,
) -> anyhow::Result<InternalSignals> {
    let outcome = recover_read_only(ledger_dir)?;
    let records: Vec<Record> = outcome
        .records
        .iter()
        .map(|p| decode_payload(p))
        .collect::<Result<_, _>>()
        .map_err(|e| anyhow::anyhow!("{e}"))?;

    let target = AccountId(account_id);
    let mut running_balance: i128 = 0;
    let mut lowest_balance: i128 = 0;
    let mut times_went_negative: u32 = 0;
    let mut was_negative = false;
    let mut total_credits: u128 = 0;
    let mut total_debits: u128 = 0;
    let mut posting_count = 0usize;
    let mut credit_amounts: BTreeMap<u128, usize> = BTreeMap::new();
    let mut value_times: BTreeSet<i64> = BTreeSet::new();
    let mut value_time_deltas: Vec<(i64, i128)> = Vec::new();

    for record in &records {
        if let Command::PostBatch(batch) = record.command() {
            for posting in &batch.postings {
                if posting.key.account != target {
                    continue;
                }
                posting_count += 1;
                value_times.insert(batch.value_time.0);

                let delta = match posting.side {
                    Side::Credit => posting.amount as i128,
                    Side::Debit => -(posting.amount as i128),
                };
                value_time_deltas.push((batch.value_time.0, delta));

                match posting.side {
                    Side::Credit => {
                        running_balance += posting.amount as i128;
                        total_credits += posting.amount;
                        *credit_amounts.entry(posting.amount).or_insert(0) += 1;
                    }
                    Side::Debit => {
                        running_balance -= posting.amount as i128;
                        total_debits += posting.amount;
                    }
                }

                if running_balance < lowest_balance {
                    lowest_balance = running_balance;
                }
                let now_negative = running_balance < 0;
                if now_negative && !was_negative {
                    times_went_negative += 1;
                }
                was_negative = now_negative;
            }
        }
    }

    let detected_recurring_credit = credit_amounts
        .into_iter()
        .filter(|(_, count)| *count >= 2)
        .max_by_key(|(_, count)| *count)
        .map(|(amount, occurrences)| RecurringCredit {
            amount,
            occurrences,
        });

    // Reconstruct the balance-over-time series in economic (ValueTime)
    // order -- not the order `value_time_deltas` happened to be pushed
    // in, which follows log order.
    value_time_deltas.sort_by_key(|(vt, _)| *vt);
    let mut cumulative: i128 = 0;
    let balance_series: Vec<(i64, i128)> = value_time_deltas
        .into_iter()
        .map(|(vt, delta)| {
            cumulative += delta;
            (vt, cumulative)
        })
        .collect();
    let lookback = compute_lookback_average(&balance_series, value_times.len());

    Ok(InternalSignals {
        account_id,
        current_balance_net: running_balance,
        lowest_balance_net: lowest_balance,
        times_went_negative,
        currently_negative: was_negative,
        total_credits,
        total_debits,
        posting_count,
        detected_recurring_credit,
        average_balance_lookback: lookback.map(|(avg, _)| avg),
        lookback_days: lookback.map(|(_, days)| days),
        distinct_value_times: value_times.len(),
    })
}

/// `events` must already be sorted by value_time (ascending), each paired
/// with the cumulative balance immediately after it. Returns
/// `(time_weighted_average, lookback_days)`, or `None` when
/// `distinct_value_times < 2` -- see `InternalSignals::
/// average_balance_lookback`'s doc comment for why that threshold exists.
fn compute_lookback_average(
    events: &[(i64, i128)],
    distinct_value_times: usize,
) -> Option<(f64, i64)> {
    if distinct_value_times < 2 {
        return None;
    }

    let today = ValueTime::today().0;
    let window_start = today - LOOKBACK_DAYS;
    if window_start >= today {
        return None; // defensive; LOOKBACK_DAYS is a positive constant, this shouldn't happen
    }

    // The balance immediately before the window starts: the balance after
    // the last event strictly before window_start, or 0 if the account's
    // whole history postdates the window.
    let mut idx = 0;
    let mut balance_at_window_start: i128 = 0;
    while idx < events.len() && events[idx].0 < window_start {
        balance_at_window_start = events[idx].1;
        idx += 1;
    }

    let mut weighted_sum = 0f64;
    let mut current_balance = balance_at_window_start as f64;
    let mut current_day = window_start;

    while idx < events.len() && events[idx].0 <= today {
        let (event_day, balance_after) = events[idx];
        let event_day = event_day.max(window_start); // clip anything exactly at the boundary
        let span_days = (event_day - current_day) as f64;
        weighted_sum += current_balance * span_days;
        current_balance = balance_after as f64;
        current_day = event_day;
        idx += 1;
    }

    // The remaining span from the last event in-window (or window_start,
    // if nothing fell inside it) up to today.
    weighted_sum += current_balance * (today - current_day) as f64;

    Some((weighted_sum / LOOKBACK_DAYS as f64, LOOKBACK_DAYS))
}

/// External (synthetic) signals. `synthetic` is a real field, not just a
/// wrapper convention, specifically so it survives every JSON
/// round-trip and can never be silently dropped by a consumer that only
/// looks at the numeric fields.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ExternalSignals {
    pub synthetic: bool,
    pub mock_bureau_score: u32,
    pub mock_external_dti: f64,
}

#[derive(Debug, Clone, Deserialize)]
struct ExternalRecord {
    account_id: u64,
    mock_bureau_score: u32,
    mock_external_dti: f64,
}

#[derive(Debug, Clone, Default, Deserialize)]
struct ExternalFile {
    #[serde(default)]
    accounts: Vec<ExternalRecord>,
}

/// A neutral default used when an account has no entry in the mock
/// external-data file. Still `synthetic: true` -- an absent record isn't
/// a special "real" case, it's just an unconfigured synthetic one.
const DEFAULT_BUREAU_SCORE: u32 = 620;
const DEFAULT_EXTERNAL_DTI: f64 = 0.30;

/// Baked into the binary at compile time via `include_str!`, exactly the
/// way `ledger-dashboard` embeds its HTML -- survives a `/tmp` wipe, a
/// fresh git clone, anything, because it's part of the compiled artifact
/// itself, not a runtime file that has to exist somewhere. Used whenever
/// `path` is `None` or the given path doesn't exist; still fully
/// overridable by pointing `EXTERNAL_DATA_PATH` at a real file if you
/// want different mock data without recompiling.
const EMBEDDED_DEFAULT_EXTERNAL_DATA: &str = include_str!("../default-external-data.json");

pub fn load_external_signals(
    path: Option<&str>,
    account_id: u64,
) -> anyhow::Result<ExternalSignals> {
    let bytes: std::borrow::Cow<[u8]> = match path {
        Some(p) if std::path::Path::new(p).exists() => std::fs::read(p)?.into(),
        _ => EMBEDDED_DEFAULT_EXTERNAL_DATA.as_bytes().into(),
    };
    let file: ExternalFile = serde_json::from_slice(&bytes)?;
    let found = file
        .accounts
        .into_iter()
        .find(|r| r.account_id == account_id);
    Ok(match found {
        Some(r) => ExternalSignals {
            synthetic: true,
            mock_bureau_score: r.mock_bureau_score,
            mock_external_dti: r.mock_external_dti,
        },
        None => ExternalSignals {
            synthetic: true,
            mock_bureau_score: DEFAULT_BUREAU_SCORE,
            mock_external_dti: DEFAULT_EXTERNAL_DTI,
        },
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use ledger_core::{Command, State, apply};
    use ledger_log::{RecordStorage, encode_payload};
    use ledger_store::SegmentedFileStorage;
    use ledger_types::balance::{BalanceKey, Phase};
    use ledger_types::ids::{AddressId, ClientKey, DenomId};
    use ledger_types::posting::{Batch, Posting};
    use ledger_types::time::{LogTime, ValueTime};
    use std::time::{SystemTime, UNIX_EPOCH};

    fn temp_dir(label: &str) -> String {
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let path = std::env::temp_dir().join(format!("ledger_credit_test_{label}_{nanos}"));
        path.to_str().unwrap().to_string()
    }

    fn key(account: u64) -> BalanceKey {
        BalanceKey {
            account: AccountId(account),
            address: AddressId::DEFAULT,
            denom: DenomId(1),
            phase: Phase::Committed,
        }
    }

    fn assert_close(actual: f64, expected: f64, label: &str) {
        assert!(
            (actual - expected).abs() < 0.01,
            "{label}: expected {expected}, got {actual}"
        );
    }

    /// Seed a ledger where account 1 receives three separate 1000-unit
    /// credits (a recurring pattern), goes negative once via a large
    /// debit, and recovers.
    fn seed_test_ledger(dir: &str) {
        let mut state = State::new();
        let mut store = SegmentedFileStorage::open(dir).unwrap();
        let mut push = |state: &mut State, cmd: Command, at: u64| {
            let (next, _) = apply(state, &cmd, LogTime(at)).unwrap();
            *state = next;
            let payload = encode_payload(&Record::v1(LogTime(at), cmd)).unwrap();
            store.append(&payload).unwrap();
        };

        push(
            &mut state,
            Command::OpenAccount {
                account: AccountId(1),
            },
            0,
        );
        push(
            &mut state,
            Command::OpenAccount {
                account: AccountId(9),
            },
            1,
        );

        // Three recurring 1000-unit credits to account 1, funded from account 9.
        for (i, at) in [2u64, 3, 4].into_iter().enumerate() {
            push(
                &mut state,
                Command::PostBatch(Batch {
                    idempotency_key: ClientKey(i as u128 + 1),
                    value_time: ValueTime(at as i64), // spread across distinct value_times
                    postings: vec![
                        Posting {
                            key: key(9),
                            side: Side::Debit,
                            amount: 1000,
                        },
                        Posting {
                            key: key(1),
                            side: Side::Credit,
                            amount: 1000,
                        },
                    ],
                }),
                at,
            );
        }

        // A large debit pushes account 1 negative.
        push(
            &mut state,
            Command::PostBatch(Batch {
                idempotency_key: ClientKey(100),
                value_time: ValueTime(5),
                postings: vec![
                    Posting {
                        key: key(1),
                        side: Side::Debit,
                        amount: 5000,
                    },
                    Posting {
                        key: key(9),
                        side: Side::Credit,
                        amount: 5000,
                    },
                ],
            }),
            5,
        );

        // A recovery credit brings it back positive.
        push(
            &mut state,
            Command::PostBatch(Batch {
                idempotency_key: ClientKey(101),
                value_time: ValueTime(6),
                postings: vec![
                    Posting {
                        key: key(9),
                        side: Side::Debit,
                        amount: 3000,
                    },
                    Posting {
                        key: key(1),
                        side: Side::Credit,
                        amount: 3000,
                    },
                ],
            }),
            6,
        );

        store.sync().unwrap();
    }

    #[test]
    fn computes_real_signals_from_ledger_history() {
        let dir = temp_dir("signals");
        seed_test_ledger(&dir);

        let signals = compute_internal_signals(&dir, 1).unwrap();
        std::fs::remove_dir_all(&dir).ok();

        // Balance: +1000*3 - 5000 + 3000 = 1000.
        assert_eq!(signals.current_balance_net, 1000);
        assert_eq!(signals.total_credits, 1000 + 1000 + 1000 + 3000);
        assert_eq!(signals.total_debits, 5000);
        assert_eq!(signals.posting_count, 5);
        assert!(!signals.currently_negative);
        assert_eq!(signals.times_went_negative, 1);
        // Balance went 1000 -> 2000 -> 3000 -> -2000 -> 1000.
        assert_eq!(signals.lowest_balance_net, -2000);
        assert_eq!(
            signals.detected_recurring_credit,
            Some(RecurringCredit {
                amount: 1000,
                occurrences: 3
            })
        );
        // value_times used: 2,3,4,5,6 = 5 distinct.
        assert_eq!(signals.distinct_value_times, 5);
    }

    #[test]
    fn ignores_postings_on_other_accounts() {
        let dir = temp_dir("isolation");
        seed_test_ledger(&dir);

        let signals = compute_internal_signals(&dir, 9).unwrap();
        std::fs::remove_dir_all(&dir).ok();

        // Account 9 funded everything: net should be the mirror image.
        assert_eq!(signals.current_balance_net, -1000);
        assert_eq!(signals.account_id, 9);
    }

    #[test]
    fn external_signals_are_always_labelled_synthetic() {
        let path = std::env::temp_dir().join("ledger_credit_ext_test.json");
        std::fs::write(
            &path,
            r#"{"accounts":[{"account_id":1,"mock_bureau_score":700,"mock_external_dti":0.2}]}"#,
        )
        .unwrap();

        let found = load_external_signals(Some(path.to_str().unwrap()), 1).unwrap();
        assert!(found.synthetic);
        assert_eq!(found.mock_bureau_score, 700);

        let missing = load_external_signals(Some(path.to_str().unwrap()), 999).unwrap();
        assert!(
            missing.synthetic,
            "even the default/unconfigured case must be labelled synthetic"
        );
        assert_eq!(missing.mock_bureau_score, DEFAULT_BUREAU_SCORE);

        std::fs::remove_file(&path).ok();
    }

    #[test]
    fn falls_back_to_the_embedded_default_when_no_path_is_given() {
        // No file on disk at all -- this must still work, using the
        // compiled-in JSON, which is the entire point of embedding it.
        let signals = load_external_signals(None, 1).unwrap();
        assert!(signals.synthetic);
        assert_eq!(signals.mock_bureau_score, 580); // matches default-external-data.json's account 1
    }

    #[test]
    fn falls_back_to_the_embedded_default_when_the_given_path_does_not_exist() {
        // A stale EXTERNAL_DATA_PATH pointing at a since-deleted file
        // (e.g. something that lived in /tmp and got wiped) must degrade
        // to the embedded default rather than erroring outright.
        let signals =
            load_external_signals(Some("/nonexistent/path/does-not-exist.json"), 2).unwrap();
        assert!(signals.synthetic);
        assert_eq!(signals.mock_bureau_score, 720); // matches default-external-data.json's account 2
    }

    /// The defining test for the lookback average: two credits at known
    /// offsets from today (60 and 30 days ago), hand-computed expected
    /// result. With today = T, window = [T-90, T]:
    ///   T-90 to T-60 (30 days): balance 0 (before any posting)
    ///   T-60 to T-30 (30 days): balance 1000 (after the first credit)
    ///   T-30 to T    (30 days): balance 1500 (after the second credit)
    /// weighted sum = 0*30 + 1000*30 + 1500*30 = 75,000; average = 75,000/90 = 833.33...
    #[test]
    fn lookback_average_matches_hand_computed_value() {
        let dir = temp_dir("lookback_normal_order");
        let mut state = State::new();
        let mut store = SegmentedFileStorage::open(&dir).unwrap();
        let mut push = |state: &mut State, cmd: Command, at: u64| {
            let (next, _) = apply(state, &cmd, LogTime(at)).unwrap();
            *state = next;
            let payload = encode_payload(&Record::v1(LogTime(at), cmd)).unwrap();
            store.append(&payload).unwrap();
        };

        let today = ValueTime::today().0;
        push(
            &mut state,
            Command::OpenAccount {
                account: AccountId(1),
            },
            0,
        );
        push(
            &mut state,
            Command::OpenAccount {
                account: AccountId(9),
            },
            1,
        );
        push(
            &mut state,
            Command::PostBatch(Batch {
                idempotency_key: ClientKey(1),
                value_time: ValueTime(today - 60),
                postings: vec![
                    Posting {
                        key: key(9),
                        side: Side::Debit,
                        amount: 1000,
                    },
                    Posting {
                        key: key(1),
                        side: Side::Credit,
                        amount: 1000,
                    },
                ],
            }),
            2,
        );
        push(
            &mut state,
            Command::PostBatch(Batch {
                idempotency_key: ClientKey(2),
                value_time: ValueTime(today - 30),
                postings: vec![
                    Posting {
                        key: key(9),
                        side: Side::Debit,
                        amount: 500,
                    },
                    Posting {
                        key: key(1),
                        side: Side::Credit,
                        amount: 500,
                    },
                ],
            }),
            3,
        );
        store.sync().unwrap();

        let signals = compute_internal_signals(&dir, 1).unwrap();
        std::fs::remove_dir_all(&dir).ok();

        assert_eq!(signals.lookback_days, Some(90));
        assert_close(
            signals.average_balance_lookback.unwrap(),
            833.33,
            "average_balance_lookback",
        );
    }

    /// Same scenario as above, but the two batches are LOGGED in the
    /// opposite order from their value_times (the T-30 batch is applied
    /// first, the T-60 "backdated" batch second) -- simulating exactly
    /// the case design section 6 describes: a correction recorded after
    /// postings with a later economic date. The result must be identical
    /// to the normal-order test, proving the sort-by-value_time step is
    /// actually doing the work, not just coincidentally matching log
    /// order because nothing has tested the mismatch case yet.
    #[test]
    fn lookback_average_is_correct_even_when_logged_out_of_value_time_order() {
        let dir = temp_dir("lookback_backdated");
        let mut state = State::new();
        let mut store = SegmentedFileStorage::open(&dir).unwrap();
        let mut push = |state: &mut State, cmd: Command, at: u64| {
            let (next, _) = apply(state, &cmd, LogTime(at)).unwrap();
            *state = next;
            let payload = encode_payload(&Record::v1(LogTime(at), cmd)).unwrap();
            store.append(&payload).unwrap();
        };

        let today = ValueTime::today().0;
        push(
            &mut state,
            Command::OpenAccount {
                account: AccountId(1),
            },
            0,
        );
        push(
            &mut state,
            Command::OpenAccount {
                account: AccountId(9),
            },
            1,
        );

        // Logged FIRST: value_time = today - 30 (the economically LATER event).
        push(
            &mut state,
            Command::PostBatch(Batch {
                idempotency_key: ClientKey(1),
                value_time: ValueTime(today - 30),
                postings: vec![
                    Posting {
                        key: key(9),
                        side: Side::Debit,
                        amount: 500,
                    },
                    Posting {
                        key: key(1),
                        side: Side::Credit,
                        amount: 500,
                    },
                ],
            }),
            2,
        );
        // Logged SECOND: value_time = today - 60 (the economically EARLIER
        // event) -- a backdated correction arriving after the fact.
        push(
            &mut state,
            Command::PostBatch(Batch {
                idempotency_key: ClientKey(2),
                value_time: ValueTime(today - 60),
                postings: vec![
                    Posting {
                        key: key(9),
                        side: Side::Debit,
                        amount: 1000,
                    },
                    Posting {
                        key: key(1),
                        side: Side::Credit,
                        amount: 1000,
                    },
                ],
            }),
            3,
        );
        store.sync().unwrap();

        let signals = compute_internal_signals(&dir, 1).unwrap();
        std::fs::remove_dir_all(&dir).ok();

        // Current balance (log-order-agnostic net) is unaffected either way.
        assert_eq!(signals.current_balance_net, 1500);
        // But the lookback average must match the SAME economically-sorted
        // timeline as the normal-order test, not the log order.
        assert_close(
            signals.average_balance_lookback.unwrap(),
            833.33,
            "average_balance_lookback under backdated log order",
        );
    }

    #[test]
    fn lookback_average_is_none_without_real_date_diversity() {
        let dir = temp_dir("no_diversity");
        let mut state = State::new();
        let mut store = SegmentedFileStorage::open(&dir).unwrap();
        let mut push = |state: &mut State, cmd: Command, at: u64| {
            let (next, _) = apply(state, &cmd, LogTime(at)).unwrap();
            *state = next;
            let payload = encode_payload(&Record::v1(LogTime(at), cmd)).unwrap();
            store.append(&payload).unwrap();
        };

        push(
            &mut state,
            Command::OpenAccount {
                account: AccountId(1),
            },
            0,
        );
        push(
            &mut state,
            Command::OpenAccount {
                account: AccountId(9),
            },
            1,
        );
        // Both postings share the same value_time -- exactly the
        // ValueTime(0)-everywhere situation design section 3.3 describes.
        push(
            &mut state,
            Command::PostBatch(Batch {
                idempotency_key: ClientKey(1),
                value_time: ValueTime(0),
                postings: vec![
                    Posting {
                        key: key(9),
                        side: Side::Debit,
                        amount: 500,
                    },
                    Posting {
                        key: key(1),
                        side: Side::Credit,
                        amount: 500,
                    },
                ],
            }),
            2,
        );
        store.sync().unwrap();

        let signals = compute_internal_signals(&dir, 1).unwrap();
        std::fs::remove_dir_all(&dir).ok();

        assert_eq!(signals.distinct_value_times, 1);
        assert_eq!(
            signals.average_balance_lookback, None,
            "must not fabricate an average from single-instant data"
        );
        assert_eq!(signals.lookback_days, None);
    }
}
