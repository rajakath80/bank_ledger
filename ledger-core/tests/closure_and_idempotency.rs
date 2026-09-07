//! `M1` "done when" checks that don't fit naturally as unit tests:
//! closure-with-nonzero-balance rejection, idempotent replay, and the
//! surrounding batch-validation rejections.

use ledger_core::{Command, Rejection, State, apply};
use ledger_types::balance::{BalanceKey, Phase};
use ledger_types::ids::{AccountId, AddressId, ClientKey, DenomId};
use ledger_types::posting::{Batch, Posting, Side};
use ledger_types::time::{LogTime, ValueTime};

fn key(account: u64) -> BalanceKey {
    BalanceKey {
        account: AccountId(account),
        address: AddressId::DEFAULT,
        denom: DenomId(1),
        phase: Phase::Committed,
    }
}

fn transfer(idempotency_key: u128, from: u64, to: u64, amount: u128) -> Command {
    Command::PostBatch(Batch {
        idempotency_key: ClientKey(idempotency_key),
        value_time: ValueTime(0),
        postings: vec![
            Posting {
                key: key(from),
                side: Side::Debit,
                amount,
            },
            Posting {
                key: key(to),
                side: Side::Credit,
                amount,
            },
        ],
    })
}

#[test]
fn closure_with_nonzero_balance_is_rejected() {
    let state = State::new();
    let (state, _) = apply(
        &state,
        &Command::OpenAccount {
            account: AccountId(1),
        },
        LogTime(0),
    )
    .unwrap();
    let (state, _) = apply(
        &state,
        &Command::OpenAccount {
            account: AccountId(2),
        },
        LogTime(1),
    )
    .unwrap();
    let (state, _) = apply(&state, &transfer(1, 1, 2, 500), LogTime(2)).unwrap();

    let err = apply(
        &state,
        &Command::CloseAccount {
            account: AccountId(1),
        },
        LogTime(3),
    )
    .unwrap_err();
    assert_eq!(err, Rejection::AccountNotEmpty);

    // Move the money back out; account 1 is zero again and can now close.
    let (state, _) = apply(&state, &transfer(2, 2, 1, 500), LogTime(4)).unwrap();
    let result = apply(
        &state,
        &Command::CloseAccount {
            account: AccountId(1),
        },
        LogTime(5),
    );
    assert!(result.is_ok());
}

#[test]
fn closing_an_unknown_account_is_rejected() {
    let state = State::new();
    let err = apply(
        &state,
        &Command::CloseAccount {
            account: AccountId(999),
        },
        LogTime(0),
    )
    .unwrap_err();
    assert_eq!(err, Rejection::CloseUnknownAccount);
}

#[test]
fn closing_an_already_closed_account_is_a_stable_no_op() {
    let state = State::new();
    let (state, _) = apply(
        &state,
        &Command::OpenAccount {
            account: AccountId(1),
        },
        LogTime(0),
    )
    .unwrap();
    let (state, _) = apply(
        &state,
        &Command::CloseAccount {
            account: AccountId(1),
        },
        LogTime(1),
    )
    .unwrap();
    let (state_again, effects) = apply(
        &state,
        &Command::CloseAccount {
            account: AccountId(1),
        },
        LogTime(2),
    )
    .unwrap();
    assert_eq!(state, state_again);
    assert!(effects.is_empty());
}

#[test]
fn replayed_idempotency_key_returns_original_outcome() {
    let state = State::new();
    let (state, _) = apply(
        &state,
        &Command::OpenAccount {
            account: AccountId(1),
        },
        LogTime(0),
    )
    .unwrap();
    let (state, _) = apply(
        &state,
        &Command::OpenAccount {
            account: AccountId(2),
        },
        LogTime(1),
    )
    .unwrap();

    let cmd = transfer(42, 1, 2, 100);
    let (state_after_first, effects_first) = apply(&state, &cmd, LogTime(2)).unwrap();
    let (state_after_retry, effects_retry) = apply(&state_after_first, &cmd, LogTime(3)).unwrap();

    assert_eq!(effects_first, effects_retry);
    // Retrying must not move the balance a second time.
    assert_eq!(state_after_first, state_after_retry);
}

#[test]
fn replayed_idempotency_key_returns_original_rejection_without_revalidating() {
    let state = State::new();
    let (state, _) = apply(
        &state,
        &Command::OpenAccount {
            account: AccountId(1),
        },
        LogTime(0),
    )
    .unwrap();

    // Unbalanced: rejected on first attempt.
    let bad = Command::PostBatch(Batch {
        idempotency_key: ClientKey(1),
        value_time: ValueTime(0),
        postings: vec![Posting {
            key: key(1),
            side: Side::Debit,
            amount: 100,
        }],
    });
    let first_err = apply(&state, &bad, LogTime(1)).unwrap_err();
    assert_eq!(first_err, Rejection::UnbalancedBatch);

    let (state_after_first, _) = match apply(&state, &bad, LogTime(1)) {
        Err(_) => (state.clone(), ()), // rejection: state passed through unchanged by caller
        Ok(_) => unreachable!(),
    };
    let second_err = apply(&state_after_first, &bad, LogTime(2)).unwrap_err();
    assert_eq!(second_err, Rejection::UnbalancedBatch);
}

#[test]
fn same_key_different_body_is_hard_rejection() {
    let state = State::new();
    let (state, _) = apply(
        &state,
        &Command::OpenAccount {
            account: AccountId(1),
        },
        LogTime(0),
    )
    .unwrap();
    let (state, _) = apply(
        &state,
        &Command::OpenAccount {
            account: AccountId(2),
        },
        LogTime(1),
    )
    .unwrap();

    let (state, _) = apply(&state, &transfer(7, 1, 2, 100), LogTime(2)).unwrap();
    let err = apply(&state, &transfer(7, 1, 2, 999), LogTime(3)).unwrap_err();
    assert_eq!(err, Rejection::DuplicateIdempotencyKeyConflict);
}

#[test]
fn unbalanced_batch_is_rejected() {
    let state = State::new();
    let (state, _) = apply(
        &state,
        &Command::OpenAccount {
            account: AccountId(1),
        },
        LogTime(0),
    )
    .unwrap();

    let bad = Command::PostBatch(Batch {
        idempotency_key: ClientKey(1),
        value_time: ValueTime(0),
        postings: vec![Posting {
            key: key(1),
            side: Side::Debit,
            amount: 100,
        }],
    });
    let err = apply(&state, &bad, LogTime(1)).unwrap_err();
    assert_eq!(err, Rejection::UnbalancedBatch);
}

#[test]
fn empty_batch_is_rejected() {
    let state = State::new();
    let empty = Command::PostBatch(Batch {
        idempotency_key: ClientKey(1),
        value_time: ValueTime(0),
        postings: vec![],
    });
    let err = apply(&state, &empty, LogTime(0)).unwrap_err();
    assert_eq!(err, Rejection::EmptyBatch);
}

#[test]
fn posting_against_unknown_account_is_rejected() {
    let state = State::new();
    let (state, _) = apply(
        &state,
        &Command::OpenAccount {
            account: AccountId(1),
        },
        LogTime(0),
    )
    .unwrap();
    let err = apply(&state, &transfer(1, 1, 999, 50), LogTime(1)).unwrap_err();
    assert_eq!(err, Rejection::AccountNotFound);
}

#[test]
fn posting_against_a_closed_account_is_rejected() {
    let state = State::new();
    let (state, _) = apply(
        &state,
        &Command::OpenAccount {
            account: AccountId(1),
        },
        LogTime(0),
    )
    .unwrap();
    let (state, _) = apply(
        &state,
        &Command::OpenAccount {
            account: AccountId(2),
        },
        LogTime(1),
    )
    .unwrap();
    let (state, _) = apply(
        &state,
        &Command::CloseAccount {
            account: AccountId(1),
        },
        LogTime(2),
    )
    .unwrap();
    let err = apply(&state, &transfer(1, 1, 2, 10), LogTime(3)).unwrap_err();
    assert_eq!(err, Rejection::AccountNotFound);
}

#[test]
fn opening_an_account_twice_is_rejected() {
    let state = State::new();
    let (state, _) = apply(
        &state,
        &Command::OpenAccount {
            account: AccountId(1),
        },
        LogTime(0),
    )
    .unwrap();
    let err = apply(
        &state,
        &Command::OpenAccount {
            account: AccountId(1),
        },
        LogTime(1),
    )
    .unwrap_err();
    assert_eq!(err, Rejection::AccountAlreadyExists);
}
