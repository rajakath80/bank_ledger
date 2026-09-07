//! Postings and batches: the unit of change accepted by the state machine.

use crate::balance::BalanceKey;
use crate::ids::ClientKey;
use crate::time::ValueTime;
use serde::{Deserialize, Serialize};

/// Which leg of a posting an amount belongs to. Amounts themselves are
/// unsigned (`u128`); direction lives entirely in this enum, which
/// eliminates the "negative credit" class of bug at the type level.
#[derive(Copy, Clone, PartialEq, Eq, Hash, Debug, Serialize, Deserialize)]
pub enum Side {
    Debit,
    Credit,
}

/// A single leg of a batch: apply `amount` to the `side` of the balance at
/// `key`.
#[derive(Copy, Clone, PartialEq, Eq, Debug, Serialize, Deserialize)]
pub struct Posting {
    pub key: BalanceKey,
    pub side: Side,
    pub amount: u128,
}

/// An atomic group of postings submitted together under one idempotency
/// key. The state machine requires every batch to net to zero per
/// denomination before any posting in it is applied (see
/// `ledger_core::apply`) -- a batch is all-or-nothing, and it is only ever
/// balanced or entirely rejected.
#[derive(Clone, PartialEq, Eq, Debug, Serialize, Deserialize)]
pub struct Batch {
    pub idempotency_key: ClientKey,
    pub value_time: ValueTime,
    pub postings: Vec<Posting>,
}
