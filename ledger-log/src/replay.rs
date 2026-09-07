//! Replay: rebuild a `State` by folding `apply` over a record slice,
//! starting from genesis.
//!
//! This function, together with the property test in
//! `tests/replay_determinism.rs` that checks it against a freshly-applied
//! state byte-for-byte, is the defining test from the design: it is the
//! single piece of evidence that justifies the log/state-machine split.

use ledger_core::{apply, Rejection, State};

use crate::record::Record;

/// Replay `records` from the genesis state, in order.
///
/// A record that `apply` rejects fails the whole replay, returning the
/// index of the offending record alongside the rejection. A log is
/// expected to contain only records that were actually accepted when
/// first written, so a rejection here means the log (or the state machine
/// version applied to it) has drifted -- it is not treated as "skip this
/// one and continue".
pub fn replay(records: &[Record]) -> Result<State, (usize, Rejection)> {
    let mut state = State::new();
    for (i, record) in records.iter().enumerate() {
        match apply(&state, record.command(), record.at()) {
            Ok((next, _effects)) => state = next,
            Err(rejection) => return Err((i, rejection)),
        }
    }
    Ok(state)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::record::Record;
    use ledger_core::Command;
    use ledger_types::balance::{BalanceKey, Phase};
    use ledger_types::ids::{AccountId, AddressId, ClientKey, DenomId};
    use ledger_types::posting::{Batch, Posting, Side};
    use ledger_types::time::{LogTime, ValueTime};

    #[test]
    fn replay_of_empty_log_is_genesis_state() {
        assert_eq!(replay(&[]).unwrap(), State::new());
    }

    #[test]
    fn replay_reproduces_live_application() {
        let mut state = State::new();
        let mut records = Vec::new();

        let push = |state: &mut State, records: &mut Vec<Record>, at: u64, cmd: Command| {
            let (next, _) = apply(state, &cmd, LogTime(at)).unwrap();
            *state = next;
            records.push(Record::v1(LogTime(at), cmd));
        };

        push(&mut state, &mut records, 0, Command::OpenAccount { account: AccountId(1) });
        push(&mut state, &mut records, 1, Command::OpenAccount { account: AccountId(2) });
        push(
            &mut state,
            &mut records,
            2,
            Command::PostBatch(Batch {
                idempotency_key: ClientKey(1),
                value_time: ValueTime(0),
                postings: vec![
                    Posting {
                        key: BalanceKey { account: AccountId(1), address: AddressId::DEFAULT, denom: DenomId(1), phase: Phase::Committed },
                        side: Side::Debit,
                        amount: 250,
                    },
                    Posting {
                        key: BalanceKey { account: AccountId(2), address: AddressId::DEFAULT, denom: DenomId(1), phase: Phase::Committed },
                        side: Side::Credit,
                        amount: 250,
                    },
                ],
            }),
        );

        let replayed = replay(&records).unwrap();
        assert_eq!(state, replayed);
    }

    #[test]
    fn a_rejected_record_fails_replay_at_its_index() {
        let records = vec![
            Record::v1(LogTime(0), Command::OpenAccount { account: AccountId(1) }),
            // Duplicate open: was never actually accepted live, so a log
            // containing it is corrupt.
            Record::v1(LogTime(1), Command::OpenAccount { account: AccountId(1) }),
        ];
        let err = replay(&records).unwrap_err();
        assert_eq!(err, (1, Rejection::AccountAlreadyExists));
    }
}
