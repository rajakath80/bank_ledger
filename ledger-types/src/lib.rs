//! `ledger-types`: money, identifiers and time.
//!
//! This crate contains no logic and no `unsafe` code (`#![forbid(unsafe_code)]`).
//! It exists so that `ledger-core` and every downstream crate share one
//! definition of what an account, an address, a denomination, an amount and
//! a moment in time are. Nothing here performs IO, allocates a clock, or
//! makes a decision -- it only defines shapes and the arithmetic that is
//! safe to do on them.

#![forbid(unsafe_code)]

pub mod balance;
pub mod ids;
pub mod money;
pub mod posting;
pub mod time;
