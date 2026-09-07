//! Read-side journal lookups over a raw record slice.
//!
//! These are deliberately *not* methods on `State`: `State` only keeps a
//! bounded, evictable window of recent idempotency keys (see
//! `State::evict_dedup_before`), so a real "show me every posting that
//! ever touched this account" or "find the batch behind this idempotency
//! key" query has to go against the log itself, not the projection. For
//! the data sizes a read service deals with at this stage, a linear scan
//! over `&[Record]` is the right amount of machinery -- an indexed read
//! path is `ledger-core`'s M5, built only once this scan is a measured
//! bottleneck, not before.

use crate::record::Record;
use ledger_types::ids::{AccountId, ClientKey};

/// Every record whose command touches `account`, in log order. A
/// `PostBatch` matches if any of its postings reference the account;
/// `OpenAccount`/`CloseAccount` match if they name it; `Tick` never
/// matches.
pub fn entries_for_account(records: &[Record], account: AccountId) -> Vec<&Record> {
    records
        .iter()
        .filter(|record| command_touches_account(record, account))
        .collect()
}

/// The single record whose `PostBatch` carries `key`, if any. Used to
/// answer "what actually happened under this idempotency key" -- the
/// forensic lookup `State`'s bounded dedup index can't answer once an
/// entry has aged out of it.
pub fn entry_for_idempotency_key(records: &[Record], key: ClientKey) -> Option<&Record> {
    records.iter().find(|record| batch_key_matches(record, key))
}

fn command_touches_account(record: &Record, account: AccountId) -> bool {
    use ledger_core::Command;
    match record.command() {
        Command::OpenAccount { account: a } | Command::CloseAccount { account: a } => *a == account,
        Command::PostBatch(batch) => batch.postings.iter().any(|p| p.key.account == account),
        Command::Tick => false,
    }
}

fn batch_key_matches(record: &Record, key: ClientKey) -> bool {
    use ledger_core::Command;
    matches!(record.command(), Command::PostBatch(batch) if batch.idempotency_key == key)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::record::Record;
    use ledger_core::Command;
    use ledger_types::balance::{BalanceKey, Phase};
    use ledger_types::ids::{AddressId, DenomId};
    use ledger_types::posting::{Batch, Posting, Side};
    use ledger_types::time::{LogTime, ValueTime};

    fn balance_key(account: u64) -> BalanceKey {
        BalanceKey {
            account: AccountId(account),
            address: AddressId::DEFAULT,
            denom: DenomId(1),
            phase: Phase::Committed,
        }
    }

    fn sample_records() -> Vec<Record> {
        vec![
            Record::v1(
                LogTime(0),
                Command::OpenAccount {
                    account: AccountId(1),
                },
            ),
            Record::v1(
                LogTime(1),
                Command::OpenAccount {
                    account: AccountId(2),
                },
            ),
            Record::v1(
                LogTime(2),
                Command::PostBatch(Batch {
                    idempotency_key: ClientKey(42),
                    value_time: ValueTime(0),
                    postings: vec![
                        Posting {
                            key: balance_key(1),
                            side: Side::Debit,
                            amount: 100,
                        },
                        Posting {
                            key: balance_key(2),
                            side: Side::Credit,
                            amount: 100,
                        },
                    ],
                }),
            ),
            Record::v1(LogTime(3), Command::Tick),
        ]
    }

    #[test]
    fn entries_for_account_finds_opens_and_postings_but_not_other_accounts() {
        let records = sample_records();
        let entries = entries_for_account(&records, AccountId(1));
        // OpenAccount(1) and the PostBatch that touches account 1: two entries.
        assert_eq!(entries.len(), 2);
        assert_eq!(entries[0].at(), LogTime(0));
        assert_eq!(entries[1].at(), LogTime(2));
    }

    #[test]
    fn entries_for_account_is_empty_for_an_account_never_mentioned() {
        let records = sample_records();
        assert!(entries_for_account(&records, AccountId(999)).is_empty());
    }

    #[test]
    fn entry_for_idempotency_key_finds_the_matching_batch() {
        let records = sample_records();
        let found = entry_for_idempotency_key(&records, ClientKey(42)).unwrap();
        assert_eq!(found.at(), LogTime(2));
    }

    #[test]
    fn entry_for_idempotency_key_returns_none_when_absent() {
        let records = sample_records();
        assert!(entry_for_idempotency_key(&records, ClientKey(999)).is_none());
    }
}
