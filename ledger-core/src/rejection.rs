//! Rejection codes returned by `apply`.
//!
//! Numbered explicitly and treated as a permanent public API: clients will
//! branch on these numbers, so once one is shipped it is never renumbered
//! or reused for a different meaning, even if the rejection it names is
//! later retired.

use serde::{Deserialize, Serialize};

#[derive(Copy, Clone, PartialEq, Eq, Debug, Serialize, Deserialize)]
#[repr(u32)]
pub enum Rejection {
    /// `OpenAccount` for an account id that already exists (open or closed).
    AccountAlreadyExists = 1,
    /// A batch posted to an account id that has never been opened, or that
    /// exists but is currently closed.
    AccountNotFound = 2,
    /// `CloseAccount` on an account that has at least one non-zero balance
    /// address.
    AccountNotEmpty = 3,
    /// A batch's postings do not net to zero for some denomination.
    UnbalancedBatch = 4,
    /// A batch with no postings.
    EmptyBatch = 5,
    /// The same idempotency key was seen before with a different batch
    /// body. Not a retry -- a conflicting request.
    DuplicateIdempotencyKeyConflict = 6,
    /// `CloseAccount` on an account id that has never been opened.
    CloseUnknownAccount = 7,
}

impl Rejection {
    pub fn code(self) -> u32 {
        self as u32
    }
}
