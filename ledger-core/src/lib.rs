//! `ledger-core`: the state machine.
//!
//! Everything in this crate is a pure function of its arguments:
//! `apply(state, command, log_time)`. No IO, no clock, no randomness, no
//! async, no floats. This is enforced at the dependency-graph level, not
//! by discipline -- see `deny.toml` at the workspace root and the CI note
//! in the crate's `Cargo.toml`. `#![forbid(unsafe_code)]` holds here too.
//!
//! The ledger knows accounts, addressable balances, postings and
//! zero-sum. It does not know what interest, a mortgage, or a fee is --
//! that belongs to a contract host layered on top (a later milestone).

#![forbid(unsafe_code)]

pub mod apply;
pub mod command;
pub mod effect;
pub mod rejection;
pub mod state;

pub use apply::apply;
pub use command::Command;
pub use effect::Effect;
pub use rejection::Rejection;
pub use state::{AccountMeta, BatchOutcome, DedupEntry, State};
