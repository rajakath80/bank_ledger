//! Two-phase transfers need no dedicated commands: they fall out of
//! posting to different `Phase`s within the zero-sum-per-batch rule that
//! `apply` already enforces. Authorisation moves value to `PendingOut`;
//! settlement moves it on to `Committed` somewhere else; release moves it
//! back to `Committed` on the original account.

use ledger_core::{Command, State, apply};
use ledger_types::balance::{BalanceKey, Phase};
use ledger_types::ids::{AccountId, AddressId, ClientKey, DenomId};
use ledger_types::posting::{Batch, Posting, Side};
use ledger_types::time::{LogTime, ValueTime};

fn key(account: u64, phase: Phase) -> BalanceKey {
    BalanceKey {
        account: AccountId(account),
        address: AddressId::DEFAULT,
        denom: DenomId(1),
        phase,
    }
}

fn open_and_fund(state: State) -> State {
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
    let fund = Command::PostBatch(Batch {
        idempotency_key: ClientKey(1),
        value_time: ValueTime(0),
        postings: vec![
            Posting {
                key: key(2, Phase::Committed),
                side: Side::Debit,
                amount: 1000,
            },
            Posting {
                key: key(1, Phase::Committed),
                side: Side::Credit,
                amount: 1000,
            },
        ],
    });
    apply(&state, &fund, LogTime(2)).unwrap().0
}

#[test]
fn authorise_settle_release_flow() {
    let state = open_and_fund(State::new());

    // Authorise: move 300 from account 1's Committed to its PendingOut.
    // Zero-sum within the same account and denom.
    let authorise = Command::PostBatch(Batch {
        idempotency_key: ClientKey(2),
        value_time: ValueTime(0),
        postings: vec![
            Posting {
                key: key(1, Phase::Committed),
                side: Side::Debit,
                amount: 300,
            },
            Posting {
                key: key(1, Phase::PendingOut),
                side: Side::Credit,
                amount: 300,
            },
        ],
    });
    let (state, _) = apply(&state, &authorise, LogTime(3)).unwrap();

    assert_eq!(
        state
            .balances
            .get(&key(1, Phase::PendingOut))
            .unwrap()
            .net(),
        300
    );
    assert_eq!(
        state.balances.get(&key(1, Phase::Committed)).unwrap().net(),
        700
    );

    // Settle: the held amount leaves account 1 for good and lands in
    // account 2's Committed balance.
    let settle = Command::PostBatch(Batch {
        idempotency_key: ClientKey(3),
        value_time: ValueTime(0),
        postings: vec![
            Posting {
                key: key(1, Phase::PendingOut),
                side: Side::Debit,
                amount: 300,
            },
            Posting {
                key: key(2, Phase::Committed),
                side: Side::Credit,
                amount: 300,
            },
        ],
    });
    let (state, _) = apply(&state, &settle, LogTime(4)).unwrap();

    assert_eq!(
        state
            .balances
            .get(&key(1, Phase::PendingOut))
            .unwrap()
            .net(),
        0
    );
    // Account 2 started at -1000 (it was the funding source for account 1),
    // then received the settled 300.
    assert_eq!(
        state.balances.get(&key(2, Phase::Committed)).unwrap().net(),
        -700
    );
    assert_eq!(
        state.balances.get(&key(1, Phase::Committed)).unwrap().net(),
        700
    );
}

#[test]
fn authorise_then_release_returns_to_committed_unchanged() {
    let state = open_and_fund(State::new());

    let authorise = Command::PostBatch(Batch {
        idempotency_key: ClientKey(2),
        value_time: ValueTime(0),
        postings: vec![
            Posting {
                key: key(1, Phase::Committed),
                side: Side::Debit,
                amount: 400,
            },
            Posting {
                key: key(1, Phase::PendingOut),
                side: Side::Credit,
                amount: 400,
            },
        ],
    });
    let (state, _) = apply(&state, &authorise, LogTime(3)).unwrap();

    let release = Command::PostBatch(Batch {
        idempotency_key: ClientKey(3),
        value_time: ValueTime(0),
        postings: vec![
            Posting {
                key: key(1, Phase::PendingOut),
                side: Side::Debit,
                amount: 400,
            },
            Posting {
                key: key(1, Phase::Committed),
                side: Side::Credit,
                amount: 400,
            },
        ],
    });
    let (state, _) = apply(&state, &release, LogTime(4)).unwrap();

    assert_eq!(
        state
            .balances
            .get(&key(1, Phase::PendingOut))
            .unwrap()
            .net(),
        0
    );
    assert_eq!(
        state.balances.get(&key(1, Phase::Committed)).unwrap().net(),
        1000
    );
}
