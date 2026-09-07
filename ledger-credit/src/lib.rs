//! `ledger-credit`: credit decisioning. See `04-credit-decisioning-design.md`
//! at the workspace root for the full design and reasoning.
//!
//! Three plain modules, each independently tested against real data
//! (`signals`) or hand-constructed cases (`scoring`, `audit`) -- the
//! binary (`main.rs`) is a thin Axum layer over these, deliberately not
//! an MCP server (see design doc section 5 for why).

pub mod audit;
pub mod scoring;
pub mod signals;
