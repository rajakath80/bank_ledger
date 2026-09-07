//! Effects produced by a successful `apply`. Informational only -- for
//! logging and downstream consumers (the read path, later milestones) --
//! and never fed back into validation.

use ledger_types::ids::{AccountId, ClientKey};
use ledger_types::time::LogTime;
use serde::{Deserialize, Serialize};

#[derive(Clone, PartialEq, Eq, Debug, Serialize, Deserialize)]
pub enum Effect {
    AccountOpened(AccountId),
    AccountClosed(AccountId),
    BatchApplied {
        idempotency_key: ClientKey,
        postings_applied: usize,
    },
    Ticked(LogTime),
}
